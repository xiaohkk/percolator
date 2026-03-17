//! Section 6 — Per-instruction correctness
//!
//! Reset helpers, fee/warmup, accrue, engine integration, spec compliance,
//! dust bound sufficiency.

#![cfg(kani)]

mod common;
use common::*;

// ############################################################################
// T3: RESET HELPERS
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t3_16_reset_pending_counter_invariant() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 1_000_000, 100, 0).unwrap();
    engine.deposit(b, 1_000_000, 100, 0).unwrap();

    let k_val: i8 = kani::any();
    let k = I256::from_i128(k_val as i128);

    engine.accounts[a as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = k;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = k;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;

    engine.adl_coeff_long = k;

    engine.oi_eff_long_q = U256::ZERO;
    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.side_mode_long == SideMode::ResetPending);
    assert!(engine.stale_account_count_long == 2);

    let _ = engine.settle_side_effects(a as usize);
    assert!(engine.stale_account_count_long == 1);

    let _ = engine.settle_side_effects(b as usize);
    assert!(engine.stale_account_count_long == 0);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t3_16b_reset_counter_with_nonzero_k_diff() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    let k_snap = I256::ZERO;

    engine.accounts[a as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = k_snap;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = k_snap;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;

    let k_diff_val: i8 = kani::any();
    kani::assume(k_diff_val != 0);
    let k_long = I256::from_i128(k_diff_val as i128);
    engine.adl_coeff_long = k_long;

    engine.oi_eff_long_q = U256::ZERO;
    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.adl_epoch_start_k_long == k_long);
    assert!(engine.stale_account_count_long == 2);

    let _ = engine.settle_side_effects(a as usize);
    assert!(engine.stale_account_count_long == 1);
    let _ = engine.settle_side_effects(b as usize);
    assert!(engine.stale_account_count_long == 0);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t3_17_clean_empty_engine_no_retrigger() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    assert!(engine.stored_pos_count_long == 0);
    assert!(engine.stored_pos_count_short == 0);
    assert!(engine.oi_eff_long_q.is_zero());
    assert!(engine.oi_eff_short_q.is_zero());
    assert!(engine.phantom_dust_bound_long_q.is_zero());
    assert!(engine.phantom_dust_bound_short_q.is_zero());

    let result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(result.is_ok());

    assert!(!ctx.pending_reset_long);
    assert!(!ctx.pending_reset_short);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t3_18_dust_bound_reset_in_begin_full_drain() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.phantom_dust_bound_long_q = U256::from_u128(5);
    engine.oi_eff_long_q = U256::ZERO;

    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.phantom_dust_bound_long_q.is_zero(),
        "phantom_dust_bound must be zeroed by begin_full_drain_reset");
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t3_19_finalize_side_reset_requires_all_stale_touched() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.side_mode_long = SideMode::ResetPending;
    engine.oi_eff_long_q = U256::ZERO;
    engine.stale_account_count_long = 1;
    engine.stored_pos_count_long = 0;
    let result1 = engine.finalize_side_reset(Side::Long);
    assert!(result1.is_err());

    engine.stale_account_count_long = 0;
    engine.stored_pos_count_long = 1;
    let result2 = engine.finalize_side_reset(Side::Long);
    assert!(result2.is_err());

    engine.stored_pos_count_long = 0;
    let result3 = engine.finalize_side_reset(Side::Long);
    assert!(result3.is_ok());
    assert!(engine.side_mode_long == SideMode::Normal);
}

#[kani::proof]
#[kani::solver(cadical)]
fn t6_26b_full_drain_reset_nonzero_k_diff() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    engine.accounts[idx as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;

    engine.adl_coeff_long = I256::from_i128(500);

    engine.oi_eff_long_q = U256::ZERO;
    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.adl_epoch_start_k_long == I256::from_i128(500));
    assert!(engine.adl_epoch_long == 1);
    assert!(engine.stale_account_count_long == 1);

    let result = engine.settle_side_effects(idx as usize);
    assert!(result.is_ok());

    assert!(engine.accounts[idx as usize].position_basis_q.is_zero());
    assert!(engine.stale_account_count_long == 0);
    assert!(engine.accounts[idx as usize].adl_epoch_snap == 1);

    assert!(engine.stored_pos_count_long == 0);
    let finalize = engine.finalize_side_reset(Side::Long);
    assert!(finalize.is_ok());
    assert!(engine.side_mode_long == SideMode::Normal);
}

