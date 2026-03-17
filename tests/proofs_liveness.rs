//! Section 7 — Liveness, progress, no-deadlock
//!
//! Auto-finalization, trade reopening, ADL fallback routes,
//! precision exhaustion, crank quiescence, drain-only progress.

#![cfg(kani)]

mod common;
use common::*;

// ============================================================================
// T11.43: end_instruction_auto_finalizes_ready_side
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn t11_43_end_instruction_auto_finalizes_ready_side() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.side_mode_long = SideMode::ResetPending;
    engine.oi_eff_long_q = U256::ZERO;
    engine.stale_account_count_long = 0;
    engine.stored_pos_count_long = 0;

    engine.side_mode_short = SideMode::ResetPending;
    engine.oi_eff_short_q = U256::ZERO;
    engine.stale_account_count_short = 1;
    engine.stored_pos_count_short = 0;

    let ctx = InstructionContext::new();
    engine.finalize_end_of_instruction_resets(&ctx);

    assert!(engine.side_mode_long == SideMode::Normal,
        "ready ResetPending side must auto-finalize to Normal");
    assert!(engine.side_mode_short == SideMode::ResetPending,
        "non-ready side must stay ResetPending");
}

// ============================================================================
// T11.44: trade_path_reopens_ready_reset_side
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn t11_44_trade_path_reopens_ready_reset_side() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    engine.side_mode_long = SideMode::ResetPending;
    engine.oi_eff_long_q = U256::ZERO;
    engine.oi_eff_short_q = U256::ZERO;
    engine.stale_account_count_long = 0;
    engine.stored_pos_count_long = 0;

    engine.last_oracle_price = 100;
    engine.last_market_slot = 1;
    engine.last_crank_slot = 1;
    engine.funding_price_sample_last = 100;

    let size_q = I256::from_u128(POS_SCALE);
    let result = engine.execute_trade(a, b, 100, 1, size_q, 100);

    assert!(result.is_ok(), "trade must succeed after auto-finalization of ready reset side");
    assert!(engine.side_mode_long == SideMode::Normal);
    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);
}

// ============================================================================
// T11.45: try_negate_u256_correctness
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
fn t11_45_try_negate_u256_correctness() {
    assert!(try_negate_u256_to_i256(U256::ZERO) == Some(I256::ZERO));

    assert!(try_negate_u256_to_i256(U256::ONE) == Some(I256::MINUS_ONE));

    let max_pos_mag = U256::new(u128::MAX, u128::MAX >> 1);
    let neg_max = try_negate_u256_to_i256(max_pos_mag);
    assert!(neg_max.is_some());
    let neg_max_val = neg_max.unwrap();
    assert!(neg_max_val.is_negative());

    let two_255 = U256::new(0, 1u128 << 127);
    assert!(try_negate_u256_to_i256(two_255) == Some(I256::MIN));

    let too_large = two_255.checked_add(U256::ONE).unwrap();
    assert!(try_negate_u256_to_i256(too_large).is_none());

    assert!(try_negate_u256_to_i256(U256::MAX).is_none());

    let regression = U256::new(u128::MAX, u128::MAX);
    assert!(try_negate_u256_to_i256(regression).is_none());
}

// ============================================================================
// T11.46: enqueue_adl_k_add_overflow_still_routes_quantity
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn t11_46_enqueue_adl_k_add_overflow_still_routes_quantity() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_coeff_long = I256::MIN.checked_add(I256::from_i128(1)).unwrap();
    engine.adl_mult_long = POS_SCALE;
    engine.oi_eff_long_q = U256::from_u128(4 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(4 * POS_SCALE);
    engine.insurance_fund.balance = U128::new(10_000_000);
    engine.stored_pos_count_long = 1;

    let a_before = engine.adl_mult_long;

    let d = U256::from_u128(1_000_000);
    let q_close = U256::from_u128(2 * POS_SCALE);

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(engine.adl_mult_long < a_before, "A must shrink on K overflow");
    assert!(engine.oi_eff_long_q == U256::from_u128(2 * POS_SCALE));
}

// ============================================================================
// T11.47: precision_exhaustion_terminal_drain
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn t11_47_precision_exhaustion_terminal_drain() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = 1;
    engine.adl_coeff_long = I256::ZERO;
    engine.oi_eff_long_q = U256::from_u128(3 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(3 * POS_SCALE);
    engine.stored_pos_count_long = 1;

    let q_close = U256::from_u128(POS_SCALE);
    let d = U256::ZERO;

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(ctx.pending_reset_long);
    assert!(ctx.pending_reset_short);
    assert!(engine.oi_eff_long_q.is_zero());
    assert!(engine.oi_eff_short_q.is_zero());
}

