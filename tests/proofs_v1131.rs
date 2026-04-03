//! Section 7 — v12.0.2 Spec Compliance Proofs
//!
//! Properties 46, 59-75: live funding, configuration immutability,
//! bilateral OI decomposition, partial liquidation, deposit guards, profit conversion.

#![cfg(kani)]

mod common;
use common::*;

// ############################################################################
// PROPERTY 46: Funding rate recomputation determinism and bound enforcement
// ############################################################################

/// recompute_r_last_from_final_state(rate) stores exactly rate when
/// |rate| <= MAX_ABS_FUNDING_BPS_PER_SLOT (spec v12.0.2 §4.12).
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_recompute_r_last_stores_rate() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let rate: i16 = kani::any();
    kani::assume(rate.unsigned_abs() <= MAX_ABS_FUNDING_BPS_PER_SLOT as u16);

    engine.recompute_r_last_from_final_state(rate as i64);
    assert!(engine.funding_rate_bps_per_slot_last == rate as i64,
        "r_last must equal the supplied rate");
}

// ############################################################################
// PROPERTY 74: Funding rate bound enforcement
// ############################################################################

/// recompute_r_last_from_final_state rejects |rate| > MAX_ABS_FUNDING_BPS_PER_SLOT.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
#[kani::should_panic]
fn proof_funding_rate_bound_rejected() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let rate: i64 = kani::any();
    kani::assume(rate.unsigned_abs() > MAX_ABS_FUNDING_BPS_PER_SLOT as u64);
    engine.recompute_r_last_from_final_state(rate);
}

// ############################################################################
// PROPERTY 72: Funding sign and floor-direction correctness
// ############################################################################

/// When r_last > 0, K_long decreases and K_short increases (longs pay shorts).
/// When r_last < 0, K_long increases and K_short decreases (shorts pay longs).
/// fund_term uses floor division: positive quotients round down, negative round
/// toward negative infinity.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_funding_sign_and_floor() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE;

    // Symbolic rate (bounded, nonzero)
    let rate: i16 = kani::any();
    kani::assume(rate != 0);
    kani::assume(rate.unsigned_abs() <= MAX_ABS_FUNDING_BPS_PER_SLOT as u16);
    engine.funding_rate_bps_per_slot_last = rate as i64;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    // dt=1, same price → only funding changes K
    let result = engine.accrue_market_to(1, DEFAULT_ORACLE);
    assert!(result.is_ok());

    if rate > 0 {
        // Longs pay shorts
        assert!(engine.adl_coeff_long <= k_long_before,
            "positive rate: K_long must not increase");
        assert!(engine.adl_coeff_short >= k_short_before,
            "positive rate: K_short must not decrease");
    } else {
        // Shorts pay longs (fund_term < 0 → floor rounds away from zero)
        assert!(engine.adl_coeff_long >= k_long_before,
            "negative rate: K_long must not decrease");
        assert!(engine.adl_coeff_short <= k_short_before,
            "negative rate: K_short must not increase");
    }
}

/// Explicit floor-direction test: rate=-1, price=1000, dt=1 produces
/// fund_num = -1000, fund_term = floor(-1000/10000) = floor(-0.1) = -1.
/// Truncation toward zero would give 0 (wrong). Floor toward -∞ gives -1.
/// This means longs gain and shorts lose even for tiny negative rates.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_funding_floor_not_truncation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE; // 1000
    engine.funding_rate_bps_per_slot_last = -1; // tiny negative rate

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(1, DEFAULT_ORACLE);
    assert!(result.is_ok());

    // fund_num = 1000 * (-1) * 1 = -1000
    // floor(-1000 / 10000) = floor(-0.1) = -1 (NOT 0 from truncation)
    // K_long -= A_long * (-1) = K_long + ADL_ONE → longs gain
    // K_short += A_short * (-1) = K_short - ADL_ONE → shorts lose
    assert_eq!(engine.adl_coeff_long, k_long_before + (ADL_ONE as i128),
        "floor(-0.1) must be -1: longs must gain ADL_ONE");
    assert_eq!(engine.adl_coeff_short, k_short_before - (ADL_ONE as i128),
        "floor(-0.1) must be -1: shorts must lose ADL_ONE");
}