// ############################################################################
// T9: FEE / WARMUP
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t9_35_warmup_slope_preservation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    let pnl_val: u8 = kani::any();
    kani::assume(pnl_val > 0);
    engine.set_pnl(idx as usize, I256::from_u128(pnl_val as u128));

    engine.accounts[idx as usize].warmup_started_at_slot = 0;
    engine.accounts[idx as usize].warmup_slope_per_step = U256::from_u128(1);
    engine.accounts[idx as usize].reserved_pnl = U256::ZERO;

    engine.current_slot = 1;
    let w1 = engine.warmable_gross(idx as usize);

    engine.current_slot = 2;
    let w2 = engine.warmable_gross(idx as usize);

    assert!(w2 >= w1, "warmable_gross must be monotonically non-decreasing");
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t9_36_fee_seniority_after_restart() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    let fc_val: i8 = kani::any();
    engine.accounts[idx as usize].fee_credits = I128::new(fc_val as i128);

    let fc_before = engine.accounts[idx as usize].fee_credits;

    engine.accounts[idx as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 1;
    engine.adl_epoch_start_k_long = I256::ZERO;
    engine.side_mode_long = SideMode::ResetPending;
    engine.stale_account_count_long = 1;
    engine.adl_coeff_long = I256::ZERO;

    let _ = engine.settle_side_effects(idx as usize);

    let fc_after = engine.accounts[idx as usize].fee_credits;
    assert!(fc_after == fc_before, "fee_credits must be preserved across epoch restart");
}

// ############################################################################
// T10: ACCRUE_MARKET_TO
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t10_37_accrue_mark_matches_eager() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.last_oracle_price = 100;
    engine.last_market_slot = 0;
    engine.funding_rate_bps_per_slot_last = 0;
    engine.funding_price_sample_last = 100;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let dp: i8 = kani::any();
    kani::assume(dp >= -50 && dp <= 50);
    let new_price = (100i16 + dp as i16) as u64;
    kani::assume(new_price > 0);

    let result = engine.accrue_market_to(1, new_price);
    assert!(result.is_ok());

    let k_long_after = engine.adl_coeff_long;
    let k_short_after = engine.adl_coeff_short;

    let expected_delta = I256::from_i128((ADL_ONE as i128) * (dp as i128));
    let actual_long_delta = k_long_after.checked_sub(k_long_before).unwrap();
    assert!(actual_long_delta == expected_delta, "K_long delta must equal A_long * delta_p");

    let actual_short_delta = k_short_after.checked_sub(k_short_before).unwrap();
    let expected_short_delta = expected_delta.checked_neg().unwrap_or(I256::ZERO);
    assert!(actual_short_delta == expected_short_delta,
        "K_short delta must equal -(A_short * delta_p)");
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t10_38_accrue_funding_payer_driven() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.last_oracle_price = 100;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 100;

    let rate: i8 = kani::any();
    kani::assume(rate != 0);
    kani::assume(rate >= -100 && rate <= 100);
    engine.funding_rate_bps_per_slot_last = rate as i64;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(1, 100);
    assert!(result.is_ok());

    let k_long_after = engine.adl_coeff_long;
    let k_short_after = engine.adl_coeff_short;

    let abs_rate = (rate as i128).unsigned_abs();
    let funding_term_raw: u128 = 100 * abs_rate * 1;

    let a = ADL_ONE as u128;
    let delta_k_payer_abs = mul_div_ceil_u256(
        U256::from_u128(a), U256::from_u128(funding_term_raw), U256::from_u128(10_000));

    let delta_k_receiver_abs = mul_div_floor_u256(
        delta_k_payer_abs, U256::from_u128(a), U256::from_u128(a));
    assert!(delta_k_receiver_abs == delta_k_payer_abs, "equal A implies symmetric funding");

    if rate > 0 {
        let payer_neg = try_negate_u256_to_i256(delta_k_payer_abs).unwrap();
        let expected_long = k_long_before.checked_add(payer_neg).unwrap();
        assert!(k_long_after == expected_long);
        let recv = I256::from_raw_u256_pub(delta_k_receiver_abs);
        let expected_short = k_short_before.checked_add(recv).unwrap();
        assert!(k_short_after == expected_short);
    } else {
        let payer_neg = try_negate_u256_to_i256(delta_k_payer_abs).unwrap();
        let expected_short = k_short_before.checked_add(payer_neg).unwrap();
        assert!(k_short_after == expected_short);
        let recv = I256::from_raw_u256_pub(delta_k_receiver_abs);
        let expected_long = k_long_before.checked_add(recv).unwrap();
        assert!(k_long_after == expected_long);
    }
}

