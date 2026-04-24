//! End-to-end tests for the oracle adapter program.
//!
//! Source accounts are fabricated: we pre-seed `ProgramTest` accounts whose
//! bytes match each source kind's expected layout. The adapter reads bytes
//! at well-known offsets; we exercise every offset path.

use percolator_oracle::{
    instruction::{
        ConvertSourceArgs, InitializeFeedArgs, InstructionTag, OracleInstruction,
    },
    state::{Feed, SourceKind, RING_LEN},
    OracleError,
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

const INIT_PRICE: u64 = 1_000_000; // arbitrary starting lamports-per-token

// ---------- fixtures ----------

fn program_test(program_id: Pubkey) -> ProgramTest {
    let mut pt = ProgramTest::new(
        "percolator_oracle",
        program_id,
        processor!(percolator_oracle::process_instruction),
    );
    pt.set_compute_max_units(1_000_000);
    pt
}

/// Bonding-curve layout:
///   0x00..0x08  discriminator
///   0x08..0x10  virtual_sol_reserves  u64 LE
///   0x10..0x18  virtual_token_reserves u64 LE
///   ...
///   0xA6        complete flag (u8)
fn packed_pump_bonding(sol: u64, tokens: u64, complete: bool) -> Account {
    let mut data = vec![0u8; 0xC0];
    data[0x08..0x10].copy_from_slice(&sol.to_le_bytes());
    data[0x10..0x18].copy_from_slice(&tokens.to_le_bytes());
    data[0xA6] = if complete { 1 } else { 0 };
    Account {
        lamports: Rent::default().minimum_balance(data.len()),
        data,
        owner: system_program::ID,
        executable: false,
        rent_epoch: 0,
    }
}

/// PumpSwap / Raydium layout (simplified):
///   0x08..0x10  base_reserve  u64 LE
///   0x10..0x18  quote_reserve u64 LE
fn packed_amm(base: u64, quote: u64) -> Account {
    let mut data = vec![0u8; 0x40];
    data[0x08..0x10].copy_from_slice(&base.to_le_bytes());
    data[0x10..0x18].copy_from_slice(&quote.to_le_bytes());
    Account {
        lamports: Rent::default().minimum_balance(data.len()),
        data,
        owner: system_program::ID,
        executable: false,
        rent_epoch: 0,
    }
}

/// Meteora DLMM layout (our adapter):
///   0x08..0x0C  active_bin_id (i32 LE)
///   0x0C..0x0E  bin_step (u16 LE)
///   0x0E..0x16  base_price (u64 LE)
fn packed_meteora(bin_id: i32, bin_step: u16, base_price: u64) -> Account {
    let mut data = vec![0u8; 0x30];
    data[0x08..0x0C].copy_from_slice(&bin_id.to_le_bytes());
    data[0x0C..0x0E].copy_from_slice(&bin_step.to_le_bytes());
    data[0x0E..0x16].copy_from_slice(&base_price.to_le_bytes());
    Account {
        lamports: Rent::default().minimum_balance(data.len()),
        data,
        owner: system_program::ID,
        executable: false,
        rent_epoch: 0,
    }
}

fn build_initialize_feed_ix(
    program_id: Pubkey,
    feed: Pubkey,
    source: Pubkey,
    mint: Pubkey,
    kind: SourceKind,
) -> Instruction {
    let ix_data = OracleInstruction::InitializeFeed(InitializeFeedArgs {
        mint: mint.to_bytes(),
        source: source.to_bytes(),
        source_kind: kind as u8,
    })
    .pack();
    assert_eq!(ix_data[0], InstructionTag::InitializeFeed as u8);
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(feed, false),
            AccountMeta::new_readonly(source, false),
        ],
        data: ix_data,
    }
}

fn build_update_ix(program_id: Pubkey, feed: Pubkey, source: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(feed, false),
            AccountMeta::new_readonly(source, false),
        ],
        data: OracleInstruction::Update.pack(),
    }
}

fn build_graduate_ix(program_id: Pubkey, feed: Pubkey, source: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(feed, false),
            AccountMeta::new_readonly(source, false),
        ],
        data: OracleInstruction::Graduate.pack(),
    }
}

fn build_convert_ix(
    program_id: Pubkey,
    feed: Pubkey,
    new_source: Pubkey,
    new_kind: SourceKind,
) -> Instruction {
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(feed, false),
            AccountMeta::new_readonly(new_source, false),
        ],
        data: OracleInstruction::ConvertSource(ConvertSourceArgs {
            new_kind: new_kind as u8,
            new_source: new_source.to_bytes(),
        })
        .pack(),
    }
}