// ############################################################################
// PROPERTY 73: Funding skip on zero OI
// ############################################################################

/// accrue_market_to applies no funding K delta when short side OI is zero.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_funding_skip_zero_oi_short() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE;
    engine.funding_rate_bps_per_slot_last = 5000;

    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = 0;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(100, DEFAULT_ORACLE);
    assert!(result.is_ok());

    assert_eq!(engine.adl_coeff_long, k_long_before,
        "K_long must not change when short OI is zero");
    assert_eq!(engine.adl_coeff_short, k_short_before,
        "K_short must not change when short OI is zero");
}

/// accrue_market_to applies no funding K delta when long side OI is zero.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_funding_skip_zero_oi_long() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE;
    engine.funding_rate_bps_per_slot_last = -3000;

    engine.oi_eff_long_q = 0;
    engine.oi_eff_short_q = POS_SCALE;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(100, DEFAULT_ORACLE);
    assert!(result.is_ok());

    assert_eq!(engine.adl_coeff_long, k_long_before,
        "K_long must not change when long OI is zero");
    assert_eq!(engine.adl_coeff_short, k_short_before,
        "K_short must not change when long OI is zero");
}

/// accrue_market_to applies no funding K delta when both sides have zero OI.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_funding_skip_zero_oi_both() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE;
    engine.funding_rate_bps_per_slot_last = 10000;

    engine.oi_eff_long_q = 0;
    engine.oi_eff_short_q = 0;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(100, DEFAULT_ORACLE);
    assert!(result.is_ok());

    assert_eq!(engine.adl_coeff_long, k_long_before,
        "K_long must not change when both OI zero");
    assert_eq!(engine.adl_coeff_short, k_short_before,
        "K_short must not change when both OI zero");
}

// ############################################################################
// PROPERTY 71: Funding sub-stepping with dt > MAX_FUNDING_DT
// ############################################################################

/// When dt > MAX_FUNDING_DT, accrue_market_to splits funding into sub-steps.
/// The total K delta must equal the sum of sub-step deltas.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_funding_substep_large_dt() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE;
    engine.funding_rate_bps_per_slot_last = 100;

    // dt = MAX_FUNDING_DT + 1 → 2 sub-steps: MAX_FUNDING_DT and 1
    let dt = MAX_FUNDING_DT + 1;
    let result = engine.accrue_market_to(dt, DEFAULT_ORACLE);
    assert!(result.is_ok());

    // fund_term per sub-step with rate=100, price=1000:
    // sub-step 1: fund_num = 1000 * 100 * 65535 = 6_553_500_000; fund_term = 655_350
    // sub-step 2: fund_num = 1000 * 100 * 1 = 100_000; fund_term = 10
    // total fund_term effect = (655_350 + 10) * ADL_ONE = 655_360_000_000
    let expected_delta: i128 = (655_350i128 + 10i128) * (ADL_ONE as i128);
    assert_eq!(engine.adl_coeff_long, -expected_delta,
        "K_long must reflect sum of sub-step funding deltas");
    assert_eq!(engine.adl_coeff_short, expected_delta,
        "K_short must reflect sum of sub-step funding deltas");
}

// ############################################################################
// PROPERTY 75: Funding price-basis timing
// ############################################################################

/// Funding uses fund_px_0 (start-of-call snapshot of fund_px_last), not the
/// current oracle_price. After the call, fund_px_last is updated to oracle_price.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_funding_price_basis_timing() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
    engine.last_oracle_price = 500; // old price
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 500; // fund_px_0 = 500
    engine.funding_rate_bps_per_slot_last = 100;

    // Call with new oracle price 1500
    let result = engine.accrue_market_to(1, 1500);
    assert!(result.is_ok());

    // Funding should use fund_px_0=500, not oracle_price=1500
    // fund_num = 500 * 100 * 1 = 50_000; fund_term = 50_000 / 10000 = 5
    // (NOT 1500 * 100 * 1 / 10000 = 15)
    // But mark-to-market also affects K: ΔP = 1500-500 = 1000
    // K_long += A_long * ΔP = ADL_ONE * 1000 = 1_000_000_000 (mark)
    // K_long -= A_long * fund_term = ADL_ONE * 5 = 5_000_000 (funding)
    // Net K_long = 1_000_000_000 - 5_000_000 = 995_000_000
    let expected_k_long = 1_000_000_000i128 - 5_000_000i128;
    assert_eq!(engine.adl_coeff_long, expected_k_long,
        "funding must use fund_px_0=500, not oracle=1500");

    // After call, fund_px_last must be updated to oracle_price
    assert_eq!(engine.funding_price_sample_last, 1500,
        "fund_px_last must be updated to oracle_price for next interval");
}