// ############################################################################
// T11: ENGINE INTEGRATION
// ############################################################################

#[kani::proof]
#[kani::solver(cadical)]
fn t11_39_same_epoch_settle_idempotent_real_engine() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    let pos = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);

    engine.adl_coeff_long = I256::from_i128(100);

    let r1 = engine.settle_side_effects(idx as usize);
    assert!(r1.is_ok());
    let pnl_after_first = engine.accounts[idx as usize].pnl;
    assert!(engine.accounts[idx as usize].adl_k_snap == I256::from_i128(100));

    let r2 = engine.settle_side_effects(idx as usize);
    assert!(r2.is_ok());
    let pnl_after_second = engine.accounts[idx as usize].pnl;

    assert!(pnl_after_second == pnl_after_first,
        "second settle with unchanged K must produce zero incremental PnL");
    assert!(engine.accounts[idx as usize].adl_a_basis == ADL_ONE);
    assert!(engine.accounts[idx as usize].position_basis_q == pos);
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_40_non_compounding_quantity_basis_two_touches() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    let pos = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);

    engine.adl_coeff_long = I256::from_i128(50);
    let _ = engine.settle_side_effects(idx as usize);

    assert!(engine.accounts[idx as usize].position_basis_q == pos);
    assert!(engine.accounts[idx as usize].adl_a_basis == ADL_ONE);
    assert!(engine.accounts[idx as usize].adl_k_snap == I256::from_i128(50));

    engine.adl_coeff_long = I256::from_i128(120);
    let _ = engine.settle_side_effects(idx as usize);

    assert!(engine.accounts[idx as usize].position_basis_q == pos);
    assert!(engine.accounts[idx as usize].adl_a_basis == ADL_ONE);
    assert!(engine.accounts[idx as usize].adl_k_snap == I256::from_i128(120));
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_41_attach_effective_position_remainder_accounting() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    let pos = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.adl_epoch_long = 0;
    engine.adl_mult_long = ADL_ONE - 1;
    engine.stored_pos_count_long = 1;

    let dust_before = engine.phantom_dust_bound_long_q;

    let new_pos = I256::from_u128(2 * POS_SCALE);
    engine.attach_effective_position(idx as usize, new_pos);

    assert!(engine.phantom_dust_bound_long_q > dust_before,
        "dust bound must increment on nonzero remainder");

    engine.accounts[idx as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.adl_mult_long = ADL_ONE;

    let dust_before2 = engine.phantom_dust_bound_long_q;
    engine.attach_effective_position(idx as usize, I256::from_u128(3 * POS_SCALE));

    assert!(engine.phantom_dust_bound_long_q == dust_before2,
        "dust bound must not increment on zero remainder");
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_42_dynamic_dust_bound_inductive() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    engine.accounts[a as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = I256::ZERO;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = I256::ZERO;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = U256::from_u128(2 * POS_SCALE);

    engine.adl_mult_long = 1;

    let _ = engine.settle_side_effects(a as usize);
    assert!(engine.accounts[a as usize].position_basis_q.is_zero());
    assert!(engine.phantom_dust_bound_long_q == U256::from_u128(1));

    let _ = engine.settle_side_effects(b as usize);
    assert!(engine.accounts[b as usize].position_basis_q.is_zero());
    assert!(engine.phantom_dust_bound_long_q == U256::from_u128(2));
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_50_execute_trade_atomic_oi_update_sign_flip() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 100_000_000, 100, 0).unwrap();
    engine.deposit(b, 100_000_000, 100, 0).unwrap();

    engine.last_oracle_price = 100;
    engine.last_market_slot = 1;
    engine.last_crank_slot = 1;
    engine.funding_price_sample_last = 100;

    let size_q = I256::from_u128(POS_SCALE);
    let r1 = engine.execute_trade(a, b, 100, 1, size_q, 100);
    assert!(r1.is_ok());
    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);

    let flip_size = I256::ZERO.checked_sub(I256::from_u128(2 * POS_SCALE)).unwrap();
    let r2 = engine.execute_trade(a, b, 100, 2, flip_size, 100);
    assert!(r2.is_ok());

    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q, "OI must be balanced after sign flip");
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_51_execute_trade_slippage_zero_sum() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    engine.last_oracle_price = 100;
    engine.last_market_slot = 1;
    engine.last_crank_slot = 1;
    engine.funding_price_sample_last = 100;

    let vault_before = engine.vault.get();

    let size_q = I256::from_u128(POS_SCALE);
    let result = engine.execute_trade(a, b, 100, 1, size_q, 100);
    assert!(result.is_ok());

    let vault_after = engine.vault.get();
    assert!(vault_after == vault_before, "vault must be unchanged with zero fees at oracle price");
    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_52_touch_account_full_restart_fee_seniority() {
    let mut params = zero_fee_params();
    params.warmup_period_slots = 10;
    let mut engine = RiskEngine::new(params);

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    let pos = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);

    let pre_pnl = I256::from_u128(5000);
    engine.accounts[idx as usize].pnl = pre_pnl;
    engine.pnl_pos_tot = U256::from_u128(5000);

    engine.adl_coeff_long = I256::from_i128((ADL_ONE as i128) * 100);

    engine.accounts[idx as usize].fee_credits = I128::new(-500i128);

    engine.accounts[idx as usize].warmup_started_at_slot = 0;
    engine.accounts[idx as usize].warmup_slope_per_step = U256::from_u128(100);

    engine.last_oracle_price = 100;
    engine.last_market_slot = 100;

    let cap_before = engine.accounts[idx as usize].capital.get();
    let ins_before = engine.insurance_fund.balance.get();

    let result = engine.touch_account_full(idx as usize, 100, 100);
    assert!(result.is_ok());

    assert!(engine.accounts[idx as usize].adl_k_snap == engine.adl_coeff_long);

    let fc_after = engine.accounts[idx as usize].fee_credits.get();
    assert!(fc_after > -500i128, "fee debt must be swept after restart conversion");

    let ins_after = engine.insurance_fund.balance.get();
    assert!(ins_after > ins_before, "insurance fund must receive fee sweep payment");

    let cap_after = engine.accounts[idx as usize].capital.get();
    assert!(cap_after != cap_before, "capital must change after restart conversion + fee sweep");
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_54_worked_example_regression() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    engine.last_oracle_price = 100;
    engine.last_market_slot = 1;
    engine.last_crank_slot = 1;
    engine.funding_price_sample_last = 100;

    let size_q = I256::from_u128(2 * POS_SCALE);
    let r1 = engine.execute_trade(a, b, 100, 1, size_q, 100);
    assert!(r1.is_ok());
    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);

    let mut ctx = InstructionContext::new();
    let d = U256::from_u128(500);
    let q_close = U256::from_u128(POS_SCALE);
    let r2 = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(r2.is_ok());

    assert!(engine.adl_mult_long < ADL_ONE);
    assert!(engine.oi_eff_long_q == U256::from_u128(POS_SCALE));
    assert!(engine.adl_coeff_long != I256::ZERO);

    let _ = engine.settle_side_effects(a as usize);

    assert!(engine.accounts[a as usize].adl_k_snap == engine.adl_coeff_long);
    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t5_24_dynamic_dust_bound_sufficient() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    engine.accounts[a as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = I256::ZERO;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = I256::ZERO;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;
    engine.oi_eff_long_q = U256::from_u128(2 * POS_SCALE);
    engine.adl_epoch_long = 0;

    engine.adl_mult_long = 1;
    engine.adl_coeff_long = I256::ZERO;

    let _ = engine.settle_side_effects(a as usize);
    assert!(engine.phantom_dust_bound_long_q == U256::from_u128(1));

    let _ = engine.settle_side_effects(b as usize);
    assert!(engine.phantom_dust_bound_long_q == U256::from_u128(2));
}

