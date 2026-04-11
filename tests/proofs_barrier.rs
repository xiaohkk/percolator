//! Kani proofs for two-phase barrier scan (spec Addendum A2)

#![cfg(kani)]

mod common;
use common::*;

// ############################################################################
// BARRIER SNAPSHOT PROOFS
// ############################################################################

/// capture_barrier_snapshot returns exact engine state fields.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_barrier_snapshot_matches_engine() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx_a = engine.add_user(0).unwrap();
    let idx_b = engine.add_user(0).unwrap();

    let oracle: u64 = kani::any();
    kani::assume(oracle > 0 && oracle <= 1_000_000);
    let slot: u64 = kani::any();
    kani::assume(slot >= DEFAULT_SLOT && slot <= DEFAULT_SLOT + 100);

    engine.deposit(idx_a, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(idx_b, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let _ = engine.accrue_market_to(slot, oracle);

    let snap = engine.capture_barrier_snapshot(slot, oracle);

    assert_eq!(snap.oracle_price_b, oracle);
    assert_eq!(snap.current_slot_b, slot);
    assert_eq!(snap.a_long_b, engine.adl_mult_long);
    assert_eq!(snap.a_short_b, engine.adl_mult_short);
    assert_eq!(snap.k_long_b, engine.adl_coeff_long);
    assert_eq!(snap.k_short_b, engine.adl_coeff_short);
    assert_eq!(snap.epoch_long_b, engine.adl_epoch_long);
    assert_eq!(snap.epoch_short_b, engine.adl_epoch_short);
    assert_eq!(snap.k_epoch_start_long_b, engine.adl_epoch_start_k_long);
    assert_eq!(snap.k_epoch_start_short_b, engine.adl_epoch_start_k_short);
    assert_eq!(snap.mode_long_b, engine.side_mode_long);
    assert_eq!(snap.mode_short_b, engine.side_mode_short);
    assert_eq!(snap.oi_eff_long_b, engine.oi_eff_long_q);
    assert_eq!(snap.oi_eff_short_b, engine.oi_eff_short_q);

    kani::cover!(true, "snapshot always reachable");
}

// ############################################################################
// PREVIEW CLASSIFICATION PROOFS
// ############################################################################

/// preview_account_at_barrier: epoch_snap != epoch_side never returns Safe.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_preview_epoch_mismatch_not_safe() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Give account a position
    engine.accounts[idx as usize].position_basis_q = 100_000i128;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long += 1;

    // Make epoch_side different from epoch_snap
    let epoch_offset: u64 = kani::any();
    kani::assume(epoch_offset >= 1 && epoch_offset <= 5);
    engine.adl_epoch_long = epoch_offset;

    let barrier = engine.capture_barrier_snapshot(DEFAULT_SLOT, DEFAULT_ORACLE);
    let class = engine.preview_account_at_barrier(idx, &barrier);

    assert_ne!(class, ReviewClass::Safe,
        "epoch mismatch must never be classified Safe");
    kani::cover!(class == ReviewClass::ReviewCleanupResetProgress, "reset progress reached");
    kani::cover!(class == ReviewClass::ReviewLiquidation, "liquidation reached");
}

/// preview_account_at_barrier: flat account with negative PnL → ReviewCleanup.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_preview_flat_negative_cleanup() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Flat account, negative PnL
    let pnl: i128 = kani::any();
    kani::assume(pnl < 0 && pnl > -100_000);
    engine.set_pnl(idx as usize, pnl);

    let barrier = engine.capture_barrier_snapshot(DEFAULT_SLOT, DEFAULT_ORACLE);
    let class = engine.preview_account_at_barrier(idx, &barrier);

    assert_eq!(class, ReviewClass::ReviewCleanup,
        "flat negative PnL must be ReviewCleanup");
    kani::cover!(true, "flat negative always ReviewCleanup");
}

/// preview_account_at_barrier: missing account returns Missing.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_preview_missing_account() {
    let engine = RiskEngine::new(zero_fee_params());

    let idx: u16 = kani::any();
    kani::assume(idx < MAX_ACCOUNTS as u16 + 5);

    let barrier = BarrierSnapshot {
        oracle_price_b: DEFAULT_ORACLE,
        current_slot_b: DEFAULT_SLOT,
        a_long_b: ADL_ONE,
        a_short_b: ADL_ONE,
        k_long_b: 0,
        k_short_b: 0,
        epoch_long_b: 0,
        epoch_short_b: 0,
        k_epoch_start_long_b: 0,
        k_epoch_start_short_b: 0,
        mode_long_b: SideMode::Normal,
        mode_short_b: SideMode::Normal,
        oi_eff_long_b: 0,
        oi_eff_short_b: 0,
        maintenance_margin_bps: 500,
    };

    let class = engine.preview_account_at_barrier(idx, &barrier);
    assert_eq!(class, ReviewClass::Missing,
        "unused account must be Missing");
}

// ############################################################################
// BARRIER WAVE INVARIANT PROOFS
// ############################################################################

/// keeper_barrier_wave preserves OI balance on healthy-account path.
/// Concrete inputs to keep SAT tractable — component functions are
/// symbolically verified in their own proofs.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_barrier_wave_oi_balance() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 500_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 500_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let size = 50 * POS_SCALE as i128;
    engine.execute_trade_not_atomic(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size, DEFAULT_ORACLE, 0i128, 0).unwrap();

    let slot = DEFAULT_SLOT + 1;
    let scan: [u16; 2] = [a, b];

    let result = engine.keeper_barrier_wave(a, slot, DEFAULT_ORACLE, 0i128, &scan, 4, 0);
    assert!(result.is_ok(), "barrier_wave must succeed on healthy accounts");
    assert_eq!(engine.oi_eff_long_q, engine.oi_eff_short_q,
        "OI_long == OI_short after barrier_wave");
    kani::cover!(true, "barrier_wave healthy path");
}

/// keeper_barrier_wave preserves conservation on crash path (liquidation).
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_barrier_wave_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // High leverage: 80 units at price 1000 with 100k capital (80% of IM)
    let size = 80 * POS_SCALE as i128;
    engine.execute_trade_not_atomic(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size, DEFAULT_ORACLE, 0i128, 0).unwrap();

    // Price crash — triggers liquidation in barrier_wave
    let crash_oracle = 500u64;
    let slot = DEFAULT_SLOT + 1;
    let scan: [u16; 2] = [a, b];

    let result = engine.keeper_barrier_wave(b, slot, crash_oracle, 0i128, &scan, 4, 0);
    if result.is_ok() {
        assert!(engine.check_conservation(),
            "conservation must hold after barrier_wave with liquidation");
    }
    kani::cover!(result.is_ok(), "barrier_wave crash path");
}
