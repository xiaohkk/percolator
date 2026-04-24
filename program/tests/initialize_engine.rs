//! End-to-end tests for `InitializeEngine` using `solana-program-test`.
//!
//! Flow:
//!   1. `CreateSlab` runs first to allocate + tag the account.
//!   2. `InitializeEngine` fills the engine region and flips
//!      `SlabHeader.initialized = 1`.
//!
//! These tests exercise the permission model (signer must be creator), the
//! idempotency flag (no double-init), the precondition that CreateSlab ran,
//! and the risk-params validator.

use percolator_program::{
    error::PercolatorError,
    instruction::{
        CreateSlabArgs, InitializeEngineArgs, InstructionTag, PercolatorInstruction,
        RiskParamsArgs,
    },
    state::{find_vault_pda, slab_account_size, SlabHeader, ENGINE_OFFSET},
};
use solana_program::{
    instruction::{AccountMeta, Instruction, InstructionError},
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

/// Risk-params with every field inside the parent crate's `validate_params`
/// envelope. Tests that need a specific bad value mutate a clone of this.
fn valid_risk_params() -> RiskParamsArgs {
    RiskParamsArgs {
        maintenance_margin_bps: 500,
        initial_margin_bps: 1_000,
        trading_fee_bps: 10,
        max_accounts: 64,
        max_crank_staleness_slots: 1_000,
        liquidation_fee_bps: 100,
        liquidation_fee_cap: 1_000_000_000_000,
        min_liquidation_abs: 10_000,
        min_initial_deposit: 1_000_000_000,
        min_nonzero_mm_req: 100,
        min_nonzero_im_req: 200,
        insurance_floor: 0,
        h_min: 10,
        h_max: 1_000,
        resolve_price_deviation_bps: 500,
        max_accrual_dt_slots: 1_000,
        max_abs_funding_e9_per_slot: 100,
        min_funding_lifetime_slots: 1_000,
        max_active_positions_per_side: 64,
    }
}

fn program_test(program_id: Pubkey) -> ProgramTest {
    let mut pt = ProgramTest::new(
        "percolator_program",
        program_id,
        processor!(percolator_program::process_instruction),
    );
    // Zero-filling the slab plus running init_in_place touches ~100 KiB of
    // account data — give the test a generous CU budget.
    pt.set_compute_max_units(10_000_000);
    pt
}

fn build_create_slab_tx(
    program_id: Pubkey,
    payer: &Keypair,
    slab: &Keypair,
    mint: Pubkey,
    oracle: Pubkey,
    blockhash: solana_sdk::hash::Hash,
) -> Transaction {
    let size = slab_account_size() as u64;
    let rent = Rent::default();
    let lamports = rent.minimum_balance(size as usize);

    let alloc_ix = system_instruction::create_account(
        &payer.pubkey(),
        &slab.pubkey(),
        lamports,
        size,
        &program_id,
    );

    let (_vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);
    let ix_data = PercolatorInstruction::CreateSlab(CreateSlabArgs {
        bump: 0,
        vault_bump,
    })
    .pack();
    assert_eq!(ix_data[0], InstructionTag::CreateSlab as u8);
    let create_ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(slab.pubkey(), false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(oracle, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: ix_data,
    };

    let mut tx = Transaction::new_with_payer(&[alloc_ix, create_ix], Some(&payer.pubkey()));
    tx.sign(&[payer, slab], blockhash);
    tx
}

fn build_init_engine_ix(
    program_id: Pubkey,
    slab: Pubkey,
    signer: Pubkey,
    args: InitializeEngineArgs,
) -> Instruction {
    let ix_data = PercolatorInstruction::InitializeEngine(args).pack();
    assert_eq!(ix_data[0], InstructionTag::InitializeEngine as u8);
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(slab, false),
            AccountMeta::new_readonly(signer, true),
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

#[tokio::test]
async fn happy_path() {
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let (mut banks, payer, blockhash) = pt.start().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let create_tx = build_create_slab_tx(program_id, &payer, &slab, mint, oracle, blockhash);
    banks.process_transaction(create_tx).await.expect("create_slab");

    let init_args = InitializeEngineArgs {
        risk_params: valid_risk_params(),
        init_oracle_price: 1_000_000,
    };
    let init_ix = build_init_engine_ix(program_id, slab.pubkey(), payer.pubkey(), init_args);

    let fresh_blockhash = banks.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[init_ix], Some(&payer.pubkey()));
    tx.sign(&[&payer], fresh_blockhash);
    banks.process_transaction(tx).await.expect("initialize_engine");

    let account: Account = banks
        .get_account(slab.pubkey())
        .await
        .expect("fetch slab")
        .expect("slab exists");
    let header = SlabHeader::read_from(&account.data[..SlabHeader::LEN]).unwrap();
    assert!(header.is_initialized(), "header.initialized must flip to 1");
    assert_eq!(header.mint, mint);
    assert_eq!(header.creator, payer.pubkey());

    // init_in_place writes non-zero values (ADL_ONE multipliers, the oracle
    // price, risk params, free-list linkage). If the engine region is still
    // all-zero, init never ran.
    let engine_bytes = &account.data[ENGINE_OFFSET..];
    assert!(
        engine_bytes.iter().any(|&b| b != 0),
        "engine region must be populated after InitializeEngine",
    );
}

#[tokio::test]
async fn double_init_rejected() {
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let mut ctx = pt.start_with_context().await;
    let payer_pubkey = ctx.payer.pubkey();

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let create_tx = build_create_slab_tx(
        program_id,
        &ctx.payer,
        &slab,
        mint,
        oracle,
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(create_tx)
        .await
        .expect("create_slab");

    let args = InitializeEngineArgs {
        risk_params: valid_risk_params(),
        init_oracle_price: 42,
    };

    // First init succeeds.
    let first_ix = build_init_engine_ix(program_id, slab.pubkey(), payer_pubkey, args.clone());
    let mut tx = Transaction::new_with_payer(&[first_ix], Some(&payer_pubkey));
    tx.sign(&[&ctx.payer], ctx.last_blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("first init");

    // Second call with the exact same args must fail with AlreadyInitialized.
    // Warp forward so the second tx can reach a fresh blockhash and isn't
    // deduplicated by the runtime as a replay of the first signature.
    ctx.warp_to_slot(2).unwrap();
    let fresh_blockhash = ctx
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap();
    let second_ix = build_init_engine_ix(program_id, slab.pubkey(), payer_pubkey, args);
    let mut tx = Transaction::new_with_payer(&[second_ix], Some(&payer_pubkey));
    tx.sign(&[&ctx.payer], fresh_blockhash);
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("double init must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::AlreadyInitialized),
        "expected AlreadyInitialized, got {:?}",
        err,
    );
}

#[tokio::test]
async fn wrong_signer_rejected() {
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let (mut banks, payer, blockhash) = pt.start().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let create_tx = build_create_slab_tx(program_id, &payer, &slab, mint, oracle, blockhash);
    banks.process_transaction(create_tx).await.expect("create_slab");

    // Fund an attacker keypair so it can cover tx fees, then try to init
    // against a slab whose header.creator is `payer.pubkey()`.
    let attacker = Keypair::new();
    let bh_fund = banks.get_latest_blockhash().await.unwrap();
    let fund_ix = system_instruction::transfer(&payer.pubkey(), &attacker.pubkey(), 1_000_000_000);
    let mut fund_tx = Transaction::new_with_payer(&[fund_ix], Some(&payer.pubkey()));
    fund_tx.sign(&[&payer], bh_fund);
    banks.process_transaction(fund_tx).await.expect("fund attacker");

    let args = InitializeEngineArgs {
        risk_params: valid_risk_params(),
        init_oracle_price: 1,
    };
    let ix = build_init_engine_ix(program_id, slab.pubkey(), attacker.pubkey(), args);
    let bh = banks.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&attacker.pubkey()));
    tx.sign(&[&attacker], bh);

    let err = banks
        .process_transaction(tx)
        .await
        .expect_err("wrong signer must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::WrongSigner),
        "expected WrongSigner, got {:?}",
        err,
    );
}

#[tokio::test]
async fn uninitialized_slab_rejected() {
    // No CreateSlab. The slab exists as a fresh, program-owned buffer of
    // zeros (we construct it via a direct system_instruction::create_account
    // so the header is still zeroed). InitializeEngine must refuse.
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let (mut banks, payer, blockhash) = pt.start().await;

    let slab = Keypair::new();
    let size = slab_account_size() as u64;
    let lamports = Rent::default().minimum_balance(size as usize);
    let alloc_ix = system_instruction::create_account(
        &payer.pubkey(),
        &slab.pubkey(),
        lamports,
        size,
        &program_id,
    );
    let mut alloc_tx = Transaction::new_with_payer(&[alloc_ix], Some(&payer.pubkey()));
    alloc_tx.sign(&[&payer, &slab], blockhash);
    banks.process_transaction(alloc_tx).await.expect("alloc");

    let args = InitializeEngineArgs {
        risk_params: valid_risk_params(),
        init_oracle_price: 1_000,
    };
    let ix = build_init_engine_ix(program_id, slab.pubkey(), payer.pubkey(), args);
    let bh = banks.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.sign(&[&payer], bh);

    let err = banks
        .process_transaction(tx)
        .await
        .expect_err("init without create_slab must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::SlabNotInitialized),
        "expected SlabNotInitialized, got {:?}",
        err,
    );
}

#[tokio::test]
async fn invalid_risk_params_rejected_zero_leverage() {
    // `initial_margin_bps = 0` ≡ infinite leverage. The program-side
    // validator rejects before touching the parent's init_in_place.
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let (mut banks, payer, blockhash) = pt.start().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let create_tx = build_create_slab_tx(program_id, &payer, &slab, mint, oracle, blockhash);
    banks.process_transaction(create_tx).await.expect("create_slab");

    let mut bad = valid_risk_params();
    bad.initial_margin_bps = 0;
    bad.maintenance_margin_bps = 0;
    let args = InitializeEngineArgs {
        risk_params: bad,
        init_oracle_price: 1_000,
    };

    let ix = build_init_engine_ix(program_id, slab.pubkey(), payer.pubkey(), args);
    let bh = banks.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.sign(&[&payer], bh);

    let err = banks
        .process_transaction(tx)
        .await
        .expect_err("zero initial margin must be rejected");
    assert!(
        expected_custom_error(&err, PercolatorError::InvalidRiskParams),
        "expected InvalidRiskParams (leverage=0), got {:?}",
        err,
    );
}

#[tokio::test]
async fn invalid_risk_params_rejected_zero_funding_cap() {
    // `max_accrual_dt_slots = 0` ≡ funding accrual is impossible. The
    // program-side validator rejects this envelope-breaking configuration.
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let (mut banks, payer, blockhash) = pt.start().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let create_tx = build_create_slab_tx(program_id, &payer, &slab, mint, oracle, blockhash);
    banks.process_transaction(create_tx).await.expect("create_slab");

    let mut bad = valid_risk_params();
    bad.max_accrual_dt_slots = 0;
    let args = InitializeEngineArgs {
        risk_params: bad,
        init_oracle_price: 1_000,
    };

    let ix = build_init_engine_ix(program_id, slab.pubkey(), payer.pubkey(), args);
    let bh = banks.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.sign(&[&payer], bh);

    let err = banks
        .process_transaction(tx)
        .await
        .expect_err("zero funding cap must be rejected");
    assert!(
        expected_custom_error(&err, PercolatorError::InvalidRiskParams),
        "expected InvalidRiskParams (funding_cap=0), got {:?}",
        err,
    );
}
