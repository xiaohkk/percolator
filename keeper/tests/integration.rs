//! Keeper integration tests — drive the pure `strategy::decide` against
//! realistic slab states produced by the full instruction flow, then
//! (where relevant) execute the chosen action via `ProgramTest`.
//!
//! The four canonical scenarios from task #16:
//!   - liquidates_underwater_account
//!   - skips_healthy_account
//!   - bounty_threshold_respected
//!   - two_keepers_no_double_claim

use percolator_keeper::{decide, Action, SlabSnapshot, MIN_BOUNTY_THRESHOLD};
use percolator_program::{
    instruction::{
        BootstrapLpArgs, CreateSlabArgs, DepositArgs, InitializeEngineArgs, LiquidateArgs,
        PercolatorInstruction, PlaceOrderArgs, RiskParamsArgs,
    },
    state::{
        engine_region_size, find_vault_pda, slab_account_size, SlabHeader, ENGINE_OFFSET,
    },
};
use solana_program::{
    instruction::{AccountMeta, Instruction, InstructionError},
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_program,
    sysvar::clock,
};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::{Transaction, TransactionError},
};
use spl_token::state::{Account as TokenAccount, AccountState, Mint};

const TEST_MAX_ACCOUNTS: u64 = 64;
const ORACLE_PRICE: u64 = 1_000_000;
const STALE_SLOTS: u64 = 150;

fn valid_risk_params() -> RiskParamsArgs {
    RiskParamsArgs {
        maintenance_margin_bps: 500,
        initial_margin_bps: 1_000,
        trading_fee_bps: 0,
        max_accounts: TEST_MAX_ACCOUNTS,
        max_crank_staleness_slots: 1_000_000,
        liquidation_fee_bps: 100,
        liquidation_fee_cap: 1_000_000_000,
        min_liquidation_abs: 100,
        min_initial_deposit: 1_000,
        min_nonzero_mm_req: 100,
        min_nonzero_im_req: 200,
        insurance_floor: 0,
        h_min: 10,
        h_max: 1_000,
        resolve_price_deviation_bps: 500,
        max_accrual_dt_slots: 1_000_000,
        max_abs_funding_e9_per_slot: 100,
        min_funding_lifetime_slots: 1_000_000,
        max_active_positions_per_side: TEST_MAX_ACCOUNTS,
    }
}

fn packed_mint() -> Account {
    let mut data = vec![0u8; Mint::LEN];
    let m = Mint {
        mint_authority: COption::Some(Pubkey::new_unique()),
        supply: u64::MAX / 2,
        decimals: 6,
        is_initialized: true,
        freeze_authority: COption::None,
    };
    Mint::pack(m, &mut data).unwrap();
    Account {
        lamports: Rent::default().minimum_balance(Mint::LEN),
        data,
        owner: spl_token::ID,
        executable: false,
        rent_epoch: 0,
    }
}

fn packed_token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let mut data = vec![0u8; TokenAccount::LEN];
    let ta = TokenAccount {
        mint,
        owner,
        amount,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };
    TokenAccount::pack(ta, &mut data).unwrap();
    Account {
        lamports: Rent::default().minimum_balance(TokenAccount::LEN),
        data,
        owner: spl_token::ID,
        executable: false,
        rent_epoch: 0,
    }
}

fn packed_oracle(price: u64) -> Account {
    let mut data = vec![0u8; 8];
    data.copy_from_slice(&price.to_le_bytes());
    Account {
        lamports: Rent::default().minimum_balance(8),
        data,
        owner: system_program::ID,
        executable: false,
        rent_epoch: 0,
    }
}

fn program_test(program_id: Pubkey) -> ProgramTest {
    let mut pt = ProgramTest::new(
        "percolator_program",
        program_id,
        processor!(percolator_program::process_instruction),
    );
    pt.set_compute_max_units(10_000_000);
    pt
}

struct Harness {
    program_id: Pubkey,
    slab: Keypair,
    mint: Pubkey,
    vault_pda: Pubkey,
    vault_bump: u8,
    oracle: Pubkey,
    creator_ta: Pubkey,
    user: Keypair,
    user_ta: Pubkey,
    keeper_a: Keypair,
    keeper_a_ta: Pubkey,
    keeper_b: Keypair,
    keeper_b_ta: Pubkey,
}