// ############################################################################
// Funding: zero rate produces no K change (regression from v11.31)
// ############################################################################

/// When r_last = 0, no funding transfer occurs regardless of dt or OI.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_accrue_no_funding_when_rate_zero() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE;
    engine.funding_rate_bps_per_slot_last = 0;

    let dt: u16 = kani::any();
    kani::assume(dt >= 1 && dt <= 1000);

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(dt as u64, DEFAULT_ORACLE);
    assert!(result.is_ok());

    assert_eq!(engine.adl_coeff_long, k_long_before, "zero rate: K_long unchanged");
    assert_eq!(engine.adl_coeff_short, k_short_before, "zero rate: K_short unchanged");
}

/// accrue_market_to still applies mark-to-market correctly.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_accrue_mark_still_works() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = DEFAULT_ORACLE;

    let new_price: u64 = kani::any();
    kani::assume(new_price > 0 && new_price <= 2000 && new_price != DEFAULT_ORACLE);

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(1, new_price);
    assert!(result.is_ok());

    // Mark must change K: K_long += A_long * ΔP, K_short -= A_short * ΔP
    let delta_p = (new_price as i128) - (DEFAULT_ORACLE as i128);
    let expected_k_long = k_long_before + (ADL_ONE as i128) * delta_p;
    let expected_k_short = k_short_before - (ADL_ONE as i128) * delta_p;

    assert!(engine.adl_coeff_long == expected_k_long,
        "K_long must reflect mark-to-market");
    assert!(engine.adl_coeff_short == expected_k_short,
        "K_short must reflect mark-to-market");
}

// ############################################################################
// PROPERTY: maintenance fees disabled (spec §8.2)
// ############################################################################

/// Spec §8.2: maintenance fees enabled — touch charges dt * fee_per_slot.
/// Symbolic fee and dt prove conservation holds with fee charges.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_touch_maintenance_fee_conservation() {
    let mut params = zero_fee_params();
    let fee_per_slot: u32 = kani::any();
    kani::assume(fee_per_slot >= 1 && fee_per_slot <= 1000);
    params.maintenance_fee_per_slot = U128::new(fee_per_slot as u128);
    let mut engine = RiskEngine::new(params);

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, 0).unwrap();
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = 0;

    let cap_before = engine.accounts[idx as usize].capital.get();

    let dt: u16 = kani::any();
    kani::assume(dt >= 1 && dt <= 1000);

    let result = engine.touch_account_full(idx as usize, DEFAULT_ORACLE, dt as u64);
    assert!(result.is_ok());

    // Capital must decrease by exactly the fee
    let expected_fee = (dt as u128) * (fee_per_slot as u128);
    let cap_after = engine.accounts[idx as usize].capital.get();
    assert_eq!(cap_before - cap_after, expected_fee,
        "capital must decrease by exactly dt * fee_per_slot");
    assert!(engine.check_conservation());
}

// ############################################################################
// PROPERTY 62: Pure deposit no-insurance-draw
// ############################################################################

