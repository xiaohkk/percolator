//! End-to-end tests for the `Deposit` instruction.
//!
//! Pattern per test:
//!   1. Preload a mint + user ATA (with tokens) + vault ATA into ProgramTest.
//!   2. CreateSlab + InitializeEngine.
//!   3. Build and submit a Deposit tx.
//!   4. Assert engine aggregates (capital, C_tot, vault) match vault token
//!      balance.

use percolator_program::{
    error::PercolatorError,
    instruction::{
        CreateSlabArgs, DepositArgs, InitializeEngineArgs, InstructionTag, PercolatorInstruction,
        RiskParamsArgs,
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
use solana_program_test::{processor, BanksClientError, ProgramTest};
use solana_sdk::{
    account::Account,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::{Transaction, TransactionError},
};
use spl_token::state::{Account as TokenAccount, AccountState, Mint};

const TEST_MAX_ACCOUNTS: u64 = 64;

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

/// Full test harness: generated keys + preloaded accounts for a slab that's
/// ready to receive Deposit ixs.
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
    fn new(user_starting_balance: u64) -> (Self, ProgramTest) {
        let program_id = Pubkey::new_unique();
        let mut pt = program_test(program_id);

        let slab = Keypair::new();
        let mint = Pubkey::new_unique();
        let user = Keypair::new();
        // The user's "ATA" is just a fixed pubkey we pre-seed; we don't use
        // the real ATA PDA because that would require running the
        // associated-token-account program.
        let user_ta = Pubkey::new_unique();

        let (vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

        pt.add_account(mint, packed_mint());
        pt.add_account(user_ta, packed_token_account(mint, user.pubkey(), user_starting_balance));
        pt.add_account(vault_pda, packed_token_account(mint, vault_pda, 0));
        // Fund the user so they can pay tx fees (the first deposit tx uses
        // the user as signer + fee-payer in some tests).
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

/// Read the engine aggregates (capital at idx 0-63, C_tot, vault) for
/// assertion.
fn read_engine_snapshot(slab_data: &[u8]) -> EngineSnapshot {
    let engine_bytes = &slab_data[ENGINE_OFFSET..];
    let engine_ptr = engine_bytes.as_ptr() as *const percolator::RiskEngine;
    let engine: &percolator::RiskEngine = unsafe { &*engine_ptr };
    let mut caps = [0u128; TEST_MAX_ACCOUNTS as usize];
    for i in 0..TEST_MAX_ACCOUNTS as usize {
        caps[i] = engine.accounts[i].capital.get();
    }
    EngineSnapshot {
        c_tot: engine.c_tot.get(),
        vault: engine.vault.get(),
        num_used: engine.num_used_accounts,
        capitals: caps,
    }
}

struct EngineSnapshot {
    c_tot: u128,
    vault: u128,
    num_used: u16,
    capitals: [u128; TEST_MAX_ACCOUNTS as usize],
}

fn read_vault_balance(ta_data: &[u8]) -> u64 {
    TokenAccount::unpack(ta_data).unwrap().amount
}

/// Run CreateSlab + InitializeEngine; return a started context ready for
/// Deposit testing.
async fn bring_up(harness: &Harness, pt: ProgramTest) -> solana_program_test::ProgramTestContext {
    let mut ctx = pt.start_with_context().await;
    let bh = ctx.last_blockhash;

    // Preload a dummy oracle account so CreateSlab can reference one.
    let oracle = Pubkey::new_unique();
    // CreateSlab only reads the oracle pubkey, never the account data, so a
    // missing account is fine — the runtime tolerates it as long as the
    // meta is `new_readonly`. If that changes, we'd pt.add_account an
    // empty-data Account here.

    let create_tx = build_create_slab_tx(harness, &ctx.payer, oracle, bh);
    ctx.banks_client
        .process_transaction(create_tx)
        .await
        .expect("create_slab");

    // InitializeEngine (creator = payer, since build_create_slab_tx uses
    // payer as signer).
    let init_ix = build_initialize_engine_ix(harness, ctx.payer.pubkey());
    let bh2 = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[init_ix], Some(&ctx.payer.pubkey()));
    tx.sign(&[&ctx.payer], bh2);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("initialize_engine");

    ctx
}

// -----------------------------------------------------------------------
// Scenarios
// -----------------------------------------------------------------------

#[tokio::test]
async fn fresh_deposit() {
    let (h, pt) = Harness::new(1_000_000);
    let mut ctx = bring_up(&h, pt).await;

    let amount = 5_000u64;
    let ix = build_deposit_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        amount,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&h.user.pubkey()));
    tx.sign(&[&h.user], bh);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit");

    let slab_acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let snap = read_engine_snapshot(&slab_acct.data);
    assert_eq!(snap.num_used, 1, "fresh deposit materializes one slot");
    assert_eq!(snap.c_tot, amount as u128);
    assert_eq!(snap.vault, amount as u128);
    // We can't predict which idx the freelist picks, so scan.
    let mut found = false;
    for i in 0..TEST_MAX_ACCOUNTS as usize {
        if snap.capitals[i] != 0 {
            assert_eq!(snap.capitals[i], amount as u128);
            found = true;
            break;
        }
    }
    assert!(found, "one account must hold capital");

    let vault_acct = ctx
        .banks_client
        .get_account(h.vault_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        read_vault_balance(&vault_acct.data),
        amount,
        "vault balance == engine.V"
    );
}

#[tokio::test]
async fn repeat_deposit() {
    let (h, pt) = Harness::new(1_000_000);
    let mut ctx = bring_up(&h, pt).await;

    for &amount in &[2_000u64, 3_000, 500] {
        let ix = build_deposit_ix(
            h.program_id,
            h.slab.pubkey(),
            h.user.pubkey(),
            h.user_ta,
            h.vault_pda,
            h.mint,
            amount,
        );
        let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let mut tx = Transaction::new_with_payer(&[ix], Some(&h.user.pubkey()));
        tx.sign(&[&h.user], bh);
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("deposit");
        // Advance slot so the next tx's blockhash is fresh and the engine's
        // `now_slot` is strictly monotonic.
        let next_slot = ctx.banks_client.get_root_slot().await.unwrap() + 1;
        ctx.warp_to_slot(next_slot.max(2)).ok();
    }

    let slab_acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let snap = read_engine_snapshot(&slab_acct.data);
    let total = 2_000u128 + 3_000 + 500;
    assert_eq!(snap.num_used, 1, "single slot accumulates");
    assert_eq!(snap.c_tot, total);
    assert_eq!(snap.vault, total);
    let vault_acct = ctx
        .banks_client
        .get_account(h.vault_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_vault_balance(&vault_acct.data), total as u64);
}

#[tokio::test]
async fn zero_amount_rejected() {
    let (h, pt) = Harness::new(1_000_000);
    let mut ctx = bring_up(&h, pt).await;

    let ix = build_deposit_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        0,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&h.user.pubkey()));
    tx.sign(&[&h.user], bh);
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("zero deposit must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::ZeroAmount),
        "got {:?}",
        err
    );
}