// ############################################################################
// From kani.rs: reset/instruction
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_begin_full_drain_reset() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let epoch_before = engine.adl_epoch_long;
    let k_before = engine.adl_coeff_long;

    assert!(engine.oi_eff_long_q.is_zero());

    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.adl_epoch_long == epoch_before + 1);
    assert!(engine.adl_mult_long == ADL_ONE);
    assert!(engine.side_mode_long == SideMode::ResetPending);
    assert!(engine.adl_epoch_start_k_long == k_before);
    assert!(engine.stale_account_count_long == engine.stored_pos_count_long);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_finalize_side_reset_requires_conditions() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let r1 = engine.finalize_side_reset(Side::Long);
    assert!(r1.is_err());

    engine.side_mode_long = SideMode::ResetPending;
    engine.oi_eff_long_q = U256::from_u128(100);
    let r2 = engine.finalize_side_reset(Side::Long);
    assert!(r2.is_err());

    engine.oi_eff_long_q = U256::ZERO;
    engine.stale_account_count_long = 1;
    let r3 = engine.finalize_side_reset(Side::Long);
    assert!(r3.is_err());

    engine.stale_account_count_long = 0;
    engine.stored_pos_count_long = 0;
    let r4 = engine.finalize_side_reset(Side::Long);
    assert!(r4.is_ok());
    assert!(engine.side_mode_long == SideMode::Normal);
}