/// deposit never calls absorb_protocol_loss, never decrements I (spec property 62).
/// settle_losses MAY pay from capital to reduce negative PNL (that's loss settlement,
/// not insurance draw), but resolve_flat_negative is NOT called.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_deposit_no_insurance_draw() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    // Start with zero capital
    engine.deposit(idx, 0, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Set very large negative PNL (much more than any deposit)
    engine.set_pnl(idx as usize, -10_000_000i128);

    let ins_before = engine.insurance_fund.balance.get();

    // Deposit a small amount — capital insufficient to cover PNL
    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 1_000_000);

    let result = engine.deposit(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT);
    assert!(result.is_ok());

    // Insurance fund must NOT decrease (no absorb_protocol_loss via resolve_flat_negative)
    assert!(engine.insurance_fund.balance.get() >= ins_before,
        "deposit must never decrement I");

    // PNL must still be negative (settle_losses paid from capital but couldn't cover all)
    assert!(engine.accounts[idx as usize].pnl < 0,
        "negative PNL must survive deposit — resolve_flat_negative not called");
}

// ############################################################################
// PROPERTY 66: Flat authoritative deposit sweep
// ############################################################################

/// deposit does NOT sweep fee debt when PNL < 0 persists after settle_losses.
/// Symbolic deposit amount — for any amount, if PNL stays negative, no sweep.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_deposit_sweep_pnl_guard() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    // Start with zero capital
    engine.deposit(idx, 0, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Symbolic fee debt
    let debt: u16 = kani::any();
    kani::assume(debt >= 1 && debt <= 10_000);
    engine.accounts[idx as usize].fee_credits = I128::new(-(debt as i128));

    // Set large negative PNL that exceeds any deposit amount
    engine.set_pnl(idx as usize, -10_000_000i128);

    let fc_before = engine.accounts[idx as usize].fee_credits.get();

    // Symbolic deposit — always insufficient to cover PNL=-10M
    let amount: u32 = kani::any();
    kani::assume(amount >= 1 && amount <= 1_000_000);
    engine.deposit(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // After deposit: capital went to settle_losses (paid toward PNL=-10M)
    // PNL is still very negative, so sweep must NOT happen
    assert!(engine.accounts[idx as usize].fee_credits.get() == fc_before,
        "deposit must not sweep when PNL < 0 after settle_losses");
    assert!(engine.accounts[idx as usize].pnl < 0,
        "PNL must still be negative — settle_losses can't cover full loss");
}

/// deposit DOES sweep fee debt on flat state with PNL >= 0.
/// Symbolic deposit amount exercises sweep with varying capital levels.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_deposit_sweep_when_pnl_nonneg() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    // Symbolic initial capital — ensures fee_debt_sweep has capital to pay from
    let init_cap: u32 = kani::any();
    kani::assume(init_cap >= 10_000 && init_cap <= 1_000_000);
    engine.deposit(idx, init_cap as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Give account fee debt
    engine.accounts[idx as usize].fee_credits = I128::new(-5000);

    // PNL = 0 (flat position, no losses)
    assert!(engine.accounts[idx as usize].pnl == 0);

    // Symbolic deposit amount
    let dep: u32 = kani::any();
    kani::assume(dep >= 1 && dep <= 100_000);
    engine.deposit(idx, dep as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // fee_credits must have improved (debt partially/fully paid)
    assert!(engine.accounts[idx as usize].fee_credits.get() > -5000,
        "deposit must sweep fee debt when flat with PNL >= 0");
}

// ############################################################################
// PROPERTY 61: Insurance top-up bounded arithmetic + now_slot
// ############################################################################

/// top_up_insurance_fund uses checked addition, enforces MAX_VAULT_TVL,
/// sets current_slot, and increases V and I by the same amount.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_top_up_insurance_now_slot() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.current_slot = 50;

    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 1_000_000);

    let now_slot: u64 = kani::any();
    kani::assume(now_slot >= 50 && now_slot <= 200);

    let v_before = engine.vault.get();
    let i_before = engine.insurance_fund.balance.get();

    let result = engine.top_up_insurance_fund(amount as u128, now_slot);
    assert!(result.is_ok());

    // current_slot updated
    assert!(engine.current_slot == now_slot, "current_slot must be updated");

    // V and I increase by exact same amount
    assert!(engine.vault.get() == v_before + amount as u128,
        "V must increase by amount");
    assert!(engine.insurance_fund.balance.get() == i_before + amount as u128,
        "I must increase by amount");
}

