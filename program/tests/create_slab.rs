//! End-to-end test for `CreateSlab` using `solana-program-test`.
//!
//! Flow:
//!   1. Client (this test) allocates the slab account via
//!      `system_instruction::create_account`, pre-funding rent and handing
//!      ownership to the program. This is the only way to allocate ~1.5 MiB
//!      on Solana — CPI create_account is capped at 10 KiB per call.
//!   2. Same transaction: the program's `CreateSlab` instruction runs and
//!      writes the `SlabHeader`.
//!
//! Assertions:
//!   - Slab account owned by our program.
//!   - Data length = header + engine.
//!   - Header roundtrips to what we sent.
//!   - Engine region is zero.

use percolator_program::{
    instruction::{CreateSlabArgs, InstructionTag, PercolatorInstruction},
    state::{find_vault_pda, slab_account_size, SlabHeader, ENGINE_OFFSET},
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    rent::Rent,
    system_program,
};
use solana_program_test::{processor, ProgramTest};
use solana_sdk::{
    account::Account, signature::Keypair, signer::Signer, system_instruction,
    transaction::Transaction,
};

fn build_create_slab_tx(
    program_id: Pubkey,
    payer: &Keypair,
    slab: &Keypair,
    mint: Pubkey,
    oracle: Pubkey,
    bump: u8,
    blockhash: solana_sdk::hash::Hash,
) -> Transaction {
    let size = slab_account_size() as u64;
    let rent = Rent::default();
    let lamports = rent.minimum_balance(size as usize);

    // Step 1: client-side allocate. 10 MiB per-account ceiling applies here,
    // not the 10 KiB CPI cap.
    let alloc_ix = system_instruction::create_account(
        &payer.pubkey(),
        &slab.pubkey(),
        lamports,
        size,
        &program_id,
    );

    // Step 2: our program's CreateSlab.
    let (_vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);
    let ix_data = PercolatorInstruction::CreateSlab(CreateSlabArgs { bump, vault_bump }).pack();
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

#[tokio::test]
async fn create_slab_happy_path() {
    let program_id = Pubkey::new_unique();
    let mut pt = ProgramTest::new(
        "percolator_program",
        program_id,
        processor!(percolator_program::process_instruction),
    );
    // Bump the compute budget — writing 1.5 MiB of zeros costs real CU.
    pt.set_compute_max_units(5_000_000);

    let (mut banks, payer, recent_blockhash) = pt.start().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let tx = build_create_slab_tx(
        program_id,
        &payer,
        &slab,
        mint,
        oracle,
        200,
        recent_blockhash,
    );
    banks.process_transaction(tx).await.expect("create_slab tx");

    let account: Account = banks
        .get_account(slab.pubkey())
        .await
        .expect("fetch slab")
        .expect("slab exists");

    assert_eq!(account.owner, program_id, "slab should be owned by us");
    assert_eq!(
        account.data.len(),
        slab_account_size(),
        "slab data length should equal header + engine"
    );

    let header = SlabHeader::read_from(&account.data[..SlabHeader::LEN]).unwrap();
    assert_eq!(header.mint, mint);
    assert_eq!(header.oracle, oracle);
    assert_eq!(header.creator, payer.pubkey());
    assert_eq!(header.bump, 200);

    // Engine region should be all zeros — we haven't initialized it yet.
    assert!(
        account.data[ENGINE_OFFSET..].iter().all(|&b| b == 0),
        "engine region should be zero-initialized"
    );
}

#[tokio::test]
async fn create_slab_rejects_replay() {
    let program_id = Pubkey::new_unique();
    let mut pt = ProgramTest::new(
        "percolator_program",
        program_id,
        processor!(percolator_program::process_instruction),
    );
    pt.set_compute_max_units(5_000_000);
    let (mut banks, payer, recent_blockhash) = pt.start().await;

    let slab = Keypair::new();
    let mint = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let tx = build_create_slab_tx(
        program_id,
        &payer,
        &slab,
        mint,
        oracle,
        1,
        recent_blockhash,
    );
    banks.process_transaction(tx).await.expect("first create");

    // Replay: call the program's CreateSlab alone against the already-owned,
    // already-populated slab. The alloc step would fail anyway (account
    // already exists), so we only re-issue the program ix, which must fail
    // on the "already populated header" guard.
    let (_vault_pda, vault_bump) = find_vault_pda(&slab.pubkey(), &program_id);
    let ix_data = PercolatorInstruction::CreateSlab(CreateSlabArgs {
        bump: 1,
        vault_bump,
    })
    .pack();
    let ix = Instruction {
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
    let fresh_blockhash = banks.get_latest_blockhash().await.unwrap();
    let mut tx2 = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx2.sign(&[&payer], fresh_blockhash);
    let err = banks.process_transaction(tx2).await;
    assert!(err.is_err(), "replay should fail, got {:?}", err);
}