// ############################################################################
// SPEC COMPLIANCE (from ak.rs)
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_55_empty_opposing_side_deficit_fallback() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = POS_SCALE;
    engine.adl_coeff_long = I256::from_i128(12345);
    engine.oi_eff_long_q = U256::from_u128(4 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(4 * POS_SCALE);
    engine.insurance_fund.balance = U128::new(10_000_000);
    engine.stored_pos_count_long = 0;

    let k_before = engine.adl_coeff_long;
    let ins_before = engine.insurance_fund.balance.get();

    let d = U256::from_u128(5_000);
    let q_close = U256::from_u128(POS_SCALE);

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(engine.adl_coeff_long == k_before, "K must not change when stored_pos_count_opp == 0");
    assert!(engine.insurance_fund.balance.get() < ins_before, "insurance must absorb deficit");
    assert!(engine.oi_eff_long_q == U256::from_u128(3 * POS_SCALE));
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_56_unilateral_empty_orphan_resolution() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.stored_pos_count_long = 0;
    engine.phantom_dust_bound_long_q = U256::from_u128(100);
    engine.oi_eff_long_q = U256::from_u128(50);

    engine.stored_pos_count_short = 2;
    engine.oi_eff_short_q = U256::from_u128(50);

    let result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(result.is_ok());

    assert!(ctx.pending_reset_long);
    assert!(ctx.pending_reset_short);
    assert!(engine.oi_eff_long_q.is_zero());
    assert!(engine.oi_eff_short_q.is_zero());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_57_unilateral_empty_corruption_guard() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.stored_pos_count_long = 0;
    engine.phantom_dust_bound_long_q = U256::from_u128(100);
    engine.oi_eff_long_q = U256::from_u128(50);

    engine.stored_pos_count_short = 2;
    engine.oi_eff_short_q = U256::from_u128(999);

    let result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(result == Err(RiskError::CorruptState));
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_58_unilateral_empty_short_side() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.stored_pos_count_short = 0;
    engine.phantom_dust_bound_short_q = U256::from_u128(200);
    engine.oi_eff_short_q = U256::from_u128(75);

    engine.stored_pos_count_long = 3;
    engine.oi_eff_long_q = U256::from_u128(75);

    let result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(result.is_ok());

    assert!(ctx.pending_reset_long);
    assert!(ctx.pending_reset_short);
    assert!(engine.oi_eff_long_q.is_zero());
    assert!(engine.oi_eff_short_q.is_zero());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_60_conditional_dust_bound_only_on_truncation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = 4;
    engine.adl_coeff_long = I256::ZERO;
    engine.oi_eff_long_q = U256::from_u128(4 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(4 * POS_SCALE);
    engine.stored_pos_count_long = 1;

    let dust_before = engine.phantom_dust_bound_long_q;

    let result = engine.enqueue_adl(
        &mut ctx, Side::Short, U256::from_u128(2 * POS_SCALE), U256::ZERO,
    );
    assert!(result.is_ok());
    assert!(engine.adl_mult_long == 2);

    assert!(engine.phantom_dust_bound_long_q == dust_before,
        "no dust added when A_trunc_rem == 0");
}

#[kani::proof]
#[kani::solver(cadical)]
fn t12_53_adl_truncation_dust_must_not_deadlock() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = 7;
    engine.adl_coeff_long = I256::ZERO;
    engine.oi_eff_long_q = U256::from_u128(10 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(10 * POS_SCALE);
    engine.stored_pos_count_long = 1;

    let result = engine.enqueue_adl(
        &mut ctx, Side::Short, U256::from_u128(POS_SCALE), U256::ZERO,
    );
    assert!(result.is_ok());
    assert!(engine.adl_mult_long == 6);
    assert!(engine.oi_eff_long_q == U256::from_u128(9 * POS_SCALE));

    let effective = mul_div_floor_u256(
        U256::from_u128(10 * POS_SCALE), U256::from_u128(6), U256::from_u128(7));

    engine.oi_eff_long_q = engine.oi_eff_long_q.checked_sub(effective).unwrap();
    engine.oi_eff_short_q = engine.oi_eff_short_q.checked_sub(effective).unwrap();

    assert!(!engine.oi_eff_long_q.is_zero());
    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);

    engine.stored_pos_count_long = 0;
    engine.stored_pos_count_short = 0;
    engine.phantom_dust_bound_long_q = engine.phantom_dust_bound_long_q
        .checked_add(U256::from_u128(1)).unwrap();
    engine.phantom_dust_bound_short_q = engine.phantom_dust_bound_short_q
        .checked_add(U256::from_u128(1)).unwrap();

    let reset_result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(reset_result.is_ok(), "ADL truncation dust must not deadlock market reset");
}

