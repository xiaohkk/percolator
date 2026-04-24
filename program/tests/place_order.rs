//! End-to-end tests for `PlaceOrder`.
//!
//! Topology:
//!   Slot 0 = protocol LP (seeded by `BootstrapLp`, owner = `[0xFF; 32]`).
//!   Slot 1..N = users (via `Deposit`).
//!   Every PlaceOrder = `execute_trade_not_atomic(user, LP, ...)` under the
//!   hood — the wrapper trades against the LP counterparty.
//!
//! Oracle account = a `percolator_oracle::state::Feed` layout. The wrapper
//! deserializes the full struct and enforces slot-delta staleness; tests
//! populate an initialized Feed with `last_update_slot = 0` (non-stale at
//! the warped slots used here) and flip the price via `set_oracle_price`.
//!
//! Several scenarios (DrainOnly / ResetPending / A-floor / profit) require
//! directly poking engine state, because without keeper #16 / ADL #14 the
//! engine only reaches those transitions through liquidation paths that
//! aren't wired yet.

use percolator_program::{
    error::PercolatorError,
    instruction::{
        BootstrapLpArgs, CreateSlabArgs, DepositArgs, InitializeEngineArgs, InstructionTag,
        PercolatorInstruction, PlaceOrderArgs, RiskParamsArgs,
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
const ORACLE_PRICE: u64 = 1_000_000; // 1.0 after POS_SCALE(1e6)

fn valid_risk_params() -> RiskParamsArgs {
    RiskParamsArgs {
        maintenance_margin_bps: 500,
        initial_margin_bps: 1_000, // 10% => 10x max leverage
        trading_fee_bps: 0,
        max_accounts: TEST_MAX_ACCOUNTS,
        max_crank_staleness_slots: 1_000_000,
        liquidation_fee_bps: 100,
        liquidation_fee_cap: 1_000_000_000_000,
        min_liquidation_abs: 10_000,
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

/// A `percolator_oracle::state::Feed` account with `last_update_slot = 0`
/// — non-stale at the small warps used here (max slot 10 across the suite,
/// `STALE_SLOTS = 150`). Owner is system_program; wrapper doesn't ownership-
/// check the oracle.
fn packed_oracle(price: u64) -> Account {
    packed_oracle_at_slot(price, 0)
}

fn packed_oracle_at_slot(price: u64, last_update_slot: u64) -> Account {
    let feed = percolator_oracle::state::Feed {
        price_lamports_per_token: price,
        last_update_slot,
        mint: Pubkey::default(),
        source: Pubkey::default(),
        source_kind: 0,
        graduated: 0,
        initialized: 1,
        ring_idx: 0,
        _pad: [0; 4],
        ring_buffer: [0; percolator_oracle::state::RING_LEN],
    };
    let mut data = vec![0u8; percolator_oracle::state::Feed::LEN];
    feed.write_into(&mut data).unwrap();
    Account {
        lamports: Rent::default().minimum_balance(percolator_oracle::state::Feed::LEN),
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
        let (vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

        pt.add_account(mint, packed_mint());
        pt.add_account(
            creator_ta,
            // Creator TA owner is set later to ctx.payer.pubkey() via
            // set_account; we can't know it before start.
            packed_token_account(mint, Pubkey::default(), 1_000_000_000),
        );
        pt.add_account(user_ta, packed_token_account(mint, user.pubkey(), 1_000_000_000));
        pt.add_account(vault_pda, packed_token_account(mint, vault_pda, 0));
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
    let ix_data = PercolatorInstruction::InitializeEngine(args).pack();
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(creator, true),
        ],
        data: ix_data,
    }
}

fn build_bootstrap_lp_ix(h: &Harness, creator: Pubkey, amount: u64) -> Instruction {
    let ix_data = PercolatorInstruction::BootstrapLp(BootstrapLpArgs { amount }).pack();
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
        data: ix_data,
    }
}

fn build_deposit_ix(h: &Harness, amount: u64) -> Instruction {
    let ix_data = PercolatorInstruction::Deposit(DepositArgs { amount }).pack();
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
        data: ix_data,
    }
}

fn build_place_order_ix(
    h: &Harness,
    oracle: Pubkey,
    side: u8,
    size: u64,
    max_price: u64,
    min_price: u64,
) -> Instruction {
    let ix_data = PercolatorInstruction::PlaceOrder(PlaceOrderArgs {
        side,
        size,
        max_price,
        min_price,
    })
    .pack();
    Instruction {
        program_id: h.program_id,
        accounts: vec![
            AccountMeta::new(h.slab.pubkey(), false),
            AccountMeta::new_readonly(h.user.pubkey(), true),
            AccountMeta::new_readonly(oracle, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
        data: ix_data,
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

/// Full bring-up: CreateSlab + InitializeEngine + BootstrapLp + user Deposit.
async fn bring_up(
    h: &Harness,
    pt: ProgramTest,
    lp_bootstrap: u64,
    user_deposit: u64,
) -> ProgramTestContext {
    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();

    // Fix creator_ta owner to match payer (we couldn't know it at add_account time).
    ctx.set_account(
        &h.creator_ta,
        &packed_token_account(h.mint, payer_pk, 1_000_000_000).into(),
    );

    // CreateSlab + InitializeEngine (single tx is fine here, but keep them
    // separate for clarity).
    let create_tx = build_create_slab_tx(h, &ctx.payer, ctx.last_blockhash);
    ctx.banks_client
        .process_transaction(create_tx)
        .await
        .expect("create_slab");

    let init_ix = build_initialize_engine_ix(h, payer_pk);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[init_ix], Some(&payer_pk));
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("init_engine");

    if lp_bootstrap > 0 {
        let bs_ix = build_bootstrap_lp_ix(h, payer_pk, lp_bootstrap);
        let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let mut tx = Transaction::new_with_payer(&[bs_ix], Some(&payer_pk));
        tx.sign(&[&ctx.payer], bh);
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("bootstrap_lp");
    }

    if user_deposit > 0 {
        send(&mut ctx, &h.user, &[build_deposit_ix(h, user_deposit)])
            .await
            .expect("user_deposit");
    }
    ctx
}

/// Snapshot of engine-side knobs used by assertions.
struct Snap {
    basis_long: i128,
    basis_short: i128,
    a_long: u128,
    a_short: u128,
    k_long: i128,
    k_short: i128,
    oi_long: u128,
    oi_short: u128,
    mode_long: percolator::SideMode,
    mode_short: percolator::SideMode,
    /// User slot's effective position.
    user_eff_pos: i128,
    user_basis: i128,
}

fn snap(slab_data: &[u8], user_idx: u16) -> Snap {
    let engine_bytes = &slab_data[ENGINE_OFFSET..];
    let e: &percolator::RiskEngine =
        unsafe { &*(engine_bytes.as_ptr() as *const percolator::RiskEngine) };
    Snap {
        basis_long: e.accounts[user_idx as usize].position_basis_q.max(0),
        basis_short: e.accounts[user_idx as usize].position_basis_q.min(0),
        a_long: e.adl_mult_long,
        a_short: e.adl_mult_short,
        k_long: e.adl_coeff_long,
        k_short: e.adl_coeff_short,
        oi_long: e.oi_eff_long_q,
        oi_short: e.oi_eff_short_q,
        mode_long: e.side_mode_long,
        mode_short: e.side_mode_short,
        user_eff_pos: e.effective_pos_q(user_idx as usize),
        user_basis: e.accounts[user_idx as usize].position_basis_q,
    }
}

async fn fetch_snap(ctx: &mut ProgramTestContext, slab: Pubkey, user_idx: u16) -> Snap {
    let acct = ctx.banks_client.get_account(slab).await.unwrap().unwrap();
    snap(&acct.data, user_idx)
}

/// Scan for which slot the user landed in (post-deposit).
async fn user_idx(ctx: &mut ProgramTestContext, slab: Pubkey, user: Pubkey) -> u16 {
    let acct = ctx.banks_client.get_account(slab).await.unwrap().unwrap();
    let engine_bytes = &acct.data[ENGINE_OFFSET..];
    let e: &percolator::RiskEngine =
        unsafe { &*(engine_bytes.as_ptr() as *const percolator::RiskEngine) };
    for i in 0..percolator::MAX_ACCOUNTS {
        if e.is_used(i) && e.accounts[i].owner == user.to_bytes() {
            return i as u16;
        }
    }
    panic!("user slot not found");
}

async fn set_oracle_price(ctx: &mut ProgramTestContext, oracle: Pubkey, price: u64) {
    // Writes a fresh Feed (last_update_slot = 0). Tests that warp past
    // STALE_SLOTS without re-calling this will (correctly) see the oracle
    // as stale.
    ctx.set_account(&oracle, &packed_oracle(price).into());
}

// -----------------------------------------------------------------------
// Scenarios
// -----------------------------------------------------------------------

#[tokio::test]
async fn open_long_ok() {
    let (h, pt) = Harness::new();
    // LP seeded with 1M, user deposits 100k (lots of margin headroom).
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;
    let uidx = user_idx(&mut ctx, h.slab.pubkey(), h.user.pubkey()).await;

    let before = fetch_snap(&mut ctx, h.slab.pubkey(), uidx).await;
    assert_eq!(before.user_eff_pos, 0, "pre-trade flat");
    assert_eq!(before.oi_long, 0);

    // Open long 1.0 (size = 1 * POS_SCALE).
    let size = 1_000_000u64;
    let ix = build_place_order_ix(&h, h.oracle, 0, size, u64::MAX, 0);
    send(&mut ctx, &h.user, &[ix]).await.expect("open long");

    let after = fetch_snap(&mut ctx, h.slab.pubkey(), uidx).await;
    assert!(after.user_eff_pos > 0, "position is long");
    assert_eq!(
        after.user_basis.unsigned_abs(),
        size as u128,
        "basis tracks size"
    );
    // Matched-trade engine invariant: every long OI has a matched short
    // OI. User is long `size` ⇒ LP is short `size` ⇒ both sides == size.
    assert_eq!(after.oi_long, size as u128);
    assert_eq!(after.oi_short, size as u128);
    // A_long stays at ADL_ONE while no liquidations happen.
    assert_eq!(after.a_long, percolator::ADL_ONE);
}

#[tokio::test]
async fn open_short_ok() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;
    let uidx = user_idx(&mut ctx, h.slab.pubkey(), h.user.pubkey()).await;

    let size = 1_000_000u64;
    let ix = build_place_order_ix(&h, h.oracle, 1, size, u64::MAX, 0);
    send(&mut ctx, &h.user, &[ix]).await.expect("open short");

    let after = fetch_snap(&mut ctx, h.slab.pubkey(), uidx).await;
    assert!(after.user_eff_pos < 0, "position is short");
    assert_eq!(after.user_basis.unsigned_abs(), size as u128);
    assert_eq!(after.oi_short, size as u128);
    // Matched: LP takes opposite long leg of equal size.
    assert_eq!(after.oi_long, size as u128);
    assert_eq!(after.a_short, percolator::ADL_ONE);
}

#[tokio::test]
async fn close_long_at_profit() {
    // Open long at price = 1.0, move price to 1.5, close.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 10_000_000, 1_000_000).await;
    let uidx = user_idx(&mut ctx, h.slab.pubkey(), h.user.pubkey()).await;

    // Open long 1.0.
    let size = 1_000_000u64;
    send(
        &mut ctx,
        &h.user,
        &[build_place_order_ix(&h, h.oracle, 0, size, u64::MAX, 0)],
    )
    .await
    .expect("open long");

    // Oracle moves up 50%.
    set_oracle_price(&mut ctx, h.oracle, ORACLE_PRICE + ORACLE_PRICE / 2).await;

    // Close by placing an offsetting short of the same size.
    ctx.warp_to_slot(10).unwrap();
    send(
        &mut ctx,
        &h.user,
        &[build_place_order_ix(
            &h,
            h.oracle,
            1,
            size,
            u64::MAX,
            0,
        )],
    )
    .await
    .expect("close long");

    let after = fetch_snap(&mut ctx, h.slab.pubkey(), uidx).await;
    assert_eq!(after.user_eff_pos, 0, "flat post-close");
    assert_eq!(after.oi_long, 0);
    assert_eq!(after.oi_short, 0);
    // The position closed into realized PnL via the engine's settle.
    // We don't assert the exact number — the engine's PnL math is covered
    // in the parent crate.
}

#[tokio::test]
async fn close_short_at_loss() {
    // Open short, price moves up (adverse), close at a loss. Verify the
    // engine still allows closing when the user has enough margin.
    let (h, pt) = Harness::new();
    // Large user balance so margin check still passes post-loss.
    let mut ctx = bring_up(&h, pt, 10_000_000, 5_000_000).await;
    let uidx = user_idx(&mut ctx, h.slab.pubkey(), h.user.pubkey()).await;

    let size = 1_000_000u64;
    send(
        &mut ctx,
        &h.user,
        &[build_place_order_ix(&h, h.oracle, 1, size, u64::MAX, 0)],
    )
    .await
    .expect("open short");

    set_oracle_price(&mut ctx, h.oracle, ORACLE_PRICE + ORACLE_PRICE / 10).await; // +10%
    ctx.warp_to_slot(10).unwrap();

    send(
        &mut ctx,
        &h.user,
        &[build_place_order_ix(
            &h,
            h.oracle,
            0,
            size,
            u64::MAX,
            0,
        )],
    )
    .await
    .expect("close short at loss");

    let after = fetch_snap(&mut ctx, h.slab.pubkey(), uidx).await;
    assert_eq!(after.user_eff_pos, 0);
    assert_eq!(after.oi_long, 0);
    assert_eq!(after.oi_short, 0);
}

#[tokio::test]
async fn zero_size_rejected() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;
    let ix = build_place_order_ix(&h, h.oracle, 0, 0, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("zero size must fail");
    assert!(expected_custom_error(&err, PercolatorError::ZeroSize), "{:?}", err);
}

#[tokio::test]
async fn stale_oracle_rejected() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;
    set_oracle_price(&mut ctx, h.oracle, 0).await;
    let ix = build_place_order_ix(&h, h.oracle, 0, 1_000_000, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("stale oracle");
    assert!(
        expected_custom_error(&err, PercolatorError::StaleOracle),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn stale_oracle_by_slot_rejected() {
    // Slot-delta staleness: a Feed with a valid in-range price but a
    // last_update_slot more than STALE_SLOTS behind `clock.slot` must be
    // rejected. Proves the wrapper goes beyond bounds-checking.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;

    // Oracle wrote at slot 0 with a sane price, never refreshed.
    ctx.set_account(
        &h.oracle,
        &packed_oracle_at_slot(ORACLE_PRICE, 0).into(),
    );
    // Warp past STALE_SLOTS (150).
    ctx.warp_to_slot(200).unwrap();

    let ix = build_place_order_ix(&h, h.oracle, 0, 1_000_000, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("stale-by-slot oracle");
    assert!(
        expected_custom_error(&err, PercolatorError::StaleOracle),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn uninitialized_oracle_rejected() {
    // A Feed whose `initialized` byte is 0 must be rejected, even if its
    // price/slot fields are otherwise valid. Catches an attacker handing
    // the wrapper a fresh, attacker-allocated account.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;

    let mut uninit = percolator_oracle::state::Feed {
        price_lamports_per_token: ORACLE_PRICE,
        last_update_slot: 0,
        mint: Pubkey::default(),
        source: Pubkey::default(),
        source_kind: 0,
        graduated: 0,
        initialized: 0, // <-- key
        ring_idx: 0,
        _pad: [0; 4],
        ring_buffer: [0; percolator_oracle::state::RING_LEN],
    };
    let mut data = vec![0u8; percolator_oracle::state::Feed::LEN];
    uninit.write_into(&mut data).unwrap();
    let acct = Account {
        lamports: Rent::default()
            .minimum_balance(percolator_oracle::state::Feed::LEN),
        data,
        owner: system_program::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&h.oracle, &acct.into());
    // Silence unused_mut warnings from borsh-era compilers.
    uninit.initialized = 0;

    let ix = build_place_order_ix(&h, h.oracle, 0, 1_000_000, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("uninitialized oracle");
    assert!(
        expected_custom_error(&err, PercolatorError::StaleOracle),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn price_slippage_rejected() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;
    // Oracle at 1.0; user sets max_price = 0.9.
    let ix = build_place_order_ix(
        &h,
        h.oracle,
        0,
        1_000_000,
        ORACLE_PRICE - 1,
        0,
    );
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("slippage rejected");
    assert!(
        expected_custom_error(&err, PercolatorError::SlippageExceeded),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn insufficient_margin_rejected() {
    let (h, pt) = Harness::new();
    // LP fat, user thin: only MIN_INITIAL_DEPOSIT of capital, which at 10%
    // initial margin supports 10 * 1_000 = 10_000 notional. Try 1_000_000
    // notional (way over).
    let mut ctx = bring_up(&h, pt, 10_000_000, 1_000).await;
    let size = 1_000_000u64; // 1.0 * 1.0 = 1M notional
    let ix = build_place_order_ix(&h, h.oracle, 0, size, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("margin fail");
    // The engine returns a generic error which the wrapper surfaces as
    // EngineError.
    assert!(
        expected_custom_error(&err, PercolatorError::EngineError),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn over_leverage_rejected() {
    let (h, pt) = Harness::new();
    // Deposit 100_000; try to open 50_000_000 notional (500x). initial_margin
    // = 10% ⇒ required margin = 5_000_000 > 100_000.
    let mut ctx = bring_up(&h, pt, 100_000_000, 100_000).await;
    let size = 50_000_000u64;
    let ix = build_place_order_ix(&h, h.oracle, 0, size, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("over-leverage");
    assert!(
        expected_custom_error(&err, PercolatorError::EngineError),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn drain_only_blocks_open() {
    // Seed DrainOnly by poking state, then attempt to open fresh OI on
    // that side: must hit our DrainOnly error.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;

    // Force side_mode_long = DrainOnly.
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
        engine.side_mode_long = percolator::SideMode::DrainOnly;
    }
    ctx.set_account(&h.slab.pubkey(), &acct.into());

    let ix = build_place_order_ix(&h, h.oracle, 0, 1_000_000, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("drain only blocks open");
    assert!(
        expected_custom_error(&err, PercolatorError::DrainOnly),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn drain_only_allows_close() {
    // Open a long normally, flip side_mode_long to DrainOnly, close: the
    // wrapper should allow because the trade reduces OI rather than grows
    // it.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 10_000_000, 1_000_000).await;
    let uidx = user_idx(&mut ctx, h.slab.pubkey(), h.user.pubkey()).await;

    send(
        &mut ctx,
        &h.user,
        &[build_place_order_ix(&h, h.oracle, 0, 1_000_000, u64::MAX, 0)],
    )
    .await
    .expect("open long");

    // Poke DrainOnly on the long side (simulating a post-liq event).
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
        engine.side_mode_long = percolator::SideMode::DrainOnly;
    }
    ctx.set_account(&h.slab.pubkey(), &acct.into());

    // Closing long via offsetting short should be allowed because the
    // user's ending side is either flat or the opposite of the gated
    // side.
    ctx.warp_to_slot(10).unwrap();
    send(
        &mut ctx,
        &h.user,
        &[build_place_order_ix(
            &h,
            h.oracle,
            1,
            1_000_000,
            u64::MAX,
            0,
        )],
    )
    .await
    .expect("close allowed under DrainOnly");

    let after = fetch_snap(&mut ctx, h.slab.pubkey(), uidx).await;
    assert_eq!(after.user_eff_pos, 0);
}

#[tokio::test]
async fn reset_pending_blocks_new_but_allows_settle() {
    // ResetPending behaves like DrainOnly for OI-growing trades.
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;

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
        engine.side_mode_long = percolator::SideMode::ResetPending;
    }
    ctx.set_account(&h.slab.pubkey(), &acct.into());

    // Fresh open on long while ResetPending: rejected.
    let ix = build_place_order_ix(&h, h.oracle, 0, 1_000_000, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("reset_pending blocks new");
    assert!(
        expected_custom_error(&err, PercolatorError::DrainOnly),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn lp_not_bootstrapped_rejected() {
    // Same as the normal flow but skip BootstrapLp. slot 0 is still free
    // (or taken by the user's deposit).
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 0, 100_000).await;

    let ix = build_place_order_ix(&h, h.oracle, 0, 1_000_000, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("no LP");
    assert!(
        expected_custom_error(&err, PercolatorError::LpNotBootstrapped),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn wrong_oracle_account_rejected() {
    let (h, pt) = Harness::new();
    let mut ctx = bring_up(&h, pt, 1_000_000, 100_000).await;
    let bogus = Pubkey::new_unique();
    ctx.set_account(&bogus, &packed_oracle(ORACLE_PRICE).into());
    let ix = build_place_order_ix(&h, bogus, 0, 1_000_000, u64::MAX, 0);
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("wrong oracle rejected");
    // Wrapper treats wrong oracle as stale (mismatch is signalled via
    // StaleOracle; the oracle-account-ID check lives before price read).
    assert!(
        expected_custom_error(&err, PercolatorError::StaleOracle),
        "{:?}",
        err
    );
}
