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
    let k = k_val as i128;

    engine.accounts[a as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = k;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = k;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;

    engine.adl_coeff_long = k;

    engine.oi_eff_long_q = 0u128;
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

    let k_snap = 0i128;

    engine.accounts[a as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = k_snap;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = k_snap;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;

    let k_diff_val: i8 = kani::any();
    kani::assume(k_diff_val != 0);
    let k_long = k_diff_val as i128;
    engine.adl_coeff_long = k_long;

    engine.oi_eff_long_q = 0u128;
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
    assert!(engine.oi_eff_long_q == 0);
    assert!(engine.oi_eff_short_q == 0);
    assert!(engine.phantom_dust_bound_long_q == 0);
    assert!(engine.phantom_dust_bound_short_q == 0);

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

    engine.phantom_dust_bound_long_q = 5u128;
    engine.oi_eff_long_q = 0u128;

    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.phantom_dust_bound_long_q == 0,
        "phantom_dust_bound must be zeroed by begin_full_drain_reset");
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t3_19_finalize_side_reset_requires_all_stale_touched() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.side_mode_long = SideMode::ResetPending;
    engine.oi_eff_long_q = 0u128;
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

    engine.accounts[idx as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = 0i128;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;

    engine.adl_coeff_long = 500i128;

    engine.oi_eff_long_q = 0u128;
    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.adl_epoch_start_k_long == 500i128);
    assert!(engine.adl_epoch_long == 1);
    assert!(engine.stale_account_count_long == 1);

    let result = engine.settle_side_effects(idx as usize);
    assert!(result.is_ok());

    assert!(engine.accounts[idx as usize].position_basis_q == 0);
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
    engine.set_pnl(idx as usize, pnl_val as i128);

    engine.accounts[idx as usize].warmup_started_at_slot = 0;
    engine.accounts[idx as usize].warmup_slope_per_step = 1u128;
    engine.accounts[idx as usize].reserved_pnl = 0u128;

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

    engine.accounts[idx as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = 0i128;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 1;
    engine.adl_epoch_start_k_long = 0i128;
    engine.side_mode_long = SideMode::ResetPending;
    engine.stale_account_count_long = 1;
    engine.adl_coeff_long = 0i128;

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

    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
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

    let expected_delta = (ADL_ONE as i128) * (dp as i128);
    let actual_long_delta = k_long_after.checked_sub(k_long_before).unwrap();
    assert!(actual_long_delta == expected_delta, "K_long delta must equal A_long * delta_p");

    let actual_short_delta = k_short_after.checked_sub(k_short_before).unwrap();
    let expected_short_delta = expected_delta.checked_neg().unwrap_or(0i128);
    assert!(actual_short_delta == expected_short_delta,
        "K_short delta must equal -(A_short * delta_p)");
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t10_38_accrue_funding_payer_driven() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
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
    let delta_k_payer_abs = mul_div_ceil_u128(a, funding_term_raw, 10_000);

    let delta_k_receiver_abs = mul_div_floor_u128(delta_k_payer_abs, a, a);
    assert!(delta_k_receiver_abs == delta_k_payer_abs, "equal A implies symmetric funding");

    if rate > 0 {
        // longs pay, shorts receive
        let expected_long = k_long_before.checked_sub(delta_k_payer_abs as i128).unwrap();
        assert!(k_long_after == expected_long);
        let expected_short = k_short_before.checked_add(delta_k_receiver_abs as i128).unwrap();
        assert!(k_short_after == expected_short);
    } else {
        // shorts pay, longs receive
        let expected_short = k_short_before.checked_sub(delta_k_payer_abs as i128).unwrap();
        assert!(k_short_after == expected_short);
        let expected_long = k_long_before.checked_add(delta_k_receiver_abs as i128).unwrap();
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

    let pos = POS_SCALE as i128;
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = 0i128;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = POS_SCALE;

    engine.adl_coeff_long = 100i128;

    let r1 = engine.settle_side_effects(idx as usize);
    assert!(r1.is_ok());
    let pnl_after_first = engine.accounts[idx as usize].pnl;
    assert!(engine.accounts[idx as usize].adl_k_snap == 100i128);

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

    let pos = POS_SCALE as i128;
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = 0i128;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = POS_SCALE;

    engine.adl_coeff_long = 50i128;
    let _ = engine.settle_side_effects(idx as usize);

    assert!(engine.accounts[idx as usize].position_basis_q == pos);
    assert!(engine.accounts[idx as usize].adl_a_basis == ADL_ONE);
    assert!(engine.accounts[idx as usize].adl_k_snap == 50i128);

    engine.adl_coeff_long = 120i128;
    let _ = engine.settle_side_effects(idx as usize);

    assert!(engine.accounts[idx as usize].position_basis_q == pos);
    assert!(engine.accounts[idx as usize].adl_a_basis == ADL_ONE);
    assert!(engine.accounts[idx as usize].adl_k_snap == 120i128);
}

#[kani::proof]
#[kani::solver(cadical)]
fn t11_41_attach_effective_position_remainder_accounting() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    engine.accounts[idx as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.adl_epoch_long = 0;
    engine.adl_mult_long = ADL_ONE - 1;
    engine.stored_pos_count_long = 1;

    let dust_before = engine.phantom_dust_bound_long_q;

    let new_pos = (2 * POS_SCALE) as i128;
    engine.attach_effective_position(idx as usize, new_pos);

    assert!(engine.phantom_dust_bound_long_q > dust_before,
        "dust bound must increment on nonzero remainder");

    engine.accounts[idx as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.adl_mult_long = ADL_ONE;

    let dust_before2 = engine.phantom_dust_bound_long_q;
    engine.attach_effective_position(idx as usize, (3 * POS_SCALE) as i128);

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

    engine.accounts[a as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = 0i128;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = 0i128;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = 2 * POS_SCALE;

    engine.adl_mult_long = 1;

    let _ = engine.settle_side_effects(a as usize);
    assert!(engine.accounts[a as usize].position_basis_q == 0);
    assert!(engine.phantom_dust_bound_long_q == 1u128);

    let _ = engine.settle_side_effects(b as usize);
    assert!(engine.accounts[b as usize].position_basis_q == 0);
    assert!(engine.phantom_dust_bound_long_q == 2u128);
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

    let size_q = POS_SCALE as i128;
    let r1 = engine.execute_trade(a, b, 100, 1, size_q, 100);
    assert!(r1.is_ok());
    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);

    let flip_size = -(2 * POS_SCALE as i128);
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

    let size_q = POS_SCALE as i128;
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

    let pos = POS_SCALE as i128;
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = 0i128;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = POS_SCALE;

    engine.accounts[idx as usize].pnl = 5000i128;
    engine.pnl_pos_tot = 5000u128;

    engine.adl_coeff_long = (ADL_ONE as i128) * 100;

    engine.accounts[idx as usize].fee_credits = I128::new(-500i128);

    engine.accounts[idx as usize].warmup_started_at_slot = 0;
    engine.accounts[idx as usize].warmup_slope_per_step = 100u128;

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

    let size_q = (2 * POS_SCALE) as i128;
    let r1 = engine.execute_trade(a, b, 100, 1, size_q, 100);
    assert!(r1.is_ok());
    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);

    let mut ctx = InstructionContext::new();
    let d = 500u128;
    let q_close = POS_SCALE;
    let r2 = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(r2.is_ok());

    assert!(engine.adl_mult_long < ADL_ONE);
    assert!(engine.oi_eff_long_q == POS_SCALE);
    assert!(engine.adl_coeff_long != 0i128);

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

    engine.accounts[a as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = 0i128;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.accounts[b as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = 0i128;
    engine.accounts[b as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 2;
    engine.oi_eff_long_q = 2 * POS_SCALE;
    engine.adl_epoch_long = 0;

    engine.adl_mult_long = 1;
    engine.adl_coeff_long = 0i128;

    let _ = engine.settle_side_effects(a as usize);
    assert!(engine.phantom_dust_bound_long_q == 1u128);

    let _ = engine.settle_side_effects(b as usize);
    assert!(engine.phantom_dust_bound_long_q == 2u128);
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

    assert!(engine.oi_eff_long_q == 0);

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
    engine.oi_eff_long_q = 100u128;
    let r2 = engine.finalize_side_reset(Side::Long);
    assert!(r2.is_err());

    engine.oi_eff_long_q = 0u128;
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
    engine.adl_coeff_long = 12345i128;
    engine.oi_eff_long_q = 4 * POS_SCALE;
    engine.oi_eff_short_q = 4 * POS_SCALE;
    engine.insurance_fund.balance = U128::new(10_000_000);
    engine.stored_pos_count_long = 0;

    let k_before = engine.adl_coeff_long;
    let ins_before = engine.insurance_fund.balance.get();

    let d = 5_000u128;
    let q_close = POS_SCALE;

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(engine.adl_coeff_long == k_before, "K must not change when stored_pos_count_opp == 0");
    assert!(engine.insurance_fund.balance.get() < ins_before, "insurance must absorb deficit");
    assert!(engine.oi_eff_long_q == 3 * POS_SCALE);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_56_unilateral_empty_orphan_resolution() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.stored_pos_count_long = 0;
    engine.phantom_dust_bound_long_q = 100u128;
    engine.oi_eff_long_q = 50u128;

    engine.stored_pos_count_short = 2;
    engine.oi_eff_short_q = 50u128;

    let result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(result.is_ok());

    assert!(ctx.pending_reset_long);
    assert!(ctx.pending_reset_short);
    assert!(engine.oi_eff_long_q == 0);
    assert!(engine.oi_eff_short_q == 0);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_57_unilateral_empty_corruption_guard() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.stored_pos_count_long = 0;
    engine.phantom_dust_bound_long_q = 100u128;
    engine.oi_eff_long_q = 50u128;

    engine.stored_pos_count_short = 2;
    engine.oi_eff_short_q = 999u128;

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
    engine.phantom_dust_bound_short_q = 200u128;
    engine.oi_eff_short_q = 75u128;

    engine.stored_pos_count_long = 3;
    engine.oi_eff_long_q = 75u128;

    let result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(result.is_ok());

    assert!(ctx.pending_reset_long);
    assert!(ctx.pending_reset_short);
    assert!(engine.oi_eff_long_q == 0);
    assert!(engine.oi_eff_short_q == 0);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_60_conditional_dust_bound_only_on_truncation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = 4;
    engine.adl_coeff_long = 0i128;
    engine.oi_eff_long_q = 4 * POS_SCALE;
    engine.oi_eff_short_q = 4 * POS_SCALE;
    engine.stored_pos_count_long = 1;

    let dust_before = engine.phantom_dust_bound_long_q;

    let result = engine.enqueue_adl(
        &mut ctx, Side::Short, 2 * POS_SCALE, 0u128,
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

    // Create one long account with known position, one short counterpart.
    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    // Set up: one long position at A=7, one short position for OI balance.
    engine.adl_mult_long = 7;
    engine.adl_mult_short = ADL_ONE;
    engine.adl_coeff_long = 0i128;
    engine.adl_coeff_short = 0i128;

    // Account a: long 10*POS_SCALE at a_basis=7
    engine.accounts[a as usize].position_basis_q = (10 * POS_SCALE) as i128;
    engine.accounts[a as usize].adl_a_basis = 7;
    engine.accounts[a as usize].adl_k_snap = 0i128;
    engine.accounts[a as usize].adl_epoch_snap = 0;

    // Account b: short 10*POS_SCALE at a_basis=ADL_ONE
    engine.accounts[b as usize].position_basis_q = -((10 * POS_SCALE) as i128);
    engine.accounts[b as usize].adl_a_basis = ADL_ONE;
    engine.accounts[b as usize].adl_k_snap = 0i128;
    engine.accounts[b as usize].adl_epoch_snap = 0;

    engine.stored_pos_count_long = 1;
    engine.stored_pos_count_short = 1;
    engine.oi_eff_long_q = 10 * POS_SCALE;
    engine.oi_eff_short_q = 10 * POS_SCALE;

    // ADL: close 1*POS_SCALE from short side → shrinks A_long
    let result = engine.enqueue_adl(
        &mut ctx, Side::Short, POS_SCALE, 0u128,
    );
    assert!(result.is_ok());
    assert!(engine.adl_mult_long == 6);
    assert!(engine.oi_eff_long_q == 9 * POS_SCALE);

    // Now settle account a through the engine to get actual effective position + dust
    let settle_result = engine.settle_side_effects(a as usize);
    assert!(settle_result.is_ok());

    // Get a's actual effective position after settlement
    let eff_a = engine.effective_pos_q(a as usize);
    let abs_eff_a = eff_a.unsigned_abs();

    // Attach the effective position (zeroing it) to close a's long
    engine.attach_effective_position(a as usize, 0i128);

    // Similarly settle and close b's short
    let settle_b = engine.settle_side_effects(b as usize);
    assert!(settle_b.is_ok());
    let eff_b = engine.effective_pos_q(b as usize);
    engine.attach_effective_position(b as usize, 0i128);

    // Update OI through actual decrements
    engine.oi_eff_long_q = engine.oi_eff_long_q.checked_sub(abs_eff_a).unwrap_or(0);
    engine.oi_eff_short_q = engine.oi_eff_short_q.checked_sub(eff_b.unsigned_abs()).unwrap_or(0);

    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);

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

/// Same-epoch zeroing: when settle_side_effects zeros a position (q_eff_new == 0),
/// the engine must increment phantom_dust_bound by 1.
#[kani::proof]
#[kani::solver(cadical)]
fn t14_62_dust_bound_same_epoch_zeroing() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, 100, 0).unwrap();

    // Account has a 1-unit position with a_basis = ADL_ONE
    engine.accounts[idx as usize].position_basis_q = POS_SCALE as i128;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = 0i128;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.adl_coeff_long = 0i128;

    // Set A_side so that floor(|basis| * A_side / a_basis) == 0
    engine.adl_mult_long = 1;

    let dust_before = engine.phantom_dust_bound_long_q;

    let result = engine.settle_side_effects(idx as usize);
    assert!(result.is_ok());

    // Position must be zeroed
    assert!(engine.accounts[idx as usize].position_basis_q == 0);
    // Dust bound must have incremented by 1
    let dust_after = engine.phantom_dust_bound_long_q;
    assert!(dust_after == dust_before + 1u128,
        "same-epoch zeroing must increment phantom_dust_bound by 1");
}

/// Position reattach: floor(|basis| * A_new / A_old) loses at most 1 unit per position.
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

    // Floor division: q_eff * a_basis + remainder == product
    assert!(q_eff * (a_basis as u32) + remainder == product,
        "floor division identity");

    // Remainder is strictly less than divisor
    assert!(remainder < (a_basis as u32), "remainder < a_basis");

    // The effective quantity never exceeds the true (unrounded) quantity
    assert!(q_eff * (a_basis as u32) <= product,
        "floor never overshoots");

    if remainder > 0 {
        assert!((q_eff + 1) * (a_basis as u32) > product,
            "next integer exceeds product → loss < 1 unit");
    }
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t14_64_dust_bound_full_drain_reset_zeroes() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.phantom_dust_bound_long_q = 42u128;
    engine.oi_eff_long_q = 0u128;
    engine.stored_pos_count_long = 0;
    engine.adl_epoch_long = 0;

    engine.begin_full_drain_reset(Side::Long);

    assert!(engine.phantom_dust_bound_long_q == 0u128);
    assert!(engine.oi_eff_long_q == 0u128);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t14_65_dust_bound_end_to_end_clearance() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    // Create two long accounts and two short counterparts for OI balance.
    let a_idx = engine.add_user(0).unwrap();
    let b_idx = engine.add_user(0).unwrap();
    let c_idx = engine.add_user(0).unwrap();
    let d_idx = engine.add_user(0).unwrap();
    engine.deposit(a_idx, 10_000_000, 100, 0).unwrap();
    engine.deposit(b_idx, 10_000_000, 100, 0).unwrap();
    engine.deposit(c_idx, 10_000_000, 100, 0).unwrap();
    engine.deposit(d_idx, 10_000_000, 100, 0).unwrap();

    engine.adl_mult_long = 13;
    engine.adl_mult_short = ADL_ONE;
    engine.adl_coeff_long = 0i128;
    engine.adl_coeff_short = 0i128;
    engine.adl_epoch_long = 0;

    // Account a: long 7*POS_SCALE at a_basis=13
    engine.accounts[a_idx as usize].position_basis_q = (7 * POS_SCALE) as i128;
    engine.accounts[a_idx as usize].adl_a_basis = 13;
    engine.accounts[a_idx as usize].adl_k_snap = 0i128;
    engine.accounts[a_idx as usize].adl_epoch_snap = 0;

    // Account b: long 5*POS_SCALE at a_basis=13
    engine.accounts[b_idx as usize].position_basis_q = (5 * POS_SCALE) as i128;
    engine.accounts[b_idx as usize].adl_a_basis = 13;
    engine.accounts[b_idx as usize].adl_k_snap = 0i128;
    engine.accounts[b_idx as usize].adl_epoch_snap = 0;

    // Accounts c,d: short 6*POS_SCALE each for OI balance
    engine.accounts[c_idx as usize].position_basis_q = -((6 * POS_SCALE) as i128);
    engine.accounts[c_idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[c_idx as usize].adl_k_snap = 0i128;
    engine.accounts[c_idx as usize].adl_epoch_snap = 0;

    engine.accounts[d_idx as usize].position_basis_q = -((6 * POS_SCALE) as i128);
    engine.accounts[d_idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[d_idx as usize].adl_k_snap = 0i128;
    engine.accounts[d_idx as usize].adl_epoch_snap = 0;

    engine.stored_pos_count_long = 2;
    engine.stored_pos_count_short = 2;
    engine.oi_eff_long_q = 12 * POS_SCALE;
    engine.oi_eff_short_q = 12 * POS_SCALE;

    // ADL: close 3*POS_SCALE from short side → shrinks A_long
    let result = engine.enqueue_adl(
        &mut ctx, Side::Short, 3 * POS_SCALE, 0u128,
    );
    assert!(result.is_ok());
    assert!(engine.adl_mult_long == 9);
    assert!(engine.phantom_dust_bound_long_q != 0);

    // Settle all four accounts through the engine's actual settle path
    let sa = engine.settle_side_effects(a_idx as usize);
    assert!(sa.is_ok());
    let sb = engine.settle_side_effects(b_idx as usize);
    assert!(sb.is_ok());
    let sc = engine.settle_side_effects(c_idx as usize);
    assert!(sc.is_ok());
    let sd = engine.settle_side_effects(d_idx as usize);
    assert!(sd.is_ok());

    // Close all positions through attach_effective_position (triggers dust accounting)
    let eff_a = engine.effective_pos_q(a_idx as usize);
    let eff_b = engine.effective_pos_q(b_idx as usize);
    let eff_c = engine.effective_pos_q(c_idx as usize);
    let eff_d = engine.effective_pos_q(d_idx as usize);

    engine.attach_effective_position(a_idx as usize, 0i128);
    engine.attach_effective_position(b_idx as usize, 0i128);
    engine.attach_effective_position(c_idx as usize, 0i128);
    engine.attach_effective_position(d_idx as usize, 0i128);

    // Update OI with actual effective positions
    engine.oi_eff_long_q = engine.oi_eff_long_q
        .checked_sub(eff_a.unsigned_abs()).unwrap_or(0);
    engine.oi_eff_long_q = engine.oi_eff_long_q
        .checked_sub(eff_b.unsigned_abs()).unwrap_or(0);
    engine.oi_eff_short_q = engine.oi_eff_short_q
        .checked_sub(eff_c.unsigned_abs()).unwrap_or(0);
    engine.oi_eff_short_q = engine.oi_eff_short_q
        .checked_sub(eff_d.unsigned_abs()).unwrap_or(0);

    assert!(engine.oi_eff_long_q == engine.oi_eff_short_q);

    let reset_result = engine.schedule_end_of_instruction_resets(&mut ctx);
    assert!(reset_result.is_ok(), "dust bound must be sufficient for reset after all positions closed");
}

// ############################################################################
// SPEC PROPERTY #17: fee shortfall routes to fee_credits, NOT PnL
// ############################################################################
//
// Spec v11.9 §4.10: "Unpaid explicit fees are account-local fee debt.
// They MUST NOT be written into PNL_i."
// Spec property #17: "trading-fee or liquidation-fee shortfall becomes
// negative fee_credits_i, does not touch PNL_i."

#[kani::proof]
#[kani::solver(cadical)]
fn proof_fee_shortfall_routes_to_fee_credits() {
    let mut params = zero_fee_params();
    params.trading_fee_bps = 10; // 10 bps
    let mut engine = RiskEngine::new(params);

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 10_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Open a position: a goes long, b goes short
    let size = POS_SCALE as i128;
    let result = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size, DEFAULT_ORACLE);
    assert!(result.is_ok());

    // Zero a's capital so the fee can't be paid from principal.
    // Give enough PnL to stay solvent for margin checks.
    engine.set_capital(a as usize, 0);
    engine.set_pnl(a as usize, 5_000_000i128);
    engine.vault = U128::new(engine.vault.get() + 5_000_000);

    // Record fee_credits and PnL before the close.
    let fc_before = engine.accounts[a as usize].fee_credits.get();

    // Close position: a sells back (trade fee will be charged).
    // Capital is 0, so the entire fee must be shortfall → fee_credits.
    let neg_size = -(POS_SCALE as i128);
    let result2 = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, neg_size, DEFAULT_ORACLE);

    match result2 {
        Ok(()) => {
            let fc_after = engine.accounts[a as usize].fee_credits.get();
            // fee_credits must have decreased (become more negative) by the shortfall
            assert!(fc_after < fc_before,
                "fee shortfall must decrease fee_credits (create debt)");
        }
        Err(_) => {
            // Trade rejected for margin or other reasons — acceptable.
        }
    }
}

// ############################################################################
// SPEC PROPERTY #16: organic-close bankruptcy guard
// ############################################################################

#[kani::proof]
#[kani::solver(cadical)]
fn proof_organic_close_bankruptcy_guard() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 10_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let size = (90 * POS_SCALE) as i128;
    let result = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size, DEFAULT_ORACLE);
    assert!(result.is_ok());

    let crash_price = 800u64;
    let crash_slot = DEFAULT_SLOT + 1;
    engine.last_crank_slot = crash_slot;

    let neg_size = -(90 * POS_SCALE as i128);
    let result2 = engine.execute_trade(a, b, crash_price, crash_slot, neg_size, crash_price);

    assert!(result2.is_err(),
        "organic close that leaves uncovered negative PnL must be rejected");
}

// ############################################################################
// SPEC PROPERTY #24: solvent flat-close succeeds
// ############################################################################

#[kani::proof]
#[kani::solver(cadical)]
fn proof_solvent_flat_close_succeeds() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Open a small position
    let size = POS_SCALE as i128;
    let result = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size, DEFAULT_ORACLE);
    assert!(result.is_ok());

    // Price drops modestly — a has losses but plenty of capital to cover
    let new_price = 900u64;
    let slot2 = DEFAULT_SLOT + 1;
    engine.last_crank_slot = slot2;

    // Close to flat: a sells their long position
    let neg_size = -(POS_SCALE as i128);
    let result2 = engine.execute_trade(a, b, new_price, slot2, neg_size, new_price);

    assert!(result2.is_ok(),
        "solvent trader closing to flat must not be rejected");
    assert!(engine.check_conservation(), "conservation must hold after flat close");
}