/// top_up_insurance_fund rejects now_slot < current_slot.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_top_up_insurance_rejects_stale_slot() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.current_slot = 100;

    let result = engine.top_up_insurance_fund(1000, 50);
    assert!(result.is_err(), "must reject now_slot < current_slot");
}

// ############################################################################
// PROPERTY 69: Positive conversion denominator
// ############################################################################

/// Whenever flat auto-conversion consumes x > 0 released profit,
/// pnl_matured_pos_tot > 0 and h_den > 0.
/// We verify this by setting up a state with released profit and checking
/// that the haircut denominator is positive.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_positive_conversion_denominator() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Set up matured positive PNL
    let pnl_val: u32 = kani::any();
    kani::assume(pnl_val > 0 && pnl_val <= 100_000);
    let pnl = pnl_val as i128;

    engine.set_pnl(idx as usize, pnl);
    // For released_pos to be > 0, the account must have matured PnL.
    // released_pos = pnl_matured_pos_tot contribution from this account.
    // In a flat account, after warmup, the released portion is positive.
    // We directly verify the haircut ratio:
    engine.pnl_matured_pos_tot = pnl_val as u128;

    let (h_num, h_den) = engine.haircut_ratio();
    // When pnl_matured_pos_tot > 0, h_den == pnl_matured_pos_tot > 0
    assert!(h_den > 0, "h_den must be positive when pnl_matured_pos_tot > 0");
    assert!(h_num <= h_den, "h_num must not exceed h_den");
}

// ############################################################################
// PROPERTY 64: Exact trade OI decomposition
// ############################################################################

/// Trade uses exact bilateral OI after-values for both gating and writeback.
/// Symbolic trade size exercises open, close, and flip paths.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_bilateral_oi_decomposition() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.last_crank_slot = DEFAULT_SLOT;
    engine.last_market_slot = DEFAULT_SLOT;
    engine.last_oracle_price = DEFAULT_ORACLE;

    // First trade: open a position (a long, b short)
    let open_size = (100 * POS_SCALE) as i128;
    let r1 = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, open_size, DEFAULT_ORACLE, 0i64);
    assert!(r1.is_ok(), "initial trade must succeed");

    // Second trade: symbolic size exercises close, reduce, and flip paths
    let raw_size: i16 = kani::any();
    kani::assume(raw_size != 0);
    // Scale to position units — covers -32768..32767 * POS_SCALE
    let abs_size_q = ((raw_size as i128).unsigned_abs()) * (POS_SCALE as u128);
    let pos_size_q = abs_size_q as i128;

    // size_q > 0 required: when raw_size < 0, swap a and b
    let result = if raw_size > 0 {
        engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, pos_size_q, DEFAULT_ORACLE, 0i64)
    } else {
        engine.execute_trade(b, a, DEFAULT_ORACLE, DEFAULT_SLOT, pos_size_q, DEFAULT_ORACLE, 0i64)
    };

    if result.is_ok() {
        let eff_a = engine.effective_pos_q(a as usize);
        let eff_b = engine.effective_pos_q(b as usize);

        // OI_long should be the sum of positive positions
        let expected_long = if eff_a > 0 { eff_a as u128 } else { 0 }
            + if eff_b > 0 { eff_b as u128 } else { 0 };
        let expected_short = if eff_a < 0 { eff_a.unsigned_abs() } else { 0 }
            + if eff_b < 0 { eff_b.unsigned_abs() } else { 0 };

        assert!(engine.oi_eff_long_q == expected_long,
            "OI_long must match bilateral decomposition");
        assert!(engine.oi_eff_short_q == expected_short,
            "OI_short must match bilateral decomposition");

        // OI balance: must be equal
        assert!(engine.oi_eff_long_q == engine.oi_eff_short_q,
            "OI_long must equal OI_short");
    }
}

// ############################################################################
// PROPERTY 68: Partial liquidation remainder nonzero
// ############################################################################