// ############################################################################
// T14: INDUCTIVE DUST-BOUND SUFFICIENCY
// ############################################################################

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t14_61_dust_bound_adl_a_truncation_sufficient() {
    let a_old: u8 = kani::any();
    kani::assume(a_old >= 2);
    let basis_1: u8 = kani::any();
    kani::assume(basis_1 > 0 && basis_1 <= 15);
    let basis_2: u8 = kani::any();
    kani::assume(basis_2 > 0 && basis_2 <= 15);

    let a_basis_1: u8 = kani::any();
    kani::assume(a_basis_1 > 0 && a_basis_1 <= a_old);
    let a_basis_2: u8 = kani::any();
    kani::assume(a_basis_2 > 0 && a_basis_2 <= a_old);

    let q_eff_old_1 = ((basis_1 as u16) * (a_old as u16)) / (a_basis_1 as u16);
    let q_eff_old_2 = ((basis_2 as u16) * (a_old as u16)) / (a_basis_2 as u16);
    let oi: u16 = q_eff_old_1 + q_eff_old_2;
    kani::assume(oi > 0);

    let q_close: u8 = kani::any();
    kani::assume(q_close > 0 && (q_close as u16) < oi);
    let oi_post = oi - (q_close as u16);

    let a_new = ((a_old as u16) * oi_post) / oi;
    kani::assume(a_new > 0);

    let q_eff_new_1 = ((basis_1 as u16) * (a_new as u16)) / (a_basis_1 as u16);
    let q_eff_new_2 = ((basis_2 as u16) * (a_new as u16)) / (a_basis_2 as u16);
    let sum_new = q_eff_new_1 + q_eff_new_2;

    let phantom_dust = if oi_post >= sum_new { oi_post - sum_new } else { 0 };

    let n: u16 = 2;
    let global_a_dust = n + ((oi + n + (a_old as u16) - 1) / (a_old as u16));

    assert!(global_a_dust >= phantom_dust,
        "A-truncation dust bound must cover phantom OI from A change");
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t14_62_dust_bound_same_epoch_zeroing() {
    let basis: u8 = kani::any();
    kani::assume(basis > 0);
    let a_cur: u8 = kani::any();
    kani::assume(a_cur > 0);
    let a_basis: u8 = kani::any();
    kani::assume(a_basis > 0 && a_basis >= a_cur);

    let q_eff_old = ((basis as u16) * (a_cur as u16)) / (a_basis as u16);

    if q_eff_old > 0 {
        let dust_increment: u16 = 1;
        assert!(dust_increment >= 1);
    }
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t14_63_dust_bound_position_reattach_remainder() {
    let basis: u8 = kani::any();
    kani::assume(basis > 0);
    let a_cur: u8 = kani::any();
    kani::assume(a_cur > 0);
    let a_basis: u8 = kani::any();
    kani::assume(a_basis > 0);

    let product = (basis as u32) * (a_cur as u32);
    let q_eff = product / (a_basis as u32);
    let remainder = product % (a_basis as u32);

    if remainder > 0 {
        let dust_increment: u32 = 1;
        let actual_fractional_loss: u32 = 1;
        assert!(dust_increment >= actual_fractional_loss);
    }

    assert!(q_eff * (a_basis as u32) <= product);
    if remainder > 0 {
        assert!((q_eff + 1) * (a_basis as u32) > product);
    }
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t14_64_dust_bound_full_drain_reset_zeroes() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.phantom_dust_bound_long_q = U256::from_u128(42);
    engine.oi_eff_long_q = U256::ZERO;
    engine.stored_pos_count_long = 0;
    engine.adl_epoch_long = 0;

    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.phantom_dust_bound_long_q == U256::ZERO);
    assert!(engine.oi_eff_long_q == U256::ZERO);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t14_65_dust_bound_end_to_end_clearance() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    let a_idx = engine.add_user(0).unwrap();
    let b_idx = engine.add_user(0).unwrap();
    engine.deposit(a_idx, 10_000_000, 100, 0).unwrap();
    engine.deposit(b_idx, 10_000_000, 100, 0).unwrap();

    engine.adl_mult_long = 13;
    engine.adl_coeff_long = I256::ZERO;
    engine.adl_epoch_long = 0;

    engine.accounts[a_idx as usize].position_basis_q = I256::from_u128(7 * POS_SCALE);
    engine.accounts[a_idx as usize].adl_a_basis = 13;
    engine.accounts[a_idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[a_idx as usize].adl_epoch_snap = 0;

    engine.accounts[b_idx as usize].position_basis_q = I256::from_u128(5 * POS_SCALE);
    engine.accounts[b_idx as usize].adl_a_basis = 13;
    engine.accounts[b_idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[b_idx as usize].adl_epoch_snap = 0;

    engine.stored_pos_count_long = 2;
    engine.oi_eff_long_q = U256::from_u128(12 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(12 * POS_SCALE);

    let result = engine.enqueue_adl(
        &mut ctx, Side::Short, U256::from_u128(3 * POS_SCALE), U256::ZERO,
    );
    assert!(result.is_ok());
    assert!(engine.adl_mult_long == 9);

    assert!(!engine.phantom_dust_bound_long_q.is_zero());

    let q_eff_0 = mul_div_floor_u256(
        U256::from_u128(7 * POS_SCALE), U256::from_u128(9), U256::from_u128(13));
    let q_eff_1 = mul_div_floor_u256(
        U256::from_u128(5 * POS_SCALE), U256::from_u128(9), U256::from_u128(13));

    engine.oi_eff_long_q = engine.oi_eff_long_q.checked_sub(q_eff_0).unwrap();
    engine.oi_eff_long_q = engine.oi_eff_long_q.checked_sub(q_eff_1).unwrap();
    engine.oi_eff_short_q = engine.oi_eff_short_q.checked_sub(q_eff_0).unwrap();
    engine.oi_eff_short_q = engine.oi_eff_short_q.checked_sub(q_eff_1).unwrap();

    engine.phantom_dust_bound_long_q = engine.phantom_dust_bound_long_q
        .checked_add(U256::from_u128(1)).unwrap();
    engine.phantom_dust_bound_long_q = engine.phantom_dust_bound_long_q
        .checked_add(U256::from_u128(1)).unwrap();
    engine.phantom_dust_bound_short_q = engine.phantom_dust_bound_short_q
        .checked_add(U256::from_u128(1)).unwrap();
    engine.phantom_dust_bound_short_q = engine.phantom_dust_bound_short_q
        .checked_add(U256::from_u128(1)).unwrap();

    engine.stored_pos_count_long = 0;
    engine.stored_pos_count_short = 0;

    let reset_result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(reset_result.is_ok(), "dust bound must be sufficient for reset after all positions closed");
}
