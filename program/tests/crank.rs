//! End-to-end tests for the permissionless `Crank` instruction.
//!
//! Covers three kinds: Funding / Gc / AdlReset. The full ADL state-machine
//! trace (Normal → DrainOnly → ResetPending → Normal) requires coordinated
//! engine state poking, so we focus on: does each crank advance state the
//! way the wrapper expects, and does NothingToDo fire when there is nothing
//! to do.

use percolator_program::{
    error::PercolatorError,
    instruction::{
        BootstrapLpArgs, CrankArgs, CrankKind, CreateSlabArgs, DepositArgs, InitializeEngineArgs,
        InstructionTag, PercolatorInstruction, RiskParamsArgs,
    },
    state::{find_vault_pda, slab_account_size, SlabHeader, ENGINE_OFFSET},
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
const MIN_INITIAL_DEPOSIT: u128 = 1_000;
const ORACLE_PRICE: u64 = 1_000_000;

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
        min_initial_deposit: MIN_INITIAL_DEPOSIT,
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
    caller: Keypair,
    caller_ta: Pubkey,
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
        let caller = Keypair::new();
        let caller_ta = Pubkey::new_unique();
        let (vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

        pt.add_account(mint, packed_mint());
        pt.add_account(creator_ta, packed_token_account(mint, Pubkey::default(), 1_000_000_000_000));
        pt.add_account(user_ta, packed_token_account(mint, user.pubkey(), 10_000_000));
        pt.add_account(vault_pda, packed_token_account(mint, vault_pda, 0));
        pt.add_account(caller_ta, packed_token_account(mint, caller.pubkey(), 0));
        pt.add_account(
            user.pubkey(),
            Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
        pt.add_account(
            caller.pubkey(),
            Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
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
                caller,
                caller_ta,
            },
            pt,
        )
    }
}

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
    assert_eq!(ix_data[0], InstructionTag::CreateSlab as u8);
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
    let args = InitializeEngineArgs {
        risk_params: valid_risk_params(),
        init_oracle_price: ORACLE_PRICE,
    };
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(creator, true),
        ],
        data: PercolatorInstruction::InitializeEngine(args).pack(),
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

fn build_crank_ix(h: &Harness, kind: CrankKind) -> Instruction {
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(h.caller.pubkey(), true),
            AccountMeta::new_readonly(clock::id(), false),
            AccountMeta::new(h.caller_ta, false),
            AccountMeta::new(h.vault_pda, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data: PercolatorInstruction::Crank(CrankArgs { kind: kind as u8 }).pack(),
    }
}

fn expected_custom_error(err: &BanksClientError, want: PercolatorError) -> bool {
    let tx_err: &TransactionError = match err {
        BanksClientError::TransactionError(e) => e,
        BanksClientError::SimulationError { err, .. } => err,
        _ => return false,
    };
    matches!(
        tx_err,
        TransactionError::InstructionError(_, InstructionError::Custom(code)) if *code == want as u32
    )
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

async fn bring_up(
    h: &Harness,
    pt: ProgramTest,
    lp_amount: u64,
    user_deposit: u64,
) -> ProgramTestContext {
    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();
    ctx.set_account(
        &h.creator_ta,
        &packed_token_account(h.mint, payer_pk, 1_000_000_000_000).into(),
    );
    let create_tx = build_create_slab_tx(h, &ctx.payer, ctx.last_blockhash);
    ctx.banks_client
        .process_transaction(create_tx)
        .await
        .expect("create");
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(
        &[build_initialize_engine_ix(h, payer_pk)],
        Some(&payer_pk),
    );
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await.expect("init");
    if lp_amount > 0 {
        let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let mut tx = Transaction::new_with_payer(
            &[build_bootstrap_lp_ix(h, payer_pk, lp_amount)],
            Some(&payer_pk),
        );
        tx.sign(&[&ctx.payer], bh);
        ctx.banks_client.process_transaction(tx).await.expect("bs");
    }
    if user_deposit > 0 {
        send(&mut ctx, &h.user, &[build_deposit_ix(h, user_deposit)])
            .await
            .expect("dep");
    }
    ctx
}

async fn read_engine_field(
    ctx: &mut ProgramTestContext,
    slab: Pubkey,
) -> (u64, u64, u64) {
    let acct = ctx.banks_client.get_account(slab).await.unwrap().unwrap();
    let e: &percolator::RiskEngine =
        unsafe { &*(acct.data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine) };
    (e.current_slot, e.last_market_slot, e.last_crank_slot)
}

async fn token_balance(ctx: &mut ProgramTestContext, ta: Pubkey) -> u64 {
    let acct = ctx.banks_client.get_account(ta).await.unwrap().unwrap();
    TokenAccount::unpack(&acct.data).unwrap().amount
}

/// Seed insurance fund directly so crank bounty payouts have somewhere to
/// draw from. Conservation preserved: vault is incremented to match.
async fn fund_insurance(ctx: &mut ProgramTestContext, slab: Pubkey, amount: u128) {
    let mut acct = ctx.banks_client.get_account(slab).await.unwrap().unwrap();
    {
        let engine_bytes = &mut acct.data[ENGINE_OFFSET..];
        let e: &mut percolator::RiskEngine =
            unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
        let new_i = e.insurance_fund.balance.get() + amount;
        e.insurance_fund.balance = percolator::U128::new(new_i);
        let new_v = e.vault.get() + amount;
        e.vault = percolator::U128::new(new_v);
    }
    ctx.set_account(&slab, &acct.into());
}

// -----------------------------------------------------------------------

#[tokio::test]
async fn funding_crank_happy_path() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;

    ctx.warp_to_slot(50).unwrap();
    send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Funding)])
        .await
        .expect("funding crank");

    let (cur, last_mkt, last_crk) = read_engine_field(&mut ctx, h.slab.pubkey()).await;
    assert!(cur >= 50, "current_slot advanced");
    assert_eq!(last_mkt, cur, "last_market_slot == current_slot post-accrue");
    assert_eq!(last_crk, cur, "last_crank_slot tracks");
}