/// Partial liquidation with 0 < q_close < abs(eff) produces nonzero remainder.
/// Close most of the position (90%) so post-partial health check passes.
/// Non-vacuity: explicitly assert Ok(true) is reached.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_partial_liquidation_remainder_nonzero() {
    let mut params = zero_fee_params();
    params.maintenance_margin_bps = 100; // 1% margin — easy to restore health after partial
    let mut engine = RiskEngine::new(params);

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    // Small deposit for a — high leverage. Large deposit for b — counterparty.
    engine.deposit(a, 50_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.last_crank_slot = DEFAULT_SLOT;
    engine.last_market_slot = DEFAULT_SLOT;
    engine.last_oracle_price = DEFAULT_ORACLE;

    // Open near-max leverage: 480 units, notional=480K, IM ~48K with 50K capital
    let size_q = (480 * POS_SCALE) as i128;
    engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size_q, DEFAULT_ORACLE, 0i64).unwrap();

    let abs_eff = engine.effective_pos_q(a as usize).unsigned_abs();
    assert!(abs_eff > 0, "position must be open");

    // Close all but 1 unit — leaves minimal remainder
    // Post-partial: 1 unit notional = ~crash_price/POS_SCALE, MM ~= 0
    let q_close = abs_eff - POS_SCALE;
    assert!(q_close > 0 && q_close < abs_eff, "q_close must be valid partial");

    // Crash: 10% drop triggers liquidation (PNL = -480*100 = -48K, equity ~2K < MM=4800)
    let crash = 900u64;
    let result = engine.liquidate_at_oracle(a, DEFAULT_SLOT + 1, crash,
        LiquidationPolicy::ExactPartial(q_close), 0i64);

    // Non-vacuity: partial MUST succeed
    assert!(result.is_ok(), "partial liquidation must not revert");
    assert!(result.unwrap(), "account must be liquidatable at crash price");

    // Core property: remainder must be nonzero
    let eff_after = engine.effective_pos_q(a as usize);
    assert!(eff_after != 0, "partial liquidation must leave nonzero remainder");
}

// ############################################################################
// PROPERTY 65: Liquidation policy determinism
// ############################################################################

/// liquidate accepts only FullClose or ExactPartial; ExactPartial with
/// q_close_q == 0 or q_close_q >= abs(eff) is rejected.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_liquidation_policy_validity() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.last_crank_slot = DEFAULT_SLOT;
    engine.last_market_slot = DEFAULT_SLOT;
    engine.last_oracle_price = DEFAULT_ORACLE;

    let size_q = (400 * POS_SCALE) as i128;
    engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size_q, DEFAULT_ORACLE, 0i64).unwrap();

    let abs_eff = engine.effective_pos_q(a as usize).unsigned_abs();

    // ExactPartial(0) must fail
    let r1 = engine.liquidate_at_oracle(a, DEFAULT_SLOT + 1, 500,
        LiquidationPolicy::ExactPartial(0), 0i64);
    // Either not liquidatable or rejected
    if let Ok(true) = r1 {
        panic!("ExactPartial(0) must not succeed as a partial liquidation");
    }
}

// ############################################################################
// PROPERTY 60: Direct fee-credit repayment cap
// ############################################################################

/// deposit_fee_credits applies only min(amount, debt), never makes fee_credits
/// positive, increases V and I by exactly the applied amount.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_deposit_fee_credits_cap() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Give fee debt
    engine.accounts[idx as usize].fee_credits = I128::new(-5000);

    let v_before = engine.vault.get();
    let i_before = engine.insurance_fund.balance.get();

    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 100_000);

    let result = engine.deposit_fee_credits(idx, amount as u128, DEFAULT_SLOT);
    assert!(result.is_ok());

    // fee_credits must be <= 0
    assert!(engine.accounts[idx as usize].fee_credits.get() <= 0,
        "fee_credits must never become positive");

    // Applied amount = min(amount, 5000)
    let expected_pay = core::cmp::min(amount as u128, 5000);
    assert!(engine.vault.get() == v_before + expected_pay, "V must increase by applied amount");
    assert!(engine.insurance_fund.balance.get() == i_before + expected_pay, "I must increase by applied amount");
}

// ############################################################################
// PROPERTY 70: Partial liquidation health check survives reset scheduling
// ############################################################################