impl Harness {
    fn new() -> (Self, ProgramTest) {
        let program_id = Pubkey::new_unique();
        let mut pt = program_test(program_id);
        let slab = Keypair::new();
        let mint = Pubkey::new_unique();
        let user = Keypair::new();
        let user_ta = Pubkey::new_unique();
        let creator_ta = Pubkey::new_unique();
        let oracle = Pubkey::new_unique();
        let keeper_a = Keypair::new();
        let keeper_a_ta = Pubkey::new_unique();
        let keeper_b = Keypair::new();
        let keeper_b_ta = Pubkey::new_unique();
        let (vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

        pt.add_account(mint, packed_mint());
        pt.add_account(creator_ta, packed_token_account(mint, Pubkey::default(), 1_000_000_000_000));
        pt.add_account(user_ta, packed_token_account(mint, user.pubkey(), 10_000_000));
        pt.add_account(vault_pda, packed_token_account(mint, vault_pda, 0));
        pt.add_account(keeper_a_ta, packed_token_account(mint, keeper_a.pubkey(), 0));
        pt.add_account(keeper_b_ta, packed_token_account(mint, keeper_b.pubkey(), 0));
        for pk in [user.pubkey(), keeper_a.pubkey(), keeper_b.pubkey()] {
            pt.add_account(
                pk,
                Account {
                    lamports: 10_000_000_000,
                    data: vec![],
                    owner: system_program::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            );
        }
        pt.add_account(oracle, packed_oracle(ORACLE_PRICE));

        (
            Self {
                program_id,
                slab,
                mint,
                vault_pda,
                vault_bump,
                oracle,
                creator_ta,
                user,
                user_ta,
                keeper_a,
                keeper_a_ta,
                keeper_b,
                keeper_b_ta,
            },
            pt,
        )
    }
}

// ---------- instruction builders ----------

fn build_create_slab_tx(
    h: &Harness,
    payer: &Keypair,
    blockhash: solana_sdk::hash::Hash,
) -> Transaction {
    let size = slab_account_size() as u64;
    let lamports = Rent::default().minimum_balance(size as usize);
    let alloc_ix = system_instruction::create_account(
        &payer.pubkey(),
        &h.slab.pubkey(),
        lamports,
        size,
        &h.program_id,
    );
    let ix_data = PercolatorInstruction::CreateSlab(CreateSlabArgs {
        bump: 0,
        vault_bump: h.vault_bump,
    })
    .pack();
    let create_ix = Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(h.mint, false),
            AccountMeta::new_readonly(h.oracle, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: ix_data,
    };
    let mut tx = Transaction::new_with_payer(&[alloc_ix, create_ix], Some(&payer.pubkey()));
    tx.sign(&[payer, &h.slab], blockhash);
    tx
}

fn build_initialize_engine_ix(h: &Harness, creator: Pubkey) -> Instruction {
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(creator, true),
        ],
        data: PercolatorInstruction::InitializeEngine(InitializeEngineArgs {
            risk_params: valid_risk_params(),
            init_oracle_price: ORACLE_PRICE,
        })
        .pack(),
    }
}

fn build_bootstrap_lp_ix(h: &Harness, creator: Pubkey, amount: u64) -> Instruction {
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(creator, true),
            AccountMeta::new(h.creator_ta, false),
            AccountMeta::new(h.vault_pda, false),
            AccountMeta::new_readonly(h.mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: PercolatorInstruction::BootstrapLp(BootstrapLpArgs { amount }).pack(),
    }
}

fn build_deposit_ix(h: &Harness, amount: u64) -> Instruction {
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(h.user.pubkey(), true),
            AccountMeta::new(h.user_ta, false),
            AccountMeta::new(h.vault_pda, false),
            AccountMeta::new_readonly(h.mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: PercolatorInstruction::Deposit(DepositArgs { amount }).pack(),
    }
}

fn build_place_order_ix(h: &Harness, side: u8, size: u64) -> Instruction {
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(h.user.pubkey(), true),
            AccountMeta::new_readonly(h.oracle, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
        data: PercolatorInstruction::PlaceOrder(PlaceOrderArgs {
            side,
            size,
            max_price: u64::MAX,
            min_price: 0,
        })
        .pack(),
    }
}

fn build_liquidate_ix(
    h: &Harness,
    keeper_pk: Pubkey,
    keeper_ta: Pubkey,
    victim_slot: u16,
) -> Instruction {
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(keeper_pk, true),
            AccountMeta::new(keeper_ta, false),
            AccountMeta::new_readonly(h.oracle, false),
            AccountMeta::new_readonly(clock::id(), false),
            AccountMeta::new(h.vault_pda, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data: PercolatorInstruction::Liquidate(LiquidateArgs { victim_slot }).pack(),
    }
}

async fn send(
    ctx: &mut ProgramTestContext,
    signer: &Keypair,
    ixs: &[Instruction],
) -> Result<(), BanksClientError> {
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(ixs, Some(&signer.pubkey()));
    tx.sign(&[signer], bh);
    ctx.banks_client.process_transaction(tx).await
}