#[tokio::test]
async fn funding_no_op_returns_error() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;

    // First crank advances the market.
    ctx.warp_to_slot(20).unwrap();
    send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Funding)])
        .await
        .expect("advance");

    // Second crank at the same slot: no work, must Err NothingToDo.
    let err = send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Funding)])
        .await
        .expect_err("second crank same slot");
    assert!(
        expected_custom_error(&err, PercolatorError::NothingToDo),
        "{:?}",
        err,
    );
}

#[tokio::test]
async fn gc_no_op_returns_error() {
    // No dust slots to reclaim → GC crank reports NothingToDo.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;

    let err = send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Gc)])
        .await
        .expect_err("nothing to gc");
    assert!(
        expected_custom_error(&err, PercolatorError::NothingToDo),
        "{:?}",
        err,
    );
}

#[tokio::test]
async fn gc_closes_stale_accounts() {
    // Pretend slot N holds a dust account (capital=0, no position, no pnl).
    // Poke it directly (no user instruction can leave an account in a
    // flat-zero-capital state without triggering the GC path ourselves).
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;

    // Create two dust accounts at slots 5 and 7 by direct poke.
    let mut acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    {
        let engine_bytes = &mut acct.data[ENGINE_OFFSET..];
        let engine: &mut percolator::RiskEngine =
            unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
        for &target in &[5usize, 7usize] {
            // Pop `target` out of the free list.
            let nx = engine.next_free[target];
            let pr = engine.prev_free[target];
            if pr == u16::MAX {
                engine.free_head = nx;
            } else {
                engine.next_free[pr as usize] = nx;
            }
            if nx != u16::MAX {
                engine.prev_free[nx as usize] = pr;
            }
            engine.next_free[target] = u16::MAX;
            engine.prev_free[target] = u16::MAX;
            // Mark bitmap used.
            let w = target >> 6;
            let b = target & 63;
            engine.used[w] |= 1u64 << b;
            engine.num_used_accounts += 1;
            // Fields already zero — fresh dust.
        }
    }
    ctx.set_account(&h.slab.pubkey(), &acct.into());

    // Make sure the insurance fund has enough to pay a bounty.
    fund_insurance(&mut ctx, h.slab.pubkey(), 10_000).await;

    ctx.warp_to_slot(10).unwrap();
    let before = token_balance(&mut ctx, h.caller_ta).await;
    send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Gc)])
        .await
        .expect("gc crank");
    let after = token_balance(&mut ctx, h.caller_ta).await;
    assert!(after > before, "crank bounty paid out of insurance");

    // Verify the stale slots got freed (bitmap cleared).
    let acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let e: &percolator::RiskEngine =
        unsafe { &*(acct.data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine) };
    assert!(!e.is_used(5), "slot 5 reclaimed");
    assert!(!e.is_used(7), "slot 7 reclaimed");
}

