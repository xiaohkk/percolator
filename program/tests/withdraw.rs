//! End-to-end tests for the `Withdraw` instruction.
//!
//! Some scenarios (profit with h=1 / h<1, warmup) require injecting engine
//! state that is only reachable via positions + market moves. Because
//! `PlaceOrder` / `Liquidate` / `Crank` aren't wired yet, we reach into the
//! engine's pub fields and seed the accounting directly. Each poke preserves
//! the conservation invariant `vault >= c_tot + insurance` so the engine's
//! `check_conservation` postcondition still holds.

use percolator_program::{
    error::PercolatorError,
    instruction::{
        CreateSlabArgs, DepositArgs, InitializeEngineArgs, InstructionTag, PercolatorInstruction,
        RiskParamsArgs, WithdrawArgs,
    },
    state::{find_vault_pda, slab_account_size, SlabHeader, ENGINE_OFFSET, VAULT_SEED},
};
use solana_program::{
    instruction::{AccountMeta, Instruction, InstructionError},
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_program,
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

fn valid_risk_params() -> RiskParamsArgs {
    RiskParamsArgs {
        maintenance_margin_bps: 500,
        initial_margin_bps: 1_000,
        trading_fee_bps: 10,
        max_accounts: TEST_MAX_ACCOUNTS,
        max_crank_staleness_slots: 1_000,
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
        supply: 1_000_000_000_000,
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
    user: Keypair,
    user_ta: Pubkey,
}

impl Harness {
    fn new(user_starting_balance: u64, vault_starting_balance: u64) -> (Self, ProgramTest) {
        let program_id = Pubkey::new_unique();
        let mut pt = program_test(program_id);
        let slab = Keypair::new();
        let mint = Pubkey::new_unique();
        let user = Keypair::new();
        let user_ta = Pubkey::new_unique();
        let (vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

        pt.add_account(mint, packed_mint());
        pt.add_account(user_ta, packed_token_account(mint, user.pubkey(), user_starting_balance));
        pt.add_account(
            vault_pda,
            packed_token_account(mint, vault_pda, vault_starting_balance),
        );
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

        (
            Self {
                program_id,
                slab,
                mint,
                vault_pda,
                vault_bump,
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
    oracle: Pubkey,
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
            AccountMeta::new_readonly(oracle, false),
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
        init_oracle_price: 1_000,
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

fn build_deposit_ix(
    program_id: Pubkey,
    slab: Pubkey,
    user: Pubkey,
    user_ta: Pubkey,
    vault_ta: Pubkey,
    mint: Pubkey,
    amount: u64,
) -> Instruction {
    let ix_data = PercolatorInstruction::Deposit(DepositArgs { amount }).pack();
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(slab, false),
            AccountMeta::new_readonly(user, true),
            AccountMeta::new(user_ta, false),
            AccountMeta::new(vault_ta, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: ix_data,
    }
}

fn build_withdraw_ix(
    program_id: Pubkey,
    slab: Pubkey,
    user: Pubkey,
    user_ta: Pubkey,
    vault_ta: Pubkey,
    mint: Pubkey,
    amount: u64,
) -> Instruction {
    let ix_data = PercolatorInstruction::Withdraw(WithdrawArgs { amount }).pack();
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(slab, false),
            AccountMeta::new_readonly(user, true),
            AccountMeta::new(user_ta, false),
            AccountMeta::new(vault_ta, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(system_program::ID, false),
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

/// CreateSlab + InitializeEngine + Deposit of `deposit_amount`.
async fn bring_up_with_deposit(
    h: &Harness,
    pt: ProgramTest,
    deposit_amount: u64,
) -> ProgramTestContext {
    let mut ctx = pt.start_with_context().await;
    let oracle = Pubkey::new_unique();
    let create_tx = build_create_slab_tx(h, &ctx.payer, oracle, ctx.last_blockhash);
    ctx.banks_client
        .process_transaction(create_tx)
        .await
        .expect("create_slab");

    let init_ix = build_initialize_engine_ix(h, ctx.payer.pubkey());
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[init_ix], Some(&ctx.payer.pubkey()));
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("initialize_engine");

    if deposit_amount > 0 {
        let dep_ix = build_deposit_ix(
            h.program_id,
            h.slab.pubkey(),
            h.user.pubkey(),
            h.user_ta,
            h.vault_pda,
            h.mint,
            deposit_amount,
        );
        send(&mut ctx, &h.user, &[dep_ix]).await.expect("deposit");
    }
    ctx
}

struct EngineSnapshot {
    c_tot: u128,
    vault: u128,
    pnl_pos_tot: u128,
    pnl_matured_pos_tot: u128,
    num_used: u16,
    capital_at_first_used: u128,
    pnl_at_first_used: i128,
}

fn read_engine(slab_data: &[u8]) -> EngineSnapshot {
    let engine_bytes = &slab_data[ENGINE_OFFSET..];
    let engine: &percolator::RiskEngine =
        unsafe { &*(engine_bytes.as_ptr() as *const percolator::RiskEngine) };
    let mut capital = 0u128;
    let mut pnl = 0i128;
    for i in 0..percolator::MAX_ACCOUNTS {
        if engine.is_used(i) {
            capital = engine.accounts[i].capital.get();
            pnl = engine.accounts[i].pnl;
            break;
        }
    }
    EngineSnapshot {
        c_tot: engine.c_tot.get(),
        vault: engine.vault.get(),
        pnl_pos_tot: engine.pnl_pos_tot,
        pnl_matured_pos_tot: engine.pnl_matured_pos_tot,
        num_used: engine.num_used_accounts,
        capital_at_first_used: capital,
        pnl_at_first_used: pnl,
    }
}

fn read_vault_balance(ta_data: &[u8]) -> u64 {
    TokenAccount::unpack(ta_data).unwrap().amount
}

fn read_user_ta_balance(ta_data: &[u8]) -> u64 {
    TokenAccount::unpack(ta_data).unwrap().amount
}

/// Poke engine state to fake "user has matured positive PnL of P" while
/// preserving `check_conservation` (vault >= c_tot + insurance).
/// `extra_vault_tokens` should match what the vault token account physically
/// holds beyond `c_tot` — the caller pre-seeds the vault at harness-build
/// time.
async fn poke_pnl(
    ctx: &mut ProgramTestContext,
    slab: Pubkey,
    pnl: u128,
    extra_vault_tokens: u128,
) {
    let mut slab_acct = ctx.banks_client.get_account(slab).await.unwrap().unwrap();
    {
        let engine_bytes = &mut slab_acct.data[ENGINE_OFFSET..];
        let engine: &mut percolator::RiskEngine =
            unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
        // Find the one used account.
        let mut idx = None;
        for i in 0..percolator::MAX_ACCOUNTS {
            if engine.is_used(i) {
                idx = Some(i);
                break;
            }
        }
        let i = idx.expect("no used account to poke");
        engine.accounts[i].pnl = pnl as i128;
        engine.pnl_pos_tot = engine.pnl_pos_tot.checked_add(pnl).unwrap();
        engine.pnl_matured_pos_tot = engine.pnl_matured_pos_tot.checked_add(pnl).unwrap();
        // Bump vault so conservation holds: residual = vault - c_tot - I.
        // For h=1 behavior we want residual >= pnl; the caller passes a
        // sized `extra_vault_tokens`.
        engine.vault = percolator::U128::new(
            engine.vault.get().checked_add(extra_vault_tokens).unwrap(),
        );
    }
    ctx.set_account(&slab, &slab_acct.into());
}

/// Variant of `poke_pnl` that stages reserved (non-matured) PnL, i.e.
/// positive pnl that has NOT yet matured into `pnl_matured_pos_tot`.
/// Exercises the "warmup not passed" path where the pnl is not yet
/// withdrawable even on a healthy market.
async fn poke_reserved_pnl(ctx: &mut ProgramTestContext, slab: Pubkey, pnl: u128) {
    let mut slab_acct = ctx.banks_client.get_account(slab).await.unwrap().unwrap();
    {
        let engine_bytes = &mut slab_acct.data[ENGINE_OFFSET..];
        let engine: &mut percolator::RiskEngine =
            unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
        let mut idx = None;
        for i in 0..percolator::MAX_ACCOUNTS {
            if engine.is_used(i) {
                idx = Some(i);
                break;
            }
        }
        let i = idx.expect("no used account to poke");
        engine.accounts[i].pnl = pnl as i128;
        engine.accounts[i].reserved_pnl = pnl;
        // Tracks toward pnl_pos_tot but NOT pnl_matured_pos_tot: it's
        // still warming up.
        engine.pnl_pos_tot = engine.pnl_pos_tot.checked_add(pnl).unwrap();
        engine.vault = percolator::U128::new(
            engine.vault.get().checked_add(pnl).unwrap(),
        );
    }
    ctx.set_account(&slab, &slab_acct.into());
}

// -----------------------------------------------------------------------
// Scenarios
// -----------------------------------------------------------------------

#[tokio::test]
async fn flat_account_full_withdraw() {
    let (h, pt) = Harness::new(1_000_000, 0);
    let deposit = 1_000u64;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    // Full withdraw after a 1_000-lamport deposit. Withdrawable == capital.
    let withdraw_ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        deposit,
    );
    send(&mut ctx, &h.user, &[withdraw_ix])
        .await
        .expect("withdraw");

    let slab = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let s = read_engine(&slab.data);
    assert_eq!(s.c_tot, 0, "c_tot drains to 0");
    assert_eq!(s.vault, 0, "engine V drains to 0");
    // The account slot remains allocated (the engine keeps it until GC),
    // but its capital must be zero.
    assert_eq!(s.capital_at_first_used, 0);

    let vault = ctx
        .banks_client
        .get_account(h.vault_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_vault_balance(&vault.data), 0);
    let user_ta = ctx.banks_client.get_account(h.user_ta).await.unwrap().unwrap();
    assert_eq!(read_user_ta_balance(&user_ta.data), 1_000_000);
}

#[tokio::test]
async fn partial_withdraw_leaves_valid_state() {
    let (h, pt) = Harness::new(1_000_000, 0);
    let deposit = 10_000u64;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    let w = 4_000u64;
    let withdraw_ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        w,
    );
    send(&mut ctx, &h.user, &[withdraw_ix]).await.expect("w1");

    let slab = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let s = read_engine(&slab.data);
    assert_eq!(s.c_tot, (deposit - w) as u128);
    assert_eq!(s.vault, (deposit - w) as u128);
    assert_eq!(s.capital_at_first_used, (deposit - w) as u128);
    assert_eq!(s.num_used, 1, "slot still occupied");

    let vault = ctx
        .banks_client
        .get_account(h.vault_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_vault_balance(&vault.data), deposit - w);
    let user_ta = ctx.banks_client.get_account(h.user_ta).await.unwrap().unwrap();
    assert_eq!(
        read_user_ta_balance(&user_ta.data),
        1_000_000 - deposit as u64 + w,
    );
}

#[tokio::test]
async fn over_withdraw_rejected() {
    let (h, pt) = Harness::new(1_000_000, 0);
    let deposit = 5_000u64;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    let withdraw_ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        deposit + 1,
    );
    let err = send(&mut ctx, &h.user, &[withdraw_ix])
        .await
        .expect_err("over-withdraw must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::EngineError),
        "got {:?}",
        err
    );
}

#[tokio::test]
async fn zero_amount_rejected() {
    let (h, pt) = Harness::new(1_000_000, 0);
    let mut ctx = bring_up_with_deposit(&h, pt, 5_000).await;
    let ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        0,
    );
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("zero withdraw must fail");
    assert!(expected_custom_error(&err, PercolatorError::ZeroAmount), "{:?}", err);
}

#[tokio::test]
async fn withdraw_without_deposit_rejected() {
    // Initialize the engine but skip the deposit. The user has no engine
    // slot, so the wrapper's find-by-owner scan fails.
    let (h, pt) = Harness::new(1_000_000, 0);
    let mut ctx = bring_up_with_deposit(&h, pt, 0).await;

    let ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        1_000,
    );
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("withdraw w/o deposit must fail");
    assert!(expected_custom_error(&err, PercolatorError::EngineError), "{:?}", err);
}

#[tokio::test]
async fn vault_pda_mismatch_rejected() {
    let (h, pt) = Harness::new(1_000_000, 0);
    let mut ctx = bring_up_with_deposit(&h, pt, 5_000).await;

    // A wrong-address vault token account.
    let attacker_vault = Pubkey::new_unique();
    ctx.set_account(
        &attacker_vault,
        &packed_token_account(h.mint, Pubkey::new_unique(), 0).into(),
    );
    let ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        attacker_vault,
        h.mint,
        1_000,
    );
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("vault mismatch must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::VaultPdaMismatch),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn profit_withdraw_with_h_equals_1() {
    // Deposit 10k. Poke the engine to simulate 4k of matured positive PnL
    // with 4k extra vault tokens. h = residual / matured = 4k / 4k = 1 so
    // the profit gets converted to capital during touch/finalize; the
    // user can now withdraw 14k.
    let (h, pt) = Harness::new(1_000_000, 4_000);
    let deposit = 10_000u64;
    let profit = 4_000u128;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    poke_pnl(&mut ctx, h.slab.pubkey(), profit, profit).await;

    let w = deposit as u128 + profit;
    let ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        w as u64,
    );
    send(&mut ctx, &h.user, &[ix])
        .await
        .expect("withdraw deposit+profit");

    let slab = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let s = read_engine(&slab.data);
    assert_eq!(s.c_tot, 0, "all capital drained");
    assert_eq!(s.vault, 0, "vault drained");
    assert_eq!(
        s.pnl_matured_pos_tot, 0,
        "matured PnL converted to capital and then withdrawn"
    );
    assert_eq!(s.pnl_at_first_used, 0, "account PnL cleared");

    let vault = ctx
        .banks_client
        .get_account(h.vault_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_vault_balance(&vault.data), 0);
}

#[tokio::test]
async fn profit_withdraw_with_h_below_1() {
    // Deposit 10k. Poke the engine to simulate 4k of matured positive PnL
    // with only 1k of extra vault tokens. residual = 1k, matured = 4k,
    // h = 1/4 < 1. is_whole == false so the PnL is NOT converted; the
    // account's withdrawable is the pre-existing capital only.
    //
    // Full capital (10k) still withdraws because capital is senior.
    // Attempting to withdraw more than capital fails.
    let (h, pt) = Harness::new(1_000_000, 1_000);
    let deposit = 10_000u64;
    let profit = 4_000u128;
    let vault_extra = 1_000u128;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    poke_pnl(&mut ctx, h.slab.pubkey(), profit, vault_extra).await;

    // Attempt to withdraw deposit + profit: must fail (profit stuck).
    let over_ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        deposit + profit as u64,
    );
    let err = send(&mut ctx, &h.user, &[over_ix])
        .await
        .expect_err("h<1 cannot withdraw stuck profit");
    assert!(expected_custom_error(&err, PercolatorError::EngineError), "{:?}", err);

    // Withdraw full capital succeeds.
    let ok_ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        deposit,
    );
    send(&mut ctx, &h.user, &[ok_ix])
        .await
        .expect("capital-only withdraw OK at h<1");

    let slab = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let s = read_engine(&slab.data);
    assert_eq!(s.c_tot, 0, "capital drained");
    // Stuck PnL still visible.
    assert_eq!(s.pnl_at_first_used, profit as i128);
    assert_eq!(s.pnl_matured_pos_tot, profit);
}