fn expected_custom_error(err: &BanksClientError, want: OracleError) -> bool {
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

/// Allocate + assign a feed account to our program. Returns the keypair.
async fn alloc_feed(
    ctx: &mut ProgramTestContext,
    program_id: Pubkey,
) -> Keypair {
    let feed_kp = Keypair::new();
    let size = Feed::LEN as u64;
    let lamports = Rent::default().minimum_balance(size as usize);
    let ix = system_instruction::create_account(
        &ctx.payer.pubkey(),
        &feed_kp.pubkey(),
        lamports,
        size,
        &program_id,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&ctx.payer.pubkey()));
    tx.sign(&[&ctx.payer, &feed_kp], bh);
    ctx.banks_client.process_transaction(tx).await.expect("alloc");
    feed_kp
}

async fn send(
    ctx: &mut ProgramTestContext,
    ixs: &[Instruction],
) -> Result<(), BanksClientError> {
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tx = Transaction::new_with_payer(ixs, Some(&ctx.payer.pubkey()));
    tx.sign(&[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await
}

async fn read_feed(ctx: &mut ProgramTestContext, feed: Pubkey) -> Feed {
    let acct = ctx.banks_client.get_account(feed).await.unwrap().unwrap();
    Feed::read_from(&acct.data[..Feed::LEN]).unwrap()
}

// ---------- scenarios ----------

#[tokio::test]
async fn initialize_feed_and_stale_source_rejected() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let good_src = Pubkey::new_unique();
    let stale_src = Pubkey::new_unique();
    pt.add_account(good_src, packed_amm(1_000, 2_000));
    pt.add_account(stale_src, packed_amm(0, 0));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    // Stale source must fail.
    let err = send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            stale_src,
            mint,
            SourceKind::PumpSwap,
        )],
    )
    .await
    .expect_err("stale source must fail init");
    assert!(
        expected_custom_error(&err, OracleError::StaleSource),
        "{:?}",
        err
    );

    // Fresh initialization from a valid source.
    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            good_src,
            mint,
            SourceKind::PumpSwap,
        )],
    )
    .await
    .expect("init");

    let f = read_feed(&mut ctx, feed.pubkey()).await;
    assert!(f.is_initialized());
    assert!(!f.is_graduated());
    assert_eq!(f.source_kind, SourceKind::PumpSwap as u8);
    assert_eq!(f.mint, mint);
    assert_eq!(f.source, good_src);
    assert!(f.price_lamports_per_token > 0);
    assert_eq!(f.ring_idx, 1);
}

#[tokio::test]
async fn update_pumpbonding_reads_reserves() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    // 30 SOL vs 1_000_000 tokens ⇒ small lamport-per-token price.
    let src = Pubkey::new_unique();
    pt.add_account(src, packed_pump_bonding(30_000_000_000, 1_000_000_000_000, false));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::PumpBonding,
        )],
    )
    .await
    .expect("init bonding");
    let f1 = read_feed(&mut ctx, feed.pubkey()).await;
    let p1 = f1.price_lamports_per_token;
    assert!(p1 > 0);

    // Mutate reserves: less tokens ⇒ higher price.
    ctx.set_account(
        &src,
        &packed_pump_bonding(30_000_000_000, 500_000_000_000, false).into(),
    );
    ctx.warp_to_slot(10).unwrap();
    send(&mut ctx, &[build_update_ix(program_id, feed.pubkey(), src)])
        .await
        .expect("update");
    let f2 = read_feed(&mut ctx, feed.pubkey()).await;
    assert!(
        f2.price_lamports_per_token >= p1,
        "fewer tokens should not lower the price (median): p1={} p2={}",
        p1,
        f2.price_lamports_per_token
    );
}

