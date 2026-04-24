//! End-to-end tests for `CreateMarket` — the permissionless paid listing
//! (task #23). Mirrors the CreateSlab flow but routes a 0.5 SOL fee to
//! `TREASURY_PUBKEY` and writes `origin = ORIGIN_OPEN`.

use percolator_program::{
    error::PercolatorError,
    instruction::{CreateMarketArgs, InstructionTag, PercolatorInstruction},
    processor::{MIN_MARKET_CREATION_FEE_LAMPORTS, TREASURY_PUBKEY},
    state::{find_vault_pda, slab_account_size, SlabHeader, ORIGIN_OPEN, ORIGIN_SEEDED},
};
use solana_program::{
    instruction::{AccountMeta, Instruction, InstructionError},
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

fn program_test(program_id: Pubkey) -> ProgramTest {
    let mut pt = ProgramTest::new(
        "percolator_program",
        program_id,
        processor!(percolator_program::process_instruction),
    );
    // Pre-fund the canonical treasury so it exists on chain.
    pt.add_account(
        TREASURY_PUBKEY,
        Account {
            lamports: 10_000_000, // starting balance we can watch grow
            data: vec![],
            owner: system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.set_compute_max_units(5_000_000);
    pt
}

fn alloc_slab_ix(
    payer: &Pubkey,
    slab: &Pubkey,
    program_id: Pubkey,
) -> Instruction {
    let size = slab_account_size() as u64;
    let lamports = Rent::default().minimum_balance(size as usize);
    system_instruction::create_account(payer, slab, lamports, size, &program_id)
}

fn build_create_market_ix(
    program_id: Pubkey,
    payer: Pubkey,
    slab: Pubkey,
    mint: Pubkey,
    oracle: Pubkey,
    treasury: Pubkey,
    vault_bump: u8,
    fee_lamports: u64,
) -> Instruction {
    let ix_data =
        PercolatorInstruction::CreateMarket(CreateMarketArgs { vault_bump, fee_lamports })
            .pack();
    assert_eq!(ix_data[0], InstructionTag::CreateMarket as u8);
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(slab, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(oracle, false),
            AccountMeta::new(treasury, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(spl_token::ID, false),
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

async fn run(
    ctx: &mut ProgramTestContext,
    slab_kp: &Keypair,
    ixs: &[Instruction],
) -> Result<(), BanksClientError> {
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(ixs, Some(&ctx.payer.pubkey()));
    tx.sign(&[&ctx.payer, slab_kp], bh);
    ctx.banks_client.process_transaction(tx).await
}

async fn run_no_slab_sign(
    ctx: &mut ProgramTestContext,
    ixs: &[Instruction],
) -> Result<(), BanksClientError> {
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(ixs, Some(&ctx.payer.pubkey()));
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await
}

// ---------------- scenarios ----------------

#[tokio::test]
async fn happy_path_fee_routes_and_origin_open() {
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let mut ctx = pt.start_with_context().await;
    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();
    let (_vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

    let treasury_before = ctx
        .banks_client
        .get_account(TREASURY_PUBKEY)
        .await
        .unwrap()
        .unwrap()
        .lamports;

    let alloc = alloc_slab_ix(&ctx.payer.pubkey(), &slab.pubkey(), program_id);
    let cm = build_create_market_ix(
        program_id,
        ctx.payer.pubkey(),
        slab.pubkey(),
        mint,
        oracle,
        TREASURY_PUBKEY,
        vault_bump,
        MIN_MARKET_CREATION_FEE_LAMPORTS,
    );
    run(&mut ctx, &slab, &[alloc, cm]).await.expect("create_market");

    let treasury_after = ctx
        .banks_client
        .get_account(TREASURY_PUBKEY)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        treasury_after - treasury_before,
        MIN_MARKET_CREATION_FEE_LAMPORTS,
        "fee reached the canonical treasury"
    );

    let slab_acct = ctx
        .banks_client
        .get_account(slab.pubkey())
        .await
        .unwrap()
        .unwrap();
    let header = SlabHeader::read_from(&slab_acct.data[..SlabHeader::LEN]).unwrap();
    assert_eq!(header.mint, mint);
    assert_eq!(header.oracle, oracle);
    assert_eq!(header.creator, ctx.payer.pubkey());
    assert_eq!(header.vault_bump, vault_bump);
    assert_eq!(header.origin, ORIGIN_OPEN);
}

#[tokio::test]
async fn wrong_treasury_rejected() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let attacker_treasury = Pubkey::new_unique();
    pt.add_account(
        attacker_treasury,
        Account {
            lamports: 0,
            data: vec![],
            owner: system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();
    let (_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

    let alloc = alloc_slab_ix(&ctx.payer.pubkey(), &slab.pubkey(), program_id);
    let cm = build_create_market_ix(
        program_id,
        ctx.payer.pubkey(),
        slab.pubkey(),
        mint,
        oracle,
        attacker_treasury,
        vault_bump,
        MIN_MARKET_CREATION_FEE_LAMPORTS,
    );
    let err = run(&mut ctx, &slab, &[alloc, cm])
        .await
        .expect_err("wrong treasury must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::WrongTreasury),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn unallocated_slab_rejected() {
    // CreateMarket without the allocate-create_account step first: the slab
    // pubkey points at nothing owned by our program, so the check fails.
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let mut ctx = pt.start_with_context().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();
    let (_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

    let cm = build_create_market_ix(
        program_id,
        ctx.payer.pubkey(),
        slab.pubkey(),
        mint,
        oracle,
        TREASURY_PUBKEY,
        vault_bump,
        MIN_MARKET_CREATION_FEE_LAMPORTS,
    );
    let err = run_no_slab_sign(&mut ctx, &[cm])
        .await
        .expect_err("no alloc → fail");
    // When the slab account doesn't exist, Solana surfaces a runtime
    // error (not a program error). We only assert that the tx failed.
    assert!(matches!(err, BanksClientError::TransactionError(_)));
}

#[tokio::test]
async fn replay_rejected() {
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let mut ctx = pt.start_with_context().await;
    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();
    let (_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

    let alloc = alloc_slab_ix(&ctx.payer.pubkey(), &slab.pubkey(), program_id);
    let cm = build_create_market_ix(
        program_id,
        ctx.payer.pubkey(),
        slab.pubkey(),
        mint,
        oracle,
        TREASURY_PUBKEY,
        vault_bump,
        MIN_MARKET_CREATION_FEE_LAMPORTS,
    );
    run(&mut ctx, &slab, &[alloc, cm.clone()])
        .await
        .expect("first");

    // Second CreateMarket against the same slab (no alloc step, since the
    // account already exists) must fail on the "header already populated"
    // guard.
    ctx.warp_to_slot(5).unwrap();
    let err = run_no_slab_sign(&mut ctx, &[cm])
        .await
        .expect_err("replay must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::SlabAlreadyInitialized),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn slab_header_len_unchanged() {
    // If the origin byte push caused us to accidentally stretch the header,
    // `slab_account_size()` would shift and CreateSlab tests would break —
    // but we also assert directly.
    assert_eq!(SlabHeader::LEN, 104, "header stays 104 bytes");
}

#[tokio::test]
async fn above_floor_fee_accepted_and_routed() {
    // Tiered pricing: when the wrapper bumps the fee to 1.5 SOL after the
    // first 10 listings, the program must accept it and route the full
    // amount to the treasury (not just the floor).
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let mut ctx = pt.start_with_context().await;
    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();
    let (_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

    let treasury_before = ctx
        .banks_client
        .get_account(TREASURY_PUBKEY)
        .await
        .unwrap()
        .unwrap()
        .lamports;

    const FEE: u64 = 1_500_000_000; // 1.5 SOL (the standard tier)
    let alloc = alloc_slab_ix(&ctx.payer.pubkey(), &slab.pubkey(), program_id);
    let cm = build_create_market_ix(
        program_id,
        ctx.payer.pubkey(),
        slab.pubkey(),
        mint,
        oracle,
        TREASURY_PUBKEY,
        vault_bump,
        FEE,
    );
    run(&mut ctx, &slab, &[alloc, cm]).await.expect("create_market");

    let treasury_after = ctx
        .banks_client
        .get_account(TREASURY_PUBKEY)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        treasury_after - treasury_before,
        FEE,
        "full tier-2 fee reached treasury",
    );
}

#[tokio::test]
async fn below_floor_fee_rejected() {
    // A client that hand-crafts an ix with a sub-floor fee (trying to
    // bypass the frontend tier gate) must be rejected on-chain.
    let program_id = Pubkey::new_unique();
    let pt = program_test(program_id);
    let mut ctx = pt.start_with_context().await;
    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();
    let (_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);

    let under_floor = MIN_MARKET_CREATION_FEE_LAMPORTS - 1;
    let alloc = alloc_slab_ix(&ctx.payer.pubkey(), &slab.pubkey(), program_id);
    let cm = build_create_market_ix(
        program_id,
        ctx.payer.pubkey(),
        slab.pubkey(),
        mint,
        oracle,
        TREASURY_PUBKEY,
        vault_bump,
        under_floor,
    );
    let err = run(&mut ctx, &slab, &[alloc, cm])
        .await
        .expect_err("fee below floor must fail");
    assert!(
        expected_custom_error(&err, PercolatorError::ListingFeeTooLow),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn seeded_header_origin_still_zero() {
    // After CreateSlab (origin must default to ORIGIN_SEEDED=0). We can't
    // call CreateSlab here — it shares a file-local helper in
    // tests/create_slab.rs — so we just assert the `SlabHeader::new`
    // default directly.
    let h = SlabHeader::new(
        Pubkey::new_from_array([1u8; 32]),
        Pubkey::new_from_array([2u8; 32]),
        Pubkey::new_from_array([3u8; 32]),
        0,
        0,
    );
    assert_eq!(h.origin, ORIGIN_SEEDED);
    assert_eq!(ORIGIN_SEEDED, 0);
    assert_eq!(ORIGIN_OPEN, 1);
}