#[tokio::test]
async fn capital_is_senior_at_h_zero() {
    // Make h == 0 by leaving residual = 0 and still injecting matured
    // positive pnl. Capital must still be fully withdrawable: it's senior
    // to PnL claims.
    let (h, pt) = Harness::new(1_000_000, 0);
    let deposit = 10_000u64;
    let profit = 4_000u128;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    // 0 extra vault tokens ⇒ residual = vault - c_tot = 10k - 10k = 0.
    poke_pnl(&mut ctx, h.slab.pubkey(), profit, 0).await;

    let ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        deposit,
    );
    send(&mut ctx, &h.user, &[ix])
        .await
        .expect("capital withdraw must succeed at h=0");

    let slab = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let s = read_engine(&slab.data);
    assert_eq!(s.c_tot, 0);
    assert_eq!(s.pnl_at_first_used, profit as i128, "pnl still stuck");
}

#[tokio::test]
async fn warmup_not_passed_rejected() {
    // Positive PnL parked in `reserved_pnl` (bucketed under sched/pending)
    // is NOT part of `pnl_matured_pos_tot` — it's warming up. Even with
    // ample vault tokens, the wrapper can't withdraw against it because
    // the engine's `released_pos` subtracts reserved_pnl, and `is_whole`
    // uses matured only. So the attempt to withdraw deposit + reserved
    // must fail.
    let (h, pt) = Harness::new(1_000_000, 0);
    let deposit = 10_000u64;
    let reserved = 4_000u128;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    poke_reserved_pnl(&mut ctx, h.slab.pubkey(), reserved).await;

    let ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        deposit + reserved as u64,
    );
    let err = send(&mut ctx, &h.user, &[ix])
        .await
        .expect_err("pre-warmup profit not withdrawable");
    assert!(expected_custom_error(&err, PercolatorError::EngineError), "{:?}", err);
}

#[tokio::test]
async fn settle_lazy_before_check() {
    // Deposit, then advance the clock past the engine's `last_market_slot`.
    // The engine's withdraw_not_atomic MUST accrue + touch before the
    // `amount <= capital` check, else a stale K snapshot could undercount
    // capital. Even without a position, advancing the slot exercises the
    // accrue path inside withdraw_not_atomic.
    let (h, pt) = Harness::new(1_000_000, 0);
    let deposit = 10_000u64;
    let mut ctx = bring_up_with_deposit(&h, pt, deposit).await;

    // Jump the slot a few steps forward to mimic time passing.
    ctx.warp_to_slot(100).unwrap();

    let ix = build_withdraw_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        deposit,
    );
    send(&mut ctx, &h.user, &[ix])
        .await
        .expect("withdraw after slot advance triggers settle");

    let slab = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let engine_bytes = &slab.data[ENGINE_OFFSET..];
    let engine: &percolator::RiskEngine =
        unsafe { &*(engine_bytes.as_ptr() as *const percolator::RiskEngine) };
    assert!(engine.current_slot >= 100, "current_slot advanced to >= 100");
}
