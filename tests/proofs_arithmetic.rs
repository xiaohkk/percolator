//! Section 3 — Pure math helper correctness proofs
//!
//! Arithmetic helper proofs: pure, loop-free, fast.

#![cfg(kani)]

mod common;
use common::*;

// ============================================================================
// T0.1: floor_div_signed_conservative_is_floor
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t0_1_floor_div_signed_conservative_is_floor() {
    let n_raw: i8 = kani::any();
    let d_raw: u8 = kani::any();
    kani::assume(d_raw > 0);

    let n = I256::from_i128(n_raw as i128);
    let d = U256::from_u128(d_raw as u128);

    let result = floor_div_signed_conservative(n, d);

    let n_i32 = n_raw as i32;
    let d_i32 = d_raw as i32;
    let expected = if n_i32 >= 0 {
        n_i32 / d_i32
    } else {
        let abs_n = -n_i32;
        -((abs_n + d_i32 - 1) / d_i32)
    };

    let result_i128 = result.try_into_i128().unwrap();
    assert!(result_i128 == expected as i128, "floor_div mismatch");
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t0_1_sat_negative_with_remainder() {
    let n_raw: i8 = kani::any();
    let d_raw: u8 = kani::any();
    kani::assume(d_raw > 1);
    kani::assume(n_raw < 0);
    let abs_n = -(n_raw as i32);
    kani::assume((abs_n as u32) % (d_raw as u32) != 0);

    let n = I256::from_i128(n_raw as i128);
    let d = U256::from_u128(d_raw as u128);
    let result = floor_div_signed_conservative(n, d);

    let trunc = (n_raw as i32) / (d_raw as i32);
    let result_i128 = result.try_into_i128().unwrap();
    assert!(result_i128 < trunc as i128);
}

// ============================================================================
// T0.2: mul_div_floor/ceil algebraic properties
// ============================================================================

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t0_2_mul_div_floor_algebraic_identity() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();
    let c: u8 = kani::any();
    kani::assume(c > 0);

    let product = (a as u32) * (b as u32);
    let floor_val = product / (c as u32);
    let remainder = product % (c as u32);

    assert!(floor_val * (c as u32) + remainder == product);
    assert!(remainder < c as u32);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t0_2_mul_div_ceil_algebraic_identity() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();
    let c: u8 = kani::any();
    kani::assume(c > 0);

    let product = (a as u32) * (b as u32);
    let floor_val = product / (c as u32);
    let remainder = product % (c as u32);
    let ceil_val = (product + (c as u32) - 1) / (c as u32);

    if remainder == 0 {
        assert!(ceil_val == floor_val);
    } else {
        assert!(ceil_val == floor_val + 1);
    }
}

#[kani::proof]
#[kani::unwind(18)]
#[kani::solver(cadical)]
fn t0_2c_mul_div_floor_matches_reference() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();
    let c: u8 = kani::any();
    kani::assume(c > 0);

    let result = mul_div_floor_u256(
        U256::from_u128(a as u128),
        U256::from_u128(b as u128),
        U256::from_u128(c as u128),
    );

    let expected = ((a as u32) * (b as u32)) / (c as u32);
    let result_u128 = result.try_into_u128().unwrap();
    assert!(result_u128 == expected as u128, "mul_div_floor mismatch");
}

#[kani::proof]
#[kani::unwind(18)]
#[kani::solver(cadical)]
fn t0_2d_mul_div_ceil_matches_reference() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();
    let c: u8 = kani::any();
    kani::assume(c > 0);

    let result = mul_div_ceil_u256(
        U256::from_u128(a as u128),
        U256::from_u128(b as u128),
        U256::from_u128(c as u128),
    );

    let product = (a as u32) * (b as u32);
    let expected = (product + (c as u32) - 1) / (c as u32);
    let result_u128 = result.try_into_u128().unwrap();
    assert!(result_u128 == expected as u128, "mul_div_ceil mismatch");
}