async fn bring_up_with_long(
    h: &Harness,
    pt: ProgramTest,
    lp_amount: u64,
    user_deposit: u64,
    long_size: u64,
) -> ProgramTestContext {
    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();
    ctx.set_account(
        &h.creator_ta,
        &packed_token_account(h.mint, payer_pk, 1_000_000_000_000).into(),
    );

    ctx.banks_client
        .process_transaction(build_create_slab_tx(h, &ctx.payer, ctx.last_blockhash))
        .await
        .expect("create");

    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(
        &[build_initialize_engine_ix(h, payer_pk)],
        Some(&payer_pk),
    );
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await.expect("init");

    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(
        &[build_bootstrap_lp_ix(h, payer_pk, lp_amount)],
        Some(&payer_pk),
    );
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await.expect("bs");

    send(&mut ctx, &h.user, &[build_deposit_ix(h, user_deposit)])
        .await
        .expect("dep");
    send(&mut ctx, &h.user, &[build_place_order_ix(h, 0, long_size)])
        .await
        .expect("long");

    ctx
}

async fn fetch_snapshot(ctx: &mut ProgramTestContext, slab: Pubkey, oracle_price: u64) -> SlabSnapshot {
    let slab_acct = ctx.banks_client.get_account(slab).await.unwrap().unwrap();
    let engine_bytes = slab_acct.data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()].to_vec();
    let now = ctx.banks_client.get_root_slot().await.unwrap();
    SlabSnapshot {
        slab,
        now_slot: now,
        engine_bytes,
        oracle_price: Some(oracle_price),
        oracle_last_update_slot: now, // tests treat the oracle as fresh
        stale_slots: STALE_SLOTS,
        funding_staleness_slots: 10_000, // out of the way for these tests
    }
}

async fn set_oracle_price(ctx: &mut ProgramTestContext, oracle: Pubkey, price: u64) {
    ctx.set_account(&oracle, &packed_oracle(price).into());
}

// ---------- scenarios ----------

#[tokio::test]
async fn integration_skips_healthy_account() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up_with_long(&h, pt, 100_000_000, 1_000_000, 100_000).await;
    let snap = fetch_snapshot(&mut ctx, h.slab.pubkey(), ORACLE_PRICE).await;
    match decide(&snap) {
        Action::Liquidate { .. } => panic!("healthy account must not be flagged"),
        _ => {}
    }
}

#[tokio::test]
async fn integration_liquidates_underwater_account() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up_with_long(&h, pt, 1_000_000_000, 1_000_000, 9_000_000).await;

    // Crash the oracle to push the position underwater.
    let crashed_price = ORACLE_PRICE / 5;
    set_oracle_price(&mut ctx, h.oracle, crashed_price).await;

    let snap = fetch_snapshot(&mut ctx, h.slab.pubkey(), crashed_price).await;
    let action = decide(&snap);

    let (victim_slot, est) = match action {
        Action::Liquidate {
            victim_slot,
            estimated_bounty,
        } => (victim_slot, estimated_bounty),
        other => panic!("expected Liquidate, got {:?}", other),
    };

    // Execute the action.
    ctx.warp_to_slot(10).unwrap();
    send(
        &mut ctx,
        &h.keeper_a,
        &[build_liquidate_ix(
            &h,
            h.keeper_a.pubkey(),
            h.keeper_a_ta,
            victim_slot,
        )],
    )
    .await
    .expect("liq");

    // Sanity check: the estimated bounty was non-zero and the victim's
    // position is now cleared.
    assert!(est > 0);
    let slab_acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let engine: &percolator::RiskEngine = unsafe {
        &*(slab_acct.data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
    };
    assert_eq!(
        engine.accounts[victim_slot as usize].position_basis_q,
        0,
        "position cleared"
    );
}