#[tokio::test]
async fn update_pumpswap_appends_ring_and_reads_median() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let src = Pubkey::new_unique();
    pt.add_account(src, packed_amm(1_000_000, INIT_PRICE));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::PumpSwap,
        )],
    )
    .await
    .expect("init");

    // Push 5 different price points into the ring. Must advance the slot
    // each time so the tx hashes differ.
    let mut prev_ring_idx = 1u8;
    for (step, (base, quote)) in [
        (1_000_000u64, INIT_PRICE),
        (2_000_000, INIT_PRICE),
        (3_000_000, INIT_PRICE),
        (4_000_000, INIT_PRICE),
        (5_000_000, INIT_PRICE),
    ]
    .iter()
    .enumerate()
    {
        ctx.set_account(&src, &packed_amm(*base, *quote).into());
        ctx.warp_to_slot((step as u64 + 1) * 2).unwrap();
        send(&mut ctx, &[build_update_ix(program_id, feed.pubkey(), src)])
            .await
            .expect("update");
        let f = read_feed(&mut ctx, feed.pubkey()).await;
        assert_ne!(f.ring_idx, prev_ring_idx, "ring_idx advanced");
        prev_ring_idx = f.ring_idx;
    }
    let f = read_feed(&mut ctx, feed.pubkey()).await;
    // 6 non-zero entries: init + 5 updates. Median is the 3rd smallest
    // (lower median on even n). The math floors each price — the exact
    // median depends on ordering but must be within the min/max bounds.
    let mut observed: Vec<u64> = f.ring_buffer.iter().copied().filter(|v| *v != 0).collect();
    observed.sort();
    assert!(
        f.price_lamports_per_token >= observed[0]
            && f.price_lamports_per_token <= *observed.last().unwrap(),
        "median within ring bounds"
    );
}

#[tokio::test]
async fn update_raydium_reads_reserves() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let src = Pubkey::new_unique();
    pt.add_account(src, packed_amm(10_000, 50_000));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::RaydiumCpmm,
        )],
    )
    .await
    .expect("init");
    let f = read_feed(&mut ctx, feed.pubkey()).await;
    // quote/base * 1e6 = 50_000/10_000 * 1_000_000 = 5_000_000.
    assert_eq!(f.price_lamports_per_token, 5_000_000);
}

#[tokio::test]
async fn update_meteora_reads_active_bin() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let src = Pubkey::new_unique();
    // base_price = 1_000_000, bin_step = 100bps, active_bin = 10
    // → price = 1_000_000 * (1.01)^10 ≈ 1_104_622.
    pt.add_account(src, packed_meteora(10, 100, 1_000_000));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::MeteoraDlmm,
        )],
    )
    .await
    .expect("init meteora");
    let f = read_feed(&mut ctx, feed.pubkey()).await;
    // Integer-math result of repeated (1 + 1%) multiplies is slightly
    // below the continuous (1.01)^10 ≈ 1.1046. Asserting within a
    // reasonable band.
    assert!(
        f.price_lamports_per_token > 1_050_000
            && f.price_lamports_per_token < 1_150_000,
        "meteora price ~ 1.1x base, got {}",
        f.price_lamports_per_token
    );
}

#[tokio::test]
async fn graduate_requires_complete_flag() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let src = Pubkey::new_unique();
    pt.add_account(src, packed_pump_bonding(10_000_000_000, 100_000_000_000, false));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::PumpBonding,
        )],
    )
    .await
    .expect("init");

    // Not graduated yet.
    let err = send(
        &mut ctx,
        &[build_graduate_ix(program_id, feed.pubkey(), src)],
    )
    .await
    .expect_err("graduation must fail while complete=false");
    assert!(
        expected_custom_error(&err, OracleError::NotGraduated),
        "{:?}",
        err
    );

    // Flip the flag; graduation succeeds.
    ctx.set_account(
        &src,
        &packed_pump_bonding(10_000_000_000, 100_000_000_000, true).into(),
    );
    ctx.warp_to_slot(5).unwrap();
    send(
        &mut ctx,
        &[build_graduate_ix(program_id, feed.pubkey(), src)],
    )
    .await
    .expect("graduate");
    let f = read_feed(&mut ctx, feed.pubkey()).await;
    assert!(f.is_graduated());
}