#[tokio::test]
async fn wrong_mint_rejected() {
    let (h, pt) = Harness::new(1_000_000);
    let mut ctx = bring_up(&h, pt).await;

    // Point the Deposit ix at a different mint pubkey. The slab header's
    // mint won't match.
    let bogus_mint = Pubkey::new_unique();
    let ix = build_deposit_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        bogus_mint,
        1_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&h.user.pubkey()));
    tx.sign(&[&h.user], bh);
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("wrong mint must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::WrongMint),
        "got {:?}",
        err
    );
}

#[tokio::test]
async fn slab_full_rejected() {
    // Preload the engine with MAX_ACCOUNTS-1 fake used slots (fill freelist
    // except one). This is achieved by running MAX_ACCOUNTS-1 real fresh
    // deposits from distinct users.
    // Cheaper proxy: deposit once, then hand-craft the bitmap in the slab
    // data to mark all other slots used. Tests the wrapper's capacity guard
    // without spending CU on MAX_ACCOUNTS materializations.
    //
    // The test uses the proxy: one real deposit (user A), then force-fill
    // the bitmap to num_used = max_accounts, then try a fresh-user deposit.
    let (h, pt) = Harness::new(1_000_000);
    let mut ctx = bring_up(&h, pt).await;

    // First real deposit to materialize user's slot.
    let ix = build_deposit_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        5_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&h.user.pubkey()));
    tx.sign(&[&h.user], bh);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("first deposit");

    // Hand-edit the engine: drain the freelist to simulate "slab full" so
    // our wrapper's SlabFull check fires without materializing 63 accounts.
    let mut slab_acct = ctx
        .banks_client
        .get_account(h.slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    {
        let engine_bytes = &mut slab_acct.data[ENGINE_OFFSET..];
        let engine_ptr = engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine;
        let engine: &mut percolator::RiskEngine = unsafe { &mut *engine_ptr };
        // Pretend every slot except the used one is also allocated: set the
        // freelist head to NIL. This alone triggers SlabFull in our
        // processor's find_or_create logic (we only check free_head ==
        // u16::MAX for a NEW user).
        engine.free_head = u16::MAX;
    }
    ctx.set_account(&h.slab.pubkey(), &slab_acct.into());

    // New user tries to deposit: must be rejected with SlabFull.
    let fresh_user = Keypair::new();
    let fresh_user_ta = Pubkey::new_unique();
    ctx.set_account(
        &fresh_user.pubkey(),
        &Account {
            lamports: 1_000_000_000,
            data: vec![],
            owner: system_program::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
    ctx.set_account(
        &fresh_user_ta,
        &packed_token_account(h.mint, fresh_user.pubkey(), 10_000).into(),
    );
    let ix = build_deposit_ix(
        h.program_id,
        h.slab.pubkey(),
        fresh_user.pubkey(),
        fresh_user_ta,
        h.vault_pda,
        h.mint,
        5_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&fresh_user.pubkey()));
    tx.sign(&[&fresh_user], bh);
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("slab full must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::SlabFull),
        "got {:?}",
        err
    );
}

#[tokio::test]
async fn vault_pda_mismatch_rejected() {
    let (h, pt) = Harness::new(1_000_000);
    let mut ctx = bring_up(&h, pt).await;

    // Build an attacker-controlled token account at a random pubkey (not
    // the vault PDA), with the same mint.
    let attacker_vault = Pubkey::new_unique();
    ctx.set_account(
        &attacker_vault,
        &packed_token_account(h.mint, Pubkey::new_unique(), 0).into(),
    );

    let ix = build_deposit_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        attacker_vault,
        h.mint,
        1_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&h.user.pubkey()));
    tx.sign(&[&h.user], bh);
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("vault pda mismatch must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::VaultPdaMismatch),
        "got {:?}",
        err
    );
}