#[tokio::test]
async fn integration_bounty_threshold_respected() {
    // Build a dust-sized underwater account. bounty = 0.5% * tiny capital
    // < MIN_BOUNTY_THRESHOLD, so decide() must skip. We can't open a
    // real position at dust capital (margin would fail), so deposit +
    // hand-poke an underwater state.
    let (h, pt) = Harness::new();
    // min_initial_deposit = 1_000 ⇒ deposit = 1_000. 50 bps = 5, way
    // below MIN_BOUNTY_THRESHOLD (1_000).
    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();
    ctx.set_account(
        &h.creator_ta,
        &packed_token_account(h.mint, payer_pk, 1_000_000_000_000).into(),
    );
    ctx.banks_client
        .process_transaction(build_create_slab_tx(&h, &ctx.payer, ctx.last_blockhash))
        .await
        .expect("create");
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(
        &[build_initialize_engine_ix(&h, payer_pk)],
        Some(&payer_pk),
    );
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await.expect("init");
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(
        &[build_bootstrap_lp_ix(&h, payer_pk, 1_000_000_000)],
        Some(&payer_pk),
    );
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await.expect("bs");
    send(&mut ctx, &h.user, &[build_deposit_ix(&h, 1_000)])
        .await
        .expect("dep dust");

    // Poke the user to have an underwater tiny position.
    let mut slab_acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let _victim_slot: u16;
    {
        let engine_bytes = &mut slab_acct.data[ENGINE_OFFSET..];
        let engine: &mut percolator::RiskEngine = unsafe {
            &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine)
        };
        let max = engine.params.max_accounts as usize;
        let mut idx = None;
        for i in 1..max {
            if engine.is_used(i) && engine.accounts[i].owner == h.user.pubkey().to_bytes() {
                idx = Some(i);
                break;
            }
        }
        let i = idx.expect("user slot");
        _victim_slot = i as u16;
        engine.accounts[i].position_basis_q = 1_000_000;
        engine.accounts[i].adl_a_basis = percolator::ADL_ONE;
        engine.oi_eff_long_q += 1_000_000;
        engine.oi_eff_short_q += 1_000_000;
        engine.accounts[i].pnl = -100_000_000;
    }
    ctx.set_account(&h.slab.pubkey(), &slab_acct.into());

    let snap = fetch_snapshot(&mut ctx, h.slab.pubkey(), ORACLE_PRICE).await;
    let action = decide(&snap);
    match action {
        Action::Liquidate { estimated_bounty, .. } => panic!(
            "sub-threshold bounty must not be submitted: bounty={} < MIN={}",
            estimated_bounty, MIN_BOUNTY_THRESHOLD
        ),
        _ => {}
    }
}

#[tokio::test]
async fn integration_two_keepers_no_double_claim() {
    // Keeper A liquidates first. Keeper B, on the same underwater slot,
    // sees a healthy account (position cleared) → decide() no longer
    // recommends Liquidate. If B blindly submits anyway, the engine
    // returns AccountHealthy — test that path too.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up_with_long(&h, pt, 1_000_000_000, 1_000_000, 9_000_000).await;
    let crashed = ORACLE_PRICE / 5;
    set_oracle_price(&mut ctx, h.oracle, crashed).await;

    let snap = fetch_snapshot(&mut ctx, h.slab.pubkey(), crashed).await;
    let victim_slot = match decide(&snap) {
        Action::Liquidate { victim_slot, .. } => victim_slot,
        other => panic!("expected Liquidate, got {:?}", other),
    };

    // Keeper A fires.
    ctx.warp_to_slot(10).unwrap();
    send(
        &mut ctx,
        &h.keeper_a,
        &[build_liquidate_ix(&h, h.keeper_a.pubkey(), h.keeper_a_ta, victim_slot)],
    )
    .await
    .expect("keeper A");

    // Keeper B re-fetches: decide() must now NOT recommend Liquidate.
    let snap2 = fetch_snapshot(&mut ctx, h.slab.pubkey(), crashed).await;
    match decide(&snap2) {
        Action::Liquidate { .. } => panic!("second keeper must not see Liquidate"),
        _ => {}
    }

    // If keeper B submits anyway (racing), the engine rejects with
    // `AccountHealthy` — single-winner guarantee at the program layer.
    ctx.warp_to_slot(11).unwrap();
    let err = send(
        &mut ctx,
        &h.keeper_b,
        &[build_liquidate_ix(&h, h.keeper_b.pubkey(), h.keeper_b_ta, victim_slot)],
    )
    .await
    .expect_err("keeper B double-claim must fail");
    assert!(matches!(
        err,
        BanksClientError::TransactionError(TransactionError::InstructionError(_, InstructionError::Custom(code)))
            if code == percolator_program::error::PercolatorError::AccountHealthy as u32
    ));
}

#[tokio::test]
async fn integration_refresh_oracle_when_stale() {
    // Decide() surfaces RefreshOracle before any other action when the
    // oracle's last_update_slot lags more than STALE_SLOTS.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up_with_long(&h, pt, 100_000_000, 1_000_000, 100_000).await;
    let mut snap = fetch_snapshot(&mut ctx, h.slab.pubkey(), ORACLE_PRICE).await;
    // Pretend oracle hasn't updated in a long time.
    snap.oracle_last_update_slot = 0;
    snap.now_slot = 500; // 500 > STALE_SLOTS=150
    assert_eq!(decide(&snap), Action::RefreshOracle);
}

#[tokio::test]
async fn integration_stale_funding_triggers_crank_funding() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up_with_long(&h, pt, 100_000_000, 1_000_000, 100_000).await;
    let mut snap = fetch_snapshot(&mut ctx, h.slab.pubkey(), ORACLE_PRICE).await;
    snap.now_slot = 1_000_000;
    snap.funding_staleness_slots = 100;
    // Oracle still fresh:
    snap.oracle_last_update_slot = snap.now_slot - 10;
    assert_eq!(decide(&snap), Action::CrankFunding);
}