// ============================================================================
// T11.48: bankruptcy_liquidation_routes_q_when_D_zero
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn t11_48_bankruptcy_liquidation_routes_q_when_D_zero() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = POS_SCALE;
    engine.adl_coeff_long = I256::from_i128(42);
    engine.oi_eff_long_q = U256::from_u128(4 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(4 * POS_SCALE);
    engine.stored_pos_count_long = 1;

    let k_before = engine.adl_coeff_long;
    let a_before = engine.adl_mult_long;

    let d = U256::ZERO;
    let q_close = U256::from_u128(POS_SCALE);

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(engine.adl_coeff_long == k_before, "K must be unchanged when D == 0");
    assert!(engine.adl_mult_long < a_before, "A must shrink");
    assert!(engine.oi_eff_long_q == U256::from_u128(3 * POS_SCALE));
}

// ============================================================================
// T11.49: pure_pnl_bankruptcy_path
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn t11_49_pure_pnl_bankruptcy_path() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = POS_SCALE;
    engine.adl_coeff_long = I256::ZERO;
    engine.oi_eff_long_q = U256::from_u128(2 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(2 * POS_SCALE);
    engine.stored_pos_count_long = 1;

    let a_before = engine.adl_mult_long;
    let k_before = engine.adl_coeff_long;

    let d = U256::from_u128(1_000);
    let q_close = U256::ZERO;

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(engine.adl_mult_long == a_before, "A must be unchanged for pure PnL bankruptcy");
    assert!(engine.adl_coeff_long != k_before, "K must change when D > 0");
    assert!(engine.oi_eff_long_q == U256::from_u128(2 * POS_SCALE));
}

// ============================================================================
// T11.53: keeper_crank_quiesces_after_pending_reset
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn t11_53_keeper_crank_quiesces_after_pending_reset() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.last_oracle_price = 100;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 100;
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.adl_epoch_long = 0;
    engine.adl_epoch_short = 0;

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    let c = engine.add_user(0).unwrap();

    engine.deposit(a, 1, 100, 0).unwrap();
    engine.accounts[a as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = I256::ZERO;
    engine.accounts[a as usize].adl_epoch_snap = 0;

    engine.deposit(b, 10_000_000, 100, 0).unwrap();
    engine.accounts[b as usize].position_basis_q = I256::from_i128(-(POS_SCALE as i128));
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = I256::ZERO;
    engine.accounts[b as usize].adl_epoch_snap = 0;

    engine.deposit(c, 10_000_000, 100, 0).unwrap();
    engine.accounts[c as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[c as usize].adl_a_basis = ADL_ONE;
    engine.accounts[c as usize].adl_k_snap = I256::ZERO;
    engine.accounts[c as usize].adl_epoch_snap = 0;

    engine.stored_pos_count_long = 2;
    engine.stored_pos_count_short = 1;
    engine.oi_eff_long_q = U256::from_u128(2 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);

    engine.adl_coeff_long = I256::from_i128(-((ADL_ONE as i128) * 1000));

    let c_cap_before = engine.accounts[c as usize].capital.get();
    let c_pnl_before = engine.accounts[c as usize].pnl;
    let c_k_snap_before = engine.accounts[c as usize].adl_k_snap;
    let c_basis_before = engine.accounts[c as usize].position_basis_q;

    let result = engine.keeper_crank(a, 1, 100, 0);
    assert!(result.is_ok());

    assert!(engine.accounts[c as usize].capital.get() == c_cap_before);
    assert!(engine.accounts[c as usize].pnl == c_pnl_before);
    assert!(engine.accounts[c as usize].adl_k_snap == c_k_snap_before);
    assert!(engine.accounts[c as usize].position_basis_q == c_basis_before);
}

// ============================================================================
// NEW: proof_drain_only_to_reset_progress
// ============================================================================

/// DrainOnly side with OI=0 → schedule_resets fires
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_drain_only_to_reset_progress() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    // Long side: DrainOnly, OI = 0, no stored positions
    engine.side_mode_long = SideMode::DrainOnly;
    engine.oi_eff_long_q = U256::ZERO;
    engine.oi_eff_short_q = U256::ZERO;
    engine.stored_pos_count_long = 0;
    engine.stored_pos_count_short = 0;

    let result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(result.is_ok());

    // With OI=0 on both sides, stored_pos_count=0, the bilateral-empty
    // guard should fire (trivially 0 <= 0) and schedule resets
    assert!(ctx.pending_reset_long, "DrainOnly with OI=0 must schedule reset for progress");
    assert!(ctx.pending_reset_short, "both sides must get reset");
}