#[tokio::test]
async fn uninitialized_engine_rejected() {
    // CreateSlab but NOT InitializeEngine. Deposit must fail.
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let user = Keypair::new();
    let user_ta = Pubkey::new_unique();
    let (vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

    pt.add_account(mint, packed_mint());
    pt.add_account(user_ta, packed_token_account(mint, user.pubkey(), 1_000_000));
    pt.add_account(vault_pda, packed_token_account(mint, vault_pda, 0));
    pt.add_account(
        user.pubkey(),
        Account {
            lamports: 1_000_000_000,
            data: vec![],
            owner: system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // CreateSlab but skip InitializeEngine.
    let harness_like = Harness {
        program_id,
        slab: clone_kp(&slab),
        mint,
        vault_pda,
        vault_bump,
        user: clone_kp(&user),
        user_ta,
    };
    let oracle = Pubkey::new_unique();
    let create_tx = build_create_slab_tx(&harness_like, &ctx.payer, oracle, ctx.last_blockhash);
    ctx.banks_client
        .process_transaction(create_tx)
        .await
        .expect("create_slab");

    let ix = build_deposit_ix(
        program_id,
        slab.pubkey(),
        user.pubkey(),
        user_ta,
        vault_pda,
        mint,
        1_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&user.pubkey()));
    tx.sign(&[&user], bh);
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("pre-init deposit must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::SlabNotInitialized),
        "got {:?}",
        err
    );
}

#[tokio::test]
async fn user_token_account_wrong_owner_rejected() {
    let (h, pt) = Harness::new(1_000_000);
    let mut ctx = bring_up(&h, pt).await;

    // Replace user_ta so its `owner` field is a different pubkey than our
    // signer.
    ctx.set_account(
        &h.user_ta,
        &packed_token_account(h.mint, Pubkey::new_unique(), 1_000_000).into(),
    );

    let ix = build_deposit_ix(
        h.program_id,
        h.slab.pubkey(),
        h.user.pubkey(),
        h.user_ta,
        h.vault_pda,
        h.mint,
        1_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&h.user.pubkey()));
    tx.sign(&[&h.user], bh);
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("wrong user ATA owner must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::TokenAccountWrongOwner),
        "got {:?}",
        err
    );
}

/// Clone a Keypair (solana_sdk::signature::Keypair doesn't impl Clone).
fn clone_kp(k: &Keypair) -> Keypair {
    Keypair::from_bytes(&k.to_bytes()).unwrap()
}