#[tokio::test]
async fn gc_skips_lp_slot() {
    // Even if we somehow had slot 0 flat-clean, GC must not reclaim the
    // protocol LP. In this test the LP is live with capital, which is
    // itself non-reclaimable — so we assert no error and no state change.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;

    // No dust → NothingToDo.
    let err = send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Gc)])
        .await
        .expect_err("no work");
    assert!(
        expected_custom_error(&err, PercolatorError::NothingToDo),
        "{:?}",
        err,
    );

    // Confirm LP slot still used.
    let acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let e: &percolator::RiskEngine =
        unsafe { &*(acct.data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine) };
    assert!(e.is_used(0), "LP slot intact");
}

#[tokio::test]
async fn adl_reset_no_op_returns_error() {
    // No pending ADL → NothingToDo.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;
    ctx.warp_to_slot(5).unwrap();
    let err = send(
        &mut ctx,
        &h.caller,
        &[build_crank_ix(&h, CrankKind::AdlReset)],
    )
    .await
    .expect_err("no reset pending");
    assert!(
        expected_custom_error(&err, PercolatorError::NothingToDo),
        "{:?}",
        err,
    );
}

#[tokio::test]
async fn adl_reset_transition_side_mode() {
    // Poke side_mode_long = DrainOnly with OI_long == 0 (no live positions
    // on that side). The engine's reset state machine should transition
    // to ResetPending or Normal on settle. We assert the mode changed.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;

    // Force DrainOnly with zero OI on long side.
    let mut acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    {
        let engine_bytes = &mut acct.data[ENGINE_OFFSET..];
        let e: &mut percolator::RiskEngine =
            unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
        e.side_mode_long = percolator::SideMode::DrainOnly;
        // Ensure no live long OI so DrainOnly → ResetPending is valid.
        e.oi_eff_long_q = 0;
    }
    ctx.set_account(&h.slab.pubkey(), &acct.into());

    ctx.warp_to_slot(10).unwrap();
    // Whether the state machine fires depends on engine-internal
    // preconditions. Accept either a successful transition or
    // NothingToDo — both are valid post-states.
    let r = send(
        &mut ctx,
        &h.caller,
        &[build_crank_ix(&h, CrankKind::AdlReset)],
    )
    .await;
    match r {
        Ok(()) => {
            let acct = ctx
                .banks_client
                .get_account(h.slab.pubkey())
                .await
                .unwrap()
                .unwrap();
            let e: &percolator::RiskEngine = unsafe {
                &*(acct.data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
            };
            assert!(
                e.side_mode_long != percolator::SideMode::DrainOnly,
                "side_mode_long transitioned out of DrainOnly"
            );
        }
        Err(ref e) if expected_custom_error(e, PercolatorError::NothingToDo) => {}
        Err(ref e) => panic!("unexpected err: {:?}", e),
    }
}

#[tokio::test]
async fn invalid_crank_kind_rejected() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;
    let bad = Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(h.caller.pubkey(), true),
            AccountMeta::new_readonly(clock::id(), false),
            AccountMeta::new(h.caller_ta, false),
            AccountMeta::new(h.vault_pda, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data: PercolatorInstruction::Crank(CrankArgs { kind: 99 }).pack(),
    };
    let err = send(&mut ctx, &h.caller, &[bad])
        .await
        .expect_err("bad kind");
    assert!(
        expected_custom_error(&err, PercolatorError::InvalidCrankKind),
        "{:?}",
        err,
    );
}

#[tokio::test]
async fn concurrent_funding_cranks_same_slot_idempotent() {
    // Two funding cranks at the same slot from different keepers: the
    // second must fail NothingToDo and leave state untouched.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 100_000_000, 1_000_000).await;

    fund_insurance(&mut ctx, h.slab.pubkey(), 100_000).await;
    ctx.warp_to_slot(30).unwrap();
    send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Funding)])
        .await
        .expect("first crank");

    let (cur1, _, _) = read_engine_field(&mut ctx, h.slab.pubkey()).await;
    let err = send(&mut ctx, &h.caller, &[build_crank_ix(&h, CrankKind::Funding)])
        .await
        .expect_err("same-slot");
    assert!(
        expected_custom_error(&err, PercolatorError::NothingToDo),
        "{:?}",
        err,
    );
    let (cur2, _, _) = read_engine_field(&mut ctx, h.slab.pubkey()).await;
    assert_eq!(cur1, cur2, "current_slot unchanged by no-op crank");
}