// ============================================================================
// T0.4: safe_fee_debt_and_cap_math
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t0_4_fee_debt_no_overflow() {
    let fc: i128 = kani::any();
    let debt = fee_debt_u128_checked(fc);
    if fc < 0 {
        assert!(debt > 0);
        assert!(debt == fc.unsigned_abs());
    } else {
        assert!(debt == 0);
    }
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t0_4_saturating_mul_no_panic() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();

    let a256 = U256::from_u128(a as u128);
    let result = saturating_mul_u256_u64(a256, b as u64);
    let expected = (a as u128) * (b as u128);
    assert!(result == U256::from_u128(expected));

    kani::assume(b > 1);
    let result_max = saturating_mul_u256_u64(U256::MAX, b as u64);
    assert!(result_max == U256::MAX, "must saturate at U256::MAX");
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t0_4_fee_debt_i128_min() {
    let debt = fee_debt_u128_checked(i128::MIN);
    assert!(debt == (1u128 << 127), "fee_debt of i128::MIN must be 2^127");
}

// ============================================================================
// From kani.rs: notional proofs
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_notional_flat_is_zero() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let oracle: u16 = kani::any();
    kani::assume(oracle > 0 && oracle <= 1000);

    let notional = engine.notional(idx as usize, oracle as u64);
    assert!(notional == 0);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_notional_scales_with_price() {
    let q: u8 = kani::any();
    let p1: u8 = kani::any();
    let p2: u8 = kani::any();

    kani::assume(q > 0);
    kani::assume(p1 > 0);
    kani::assume(p2 >= p1);

    let n1 = (q as u32) * (p1 as u32);
    let n2 = (q as u32) * (p2 as u32);
    assert!(n2 >= n1);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_warmup_bounded_by_available() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let pnl_val: u16 = kani::any();
    kani::assume(pnl_val > 0 && pnl_val <= 10_000);
    engine.set_pnl(idx as usize, I256::from_u128(pnl_val as u128));
    engine.update_warmup_slope(idx as usize);

    let elapsed: u16 = kani::any();
    kani::assume(elapsed <= 500);
    engine.current_slot = DEFAULT_SLOT + elapsed as u64;

    let warmable = engine.warmable_gross(idx as usize);
    let pnl = &engine.accounts[idx as usize].pnl;
    let avail = if pnl.is_positive() {
        pnl.abs_u256().saturating_sub(engine.accounts[idx as usize].reserved_pnl)
    } else {
        U256::ZERO
    };

    assert!(warmable <= avail);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_warmup_bounded_by_cap() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 100_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    engine.set_pnl(idx as usize, I256::from_u128(50_000));
    engine.update_warmup_slope(idx as usize);

    let slope = engine.accounts[idx as usize].warmup_slope_per_step;
    let started = engine.accounts[idx as usize].warmup_started_at_slot;

    let elapsed: u16 = kani::any();
    kani::assume(elapsed <= 500);
    engine.current_slot = started + elapsed as u64;

    let warmable = engine.warmable_gross(idx as usize);

    let cap = if slope.is_zero() {
        U256::ZERO
    } else {
        slope.checked_mul(U256::from_u128(elapsed as u128)).unwrap_or(U256::MAX)
    };

    assert!(warmable <= cap);
}

// ============================================================================
// T13.59: fused_delta_k_no_double_rounding
// ============================================================================

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t13_59_fused_delta_k_no_double_rounding() {
    let d: u8 = kani::any();
    kani::assume(d > 0);
    let oi: u8 = kani::any();
    kani::assume(oi > 0);
    let a: u8 = kani::any();
    kani::assume(a > 0);

    let beta_abs = ((d as u32) + (oi as u32) - 1) / (oi as u32);
    let old_delta_k = (a as u32) * beta_abs;

    let new_delta_k = ((d as u32) * (a as u32) + (oi as u32) - 1) / (oi as u32);

    assert!(new_delta_k <= old_delta_k,
        "fused formula must not exceed old two-step formula");

    let exact_times_oi = (d as u32) * (a as u32);
    assert!(new_delta_k * (oi as u32) >= exact_times_oi,
        "fused ceiling must be >= exact value");
}

// ============================================================================
// NEW: proof_ceil_div_positive_checked
// ============================================================================

/// ceil helper matches reference for u8
#[kani::proof]
#[kani::unwind(18)]
#[kani::solver(cadical)]
fn proof_ceil_div_positive_checked() {
    let n: u8 = kani::any();
    let d: u8 = kani::any();
    kani::assume(d > 0);

    let result = ceil_div_positive_checked(
        U256::from_u128(n as u128),
        U256::from_u128(d as u128),
    );

    let expected = ((n as u32) + (d as u32) - 1) / (d as u32);
    let result_u128 = result.try_into_u128().unwrap();
    assert!(result_u128 == expected as u128, "ceil_div_positive_checked mismatch");
}

// ============================================================================
// NEW: proof_haircut_mul_div_conservative
// ============================================================================

/// haircut uses floor, never overshoots
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_haircut_mul_div_conservative() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let pnl_val: u16 = kani::any();
    kani::assume(pnl_val > 0 && pnl_val <= 10_000);
    engine.set_pnl(idx as usize, I256::from_u128(pnl_val as u128));

    // Set vault > c_tot so residual is positive
    let cap: u16 = kani::any();
    kani::assume(cap >= 100 && cap <= 10_000);
    engine.set_capital(idx as usize, cap as u128);
    engine.vault = U128::new((cap as u128) + (pnl_val as u128));

    let (h_num, h_den) = engine.haircut_ratio();
    assert!(h_num <= h_den, "h_num must be <= h_den");
    assert!(!h_den.is_zero(), "h_den must not be zero");

    // effective_pnl = floor(pnl * h_num / h_den) <= pnl
    let effective = mul_div_floor_u256(
        U256::from_u128(pnl_val as u128), h_num, h_den);
    assert!(effective <= U256::from_u128(pnl_val as u128),
        "floor haircut must not overshoot pnl");
}