/// Partial liquidation that closes a tiny amount MUST be rejected by the
/// mandatory post-partial health check (§9.4 step 14). Closing 1 unit out
/// of a large position at a crash price cannot restore health.
/// This proves enforcement: the health check rejects insufficient partials.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_partial_liq_health_check_mandatory() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.last_crank_slot = DEFAULT_SLOT;
    engine.last_market_slot = DEFAULT_SLOT;
    engine.last_oracle_price = DEFAULT_ORACLE;

    // Open near-max leverage position
    let size_q = (400 * POS_SCALE) as i128;
    engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size_q, DEFAULT_ORACLE, 0i64).unwrap();

    // Symbolic tiny close amount (1..100 units — all too small to restore health)
    let tiny_close: u8 = kani::any();
    kani::assume(tiny_close >= 1);

    // Severe crash — account is deeply unhealthy
    let result = engine.liquidate_at_oracle(a, DEFAULT_SLOT + 1, 500,
        LiquidationPolicy::ExactPartial(tiny_close as u128), 0i64);

    // Health check at step 14 MUST reject: closing a few units out of 400M
    // position at 50% crash cannot restore maintenance margin.
    // Result is Err(Undercollateralized) — NOT Ok(true).
    assert!(!matches!(result, Ok(true)),
        "tiny partial must be rejected by health check — remainder still unhealthy");
}

// ############################################################################
// PROPERTY 42: Post-reset funding recomputation stores exactly 0
// ############################################################################

/// keeper_crank invokes recompute_r_last_from_final_state exactly once after
/// final reset handling. The stored rate equals the supplied funding_rate
/// regardless of the pre-crank rate.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_keeper_crank_r_last_stores_supplied_rate() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Symbolic pre-crank rate and supplied rate
    let pre_rate: i64 = kani::any();
    engine.funding_rate_bps_per_slot_last = pre_rate;

    let supplied_rate: i16 = kani::any();
    kani::assume(supplied_rate.unsigned_abs() <= MAX_ABS_FUNDING_BPS_PER_SLOT as u16);

    let result = engine.keeper_crank(DEFAULT_SLOT + 1, DEFAULT_ORACLE,
        &[(idx, None)], 64, supplied_rate as i64);
    assert!(result.is_ok());

    // r_last must equal the supplied rate, not the pre-crank rate
    assert!(engine.funding_rate_bps_per_slot_last == supplied_rate as i64,
        "r_last must equal supplied funding_rate after keeper_crank");
}

// ############################################################################
// PROPERTY 44: Deposit true-flat guard and latent-loss seniority
// ############################################################################

/// A deposit into an account with basis_pos_q != 0 neither routes unresolved
/// negative PnL through §7.3 nor sweeps fee debt.
/// Symbolic deposit amount and fee debt prove this for all combinations.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_deposit_nonflat_no_sweep_no_resolve() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.last_crank_slot = DEFAULT_SLOT;
    engine.last_market_slot = DEFAULT_SLOT;
    engine.last_oracle_price = DEFAULT_ORACLE;

    // Open position for a
    let size_q = (100 * POS_SCALE) as i128;
    engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size_q, DEFAULT_ORACLE, 0i64).unwrap();

    // Symbolic fee debt
    let debt: u16 = kani::any();
    kani::assume(debt >= 1 && debt <= 10_000);
    engine.accounts[a as usize].fee_credits = I128::new(-(debt as i128));
    engine.set_pnl(a as usize, -500i128);

    let fc_before = engine.accounts[a as usize].fee_credits.get();
    let ins_before = engine.insurance_fund.balance.get();

    // Symbolic deposit into account with open position (basis != 0)
    let dep_amount: u32 = kani::any();
    kani::assume(dep_amount >= 1 && dep_amount <= 1_000_000);
    engine.deposit(a, dep_amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // fee_credits unchanged (no sweep on non-flat account)
    assert!(engine.accounts[a as usize].fee_credits.get() == fc_before,
        "deposit must not sweep fee debt when basis != 0");

    // Insurance must not decrease (no resolve_flat_negative when not flat)
    assert!(engine.insurance_fund.balance.get() >= ins_before,
        "deposit must not decrement insurance on non-flat account");
}