#[tokio::test]
async fn graduate_is_idempotent_once_only() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let src = Pubkey::new_unique();
    pt.add_account(src, packed_pump_bonding(10_000_000_000, 100_000_000_000, true));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::PumpBonding,
        )],
    )
    .await
    .expect("init");
    ctx.warp_to_slot(5).unwrap();
    send(
        &mut ctx,
        &[build_graduate_ix(program_id, feed.pubkey(), src)],
    )
    .await
    .expect("first graduate");
    ctx.warp_to_slot(6).unwrap();
    let err = send(
        &mut ctx,
        &[build_graduate_ix(program_id, feed.pubkey(), src)],
    )
    .await
    .expect_err("second graduate");
    assert!(
        expected_custom_error(&err, OracleError::AlreadyGraduated),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn convert_source_pump_bonding_to_pumpswap() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let bonding = Pubkey::new_unique();
    let pumpswap = Pubkey::new_unique();
    pt.add_account(bonding, packed_pump_bonding(10_000_000_000, 100_000_000_000, true));
    pt.add_account(pumpswap, packed_amm(1_000_000, 2_000_000));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            bonding,
            mint,
            SourceKind::PumpBonding,
        )],
    )
    .await
    .expect("init");
    ctx.warp_to_slot(5).unwrap();
    send(
        &mut ctx,
        &[build_graduate_ix(program_id, feed.pubkey(), bonding)],
    )
    .await
    .expect("graduate");
    ctx.warp_to_slot(10).unwrap();
    send(
        &mut ctx,
        &[build_convert_ix(program_id, feed.pubkey(), pumpswap, SourceKind::PumpSwap)],
    )
    .await
    .expect("convert");

    let f = read_feed(&mut ctx, feed.pubkey()).await;
    assert_eq!(f.source, pumpswap);
    assert_eq!(f.source_kind, SourceKind::PumpSwap as u8);
    // Ring reset — one fresh sample at index 0.
    assert_eq!(f.ring_idx, 1);
    assert!(f.price_lamports_per_token > 0);
}

#[tokio::test]
async fn convert_source_rejected_without_graduation() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let bonding = Pubkey::new_unique();
    let new_src = Pubkey::new_unique();
    pt.add_account(bonding, packed_pump_bonding(10_000_000_000, 100_000_000_000, false));
    pt.add_account(new_src, packed_amm(1_000_000, 2_000_000));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            bonding,
            mint,
            SourceKind::PumpBonding,
        )],
    )
    .await
    .expect("init");
    let err = send(
        &mut ctx,
        &[build_convert_ix(program_id, feed.pubkey(), new_src, SourceKind::PumpSwap)],
    )
    .await
    .expect_err("convert without graduation");
    assert!(
        expected_custom_error(&err, OracleError::ConvertRequiresGraduation),
        "{:?}",
        err
    );
}

#[tokio::test]
async fn ring_buffer_wraps_and_median_stays_bounded() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let src = Pubkey::new_unique();
    pt.add_account(src, packed_amm(1_000_000, 1_000_000));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::PumpSwap,
        )],
    )
    .await
    .expect("init");

    // 35 updates (> RING_LEN = 30) to exercise wrap.
    for i in 0..35u64 {
        let base = 1_000_000 - i * 10_000;
        ctx.set_account(&src, &packed_amm(base, 1_000_000).into());
        ctx.warp_to_slot(i + 2).unwrap();
        send(&mut ctx, &[build_update_ix(program_id, feed.pubkey(), src)])
            .await
            .expect("update");
    }

    let f = read_feed(&mut ctx, feed.pubkey()).await;
    // ring_idx should have wrapped.
    assert!((f.ring_idx as usize) < RING_LEN);
    // Median should be sane (within the observed range).
    let observed_max = f.ring_buffer.iter().copied().max().unwrap();
    let observed_min = f
        .ring_buffer
        .iter()
        .copied()
        .filter(|v| *v != 0)
        .min()
        .unwrap();
    assert!(
        f.price_lamports_per_token >= observed_min
            && f.price_lamports_per_token <= observed_max,
        "median within bounds [{}, {}], got {}",
        observed_min,
        observed_max,
        f.price_lamports_per_token,
    );
}

#[tokio::test]
async fn update_on_graduated_bonding_rejected() {
    let program_id = Pubkey::new_unique();
    let mut pt = program_test(program_id);
    let mint = Pubkey::new_unique();
    let src = Pubkey::new_unique();
    pt.add_account(src, packed_pump_bonding(10_000_000_000, 100_000_000_000, true));
    let mut ctx = pt.start_with_context().await;
    let feed = alloc_feed(&mut ctx, program_id).await;

    send(
        &mut ctx,
        &[build_initialize_feed_ix(
            program_id,
            feed.pubkey(),
            src,
            mint,
            SourceKind::PumpBonding,
        )],
    )
    .await
    .expect("init");
    ctx.warp_to_slot(5).unwrap();
    send(
        &mut ctx,
        &[build_graduate_ix(program_id, feed.pubkey(), src)],
    )
    .await
    .expect("grad");

    ctx.warp_to_slot(10).unwrap();
    let err = send(&mut ctx, &[build_update_ix(program_id, feed.pubkey(), src)])
        .await
        .expect_err("update after graduation must fail");
    assert!(
        expected_custom_error(&err, OracleError::AlreadyGraduated),
        "{:?}",
        err
    );
}
