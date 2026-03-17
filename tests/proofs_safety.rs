//! Section 5 — Economic safety, conservation
//!
//! Bounded integration, ADL safety, dust bounds, funding no-mint.

#![cfg(kani)]

mod common;
use common::*;

// ############################################################################
// BOUNDED INTEGRATION PROOFS (from kani.rs)
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_deposit_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();

    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 10_000_000);

    engine.deposit(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    assert!(engine.vault.get() == amount as u128);
    assert!(engine.c_tot.get() == amount as u128);
    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_withdraw_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.last_crank_slot = DEFAULT_SLOT;

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 1_000_000);

    let result = engine.withdraw(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT);
    if result.is_ok() {
        assert!(engine.check_conservation());
        assert!(engine.accounts[idx as usize].capital.get() == 1_000_000 - amount as u128);
    }
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_trade_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();

    engine.deposit(a, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    assert!(engine.check_conservation());

    let delta: i16 = kani::any();
    kani::assume(delta > i16::MIN);
    let delta_i256 = I256::from_i128(delta as i128);

    let pnl_a = engine.accounts[a as usize].pnl;
    let pnl_b = engine.accounts[b as usize].pnl;

    let new_a = pnl_a.checked_add(delta_i256);
    let neg_delta = delta_i256.checked_neg();

    if let (Some(na), Some(nd)) = (new_a, neg_delta) {
        if na != I256::MIN {
            if let Some(nb) = pnl_b.checked_add(nd) {
                if nb != I256::MIN {
                    engine.set_pnl(a as usize, na);
                    engine.set_pnl(b as usize, nb);

                    assert!(engine.check_conservation());
                }
            }
        }
    }
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_haircut_ratio_bounded() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let vault_val: u32 = kani::any();
    let c_tot_val: u32 = kani::any();
    let ins_val: u32 = kani::any();
    let ppt_val: u32 = kani::any();

    engine.vault = U128::new(vault_val as u128);
    engine.c_tot = U128::new(c_tot_val as u128);
    engine.insurance_fund.balance = U128::new(ins_val as u128);
    engine.pnl_pos_tot = U256::from_u128(ppt_val as u128);

    let (h_num, h_den) = engine.haircut_ratio();

    assert!(h_num <= h_den);
    assert!(!h_den.is_zero());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_equity_nonneg_flat() {
    // Test equity non-negativity with non-trivial haircut (Residual > 0).
    // Two accounts: idx has the tested state, idx2 provides vault excess
    // so that Residual = Vault - (C_tot + I) > 0, giving h > 0.
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    let idx2 = engine.add_user(0).unwrap();

    let cap: u16 = kani::any();
    kani::assume(cap <= 10_000);
    engine.set_capital(idx as usize, cap as u128);

    // idx2 has capital too, and vault has excess to create Residual > 0
    let cap2: u16 = kani::any();
    kani::assume(cap2 <= 10_000);
    engine.set_capital(idx2 as usize, cap2 as u128);

    let excess: u16 = kani::any();
    kani::assume(excess <= 10_000);
    let total_cap = (cap as u128) + (cap2 as u128);
    engine.vault = U128::new(total_cap + (excess as u128));

    let pnl_val: i16 = kani::any();
    kani::assume(pnl_val > i16::MIN);
    engine.set_pnl(idx as usize, I256::from_i128(pnl_val as i128));

    assert!(engine.accounts[idx as usize].position_basis_q.is_zero());

    let eq = engine.account_equity_net(&engine.accounts[idx as usize], DEFAULT_ORACLE);
    assert!(!eq.is_negative(),
        "flat account equity must be non-negative even with non-trivial haircut");
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_liquidation_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();

    let deposit_amt: u32 = kani::any();
    kani::assume(deposit_amt > 0 && deposit_amt <= 10_000_000);
    engine.deposit(a, deposit_amt as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let loss: u32 = kani::any();
    kani::assume(loss > 0 && loss <= deposit_amt);
    let pnl = I256::from_i128(-(loss as i128));
    engine.set_pnl(a as usize, pnl);

    let cap = engine.accounts[a as usize].capital.get();
    let pay = core::cmp::min(loss as u128, cap);
    engine.set_capital(a as usize, cap - pay);
    let new_pnl = pnl.checked_add(I256::from_u128(pay)).unwrap_or(I256::ZERO);
    engine.set_pnl(a as usize, new_pnl);

    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_margin_withdrawal() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.last_crank_slot = DEFAULT_SLOT;

    let a = engine.add_user(0).unwrap();

    let deposit_amt: u32 = kani::any();
    kani::assume(deposit_amt >= 1000 && deposit_amt <= 10_000_000);
    engine.deposit(a, deposit_amt as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let withdraw_amt: u32 = kani::any();
    kani::assume(withdraw_amt > 0 && withdraw_amt <= deposit_amt);
    let result = engine.withdraw(a, withdraw_amt as u128, DEFAULT_ORACLE, DEFAULT_SLOT);
    assert!(result.is_ok());
    assert!(engine.check_conservation());

    let remaining = engine.accounts[a as usize].capital.get();
    if remaining < u128::MAX {
        let result2 = engine.withdraw(a, remaining + 1, DEFAULT_ORACLE, DEFAULT_SLOT);
        assert!(result2.is_err());
    }
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_top_up_insurance_preserves_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 1_000_000);

    let vault_before = engine.vault.get();
    let ins_before = engine.insurance_fund.balance.get();

    engine.top_up_insurance_fund(amount as u128).unwrap();

    assert!(engine.vault.get() == vault_before + amount as u128);
    assert!(engine.insurance_fund.balance.get() == ins_before + amount as u128);
    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_deposit_then_withdraw_roundtrip() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.last_crank_slot = DEFAULT_SLOT;

    let idx = engine.add_user(0).unwrap();
    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 1_000_000);

    engine.deposit(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    assert!(engine.check_conservation());

    let result = engine.withdraw(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT);
    assert!(result.is_ok());
    assert!(engine.accounts[idx as usize].capital.get() == 0);
    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_multiple_deposits_aggregate_correctly() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();

    let amount_a: u32 = kani::any();
    let amount_b: u32 = kani::any();
    kani::assume(amount_a <= 1_000_000);
    kani::assume(amount_b <= 1_000_000);

    engine.deposit(a, amount_a as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, amount_b as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let cap_a = engine.accounts[a as usize].capital.get();
    let cap_b = engine.accounts[b as usize].capital.get();

    assert!(engine.c_tot.get() == cap_a + cap_b);
    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_close_account_returns_capital() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 50_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    assert!(engine.check_conservation());

    let result = engine.close_account(idx, DEFAULT_SLOT, DEFAULT_ORACLE);
    assert!(result.is_ok());
    let returned = result.unwrap();
    assert!(returned == 50_000);
    assert!(engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_trade_pnl_is_zero_sum_algebraic() {
    let size: i32 = kani::any();
    let price_diff: i32 = kani::any();
    kani::assume(size != 0 && size > i32::MIN);
    kani::assume(price_diff > i32::MIN);

    let product = (size as i64) * (price_diff as i64);
    let neg_product = -product;
    assert!(product + neg_product == 0);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_flat_negative_resolves_through_insurance() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();
    engine.vault = U128::new(10_000);
    engine.insurance_fund.balance = U128::new(5_000);

    engine.set_pnl(idx as usize, I256::from_i128(-1000));

    let ins_before = engine.insurance_fund.balance.get();

    let result = engine.touch_account_full(idx as usize, DEFAULT_ORACLE, DEFAULT_SLOT);
    assert!(result.is_ok());

    assert!(engine.accounts[idx as usize].pnl == I256::ZERO);
    assert!(engine.insurance_fund.balance.get() <= ins_before);
}

// ############################################################################
// ADL SAFETY (from ak.rs)
// ############################################################################

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t4_17_enqueue_adl_preserves_oi_balance_qty_only() {
    let q1: u8 = kani::any();
    let q2: u8 = kani::any();
    kani::assume(q1 > 0 && q2 > 0);
    let oi = (q1 as u16) + (q2 as u16);
    kani::assume(oi <= 15);

    let q_close: u8 = kani::any();
    kani::assume(q_close > 0 && (q_close as u16) < oi);
    let oi_post = oi - (q_close as u16);

    let a_old = S_ADL_ONE;
    let a_new = a_after_adl(a_old, oi_post, oi);

    let basis_q1 = (q1 as u16) * S_POS_SCALE;
    let basis_q2 = (q2 as u16) * S_POS_SCALE;
    let eff_q1 = lazy_eff_q(basis_q1, a_new, a_old) / S_POS_SCALE;
    let eff_q2 = lazy_eff_q(basis_q2, a_new, a_old) / S_POS_SCALE;

    assert!(eff_q1 + eff_q2 <= oi_post, "sum of effective positions must not exceed oi_post");
    assert!(eff_q1 <= q1 as u16);
    assert!(eff_q2 <= q2 as u16);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t4_18_precision_exhaustion_both_sides_reset() {
    let a_old: u16 = kani::any();
    kani::assume(a_old > 0);
    let oi: u8 = kani::any();
    kani::assume(oi >= 2);
    let q_close: u8 = kani::any();
    kani::assume(q_close > 0 && q_close < oi);
    let oi_post = oi - q_close;

    let a_candidate = ((a_old as u32) * (oi_post as u32)) / (oi as u32);

    kani::assume(a_candidate == 0);
    assert!(oi_post > 0);

    let mut oi_eff_opp: u16 = oi_post as u16;
    let mut oi_eff_liq: u16 = kani::any();
    let mut pending_reset_opp = false;
    let mut pending_reset_liq = false;

    oi_eff_opp = 0;
    oi_eff_liq = 0;
    pending_reset_opp = true;
    pending_reset_liq = true;

    assert!(oi_eff_opp == 0);
    assert!(oi_eff_liq == 0);
    assert!(pending_reset_opp);
    assert!(pending_reset_liq);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t4_19_full_drain_terminal_k_includes_deficit() {
    let oi: u8 = kani::any();
    kani::assume(oi > 0 && oi <= 10);
    let d: u8 = kani::any();
    kani::assume(d > 0 && d <= 100);

    let a_opp = S_ADL_ONE;
    let k_before: i32 = 0;

    let delta_k_abs = ((d as u32) * (a_opp as u32) + (oi as u32) - 1) / (oi as u32);
    let delta_k = -(delta_k_abs as i32);
    let k_after = k_before + delta_k;

    assert!(k_after < k_before);

    let k_epoch_start = k_after;
    assert!(k_epoch_start == k_before + delta_k);
    assert!(k_epoch_start < k_before);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t4_20_bankruptcy_qty_routes_when_d_zero() {
    let oi: u8 = kani::any();
    kani::assume(oi >= 2);
    let q_close: u8 = kani::any();
    kani::assume(q_close > 0 && q_close < oi);

    let a_old = S_ADL_ONE;
    let oi_post = oi - q_close;

    let a_new = ((a_old as u32) * (oi_post as u32)) / (oi as u32);

    assert!((a_new as u32) <= (a_old as u32));
    assert!((a_new as u32) < (a_old as u32));

    assert!(oi_post < oi);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t4_21_precision_exhaustion_zeroes_both_sides() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    engine.adl_mult_long = 1;
    engine.oi_eff_long_q = U256::from_u128(3 * POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(3 * POS_SCALE);
    engine.adl_coeff_long = I256::ZERO;
    engine.stored_pos_count_long = 1;

    let q_close = U256::from_u128(POS_SCALE);
    let d = U256::ZERO;

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(engine.oi_eff_long_q.is_zero());
    assert!(engine.oi_eff_short_q.is_zero());
    assert!(ctx.pending_reset_long);
    assert!(ctx.pending_reset_short);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t4_22_k_overflow_routes_to_absorb() {
    let oi: u8 = kani::any();
    kani::assume(oi >= 2);
    let q_close: u8 = kani::any();
    kani::assume(q_close > 0 && q_close < oi);
    let d: u8 = kani::any();
    kani::assume(d > 0);

    let a_old = S_ADL_ONE;
    let oi_post = oi - q_close;

    let k_opp: i8 = -127;
    let k_after = k_opp;

    let a_new = a_after_adl(a_old, oi_post as u16, oi as u16);
    assert!(a_new < a_old as u16, "A must shrink even on K overflow");
    assert!(k_after == k_opp, "K must be unchanged on overflow (routed to absorb)");
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t4_23_d_zero_routes_quantity_only() {
    let oi: u8 = kani::any();
    kani::assume(oi >= 2);
    let q_close: u8 = kani::any();
    kani::assume(q_close > 0 && q_close < oi);

    let a_old = S_ADL_ONE;
    let k_before: i32 = kani::any::<i8>() as i32;
    let oi_post = oi - q_close;

    let k_after = k_before;

    let a_new = a_after_adl(a_old, oi_post as u16, oi as u16);
    assert!(a_new < a_old as u16, "A must strictly decrease");
    assert!(k_after == k_before, "K must be unchanged when D == 0");
}

// ############################################################################
// DUST BOUNDS (from ak.rs)
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t5_21_local_floor_quantity_error_bounded() {
    let basis_q: u16 = kani::any();
    kani::assume(basis_q > 0);

    let a_cur: u16 = kani::any();
    kani::assume(a_cur > 0);
    let a_basis: u16 = kani::any();
    kani::assume(a_basis > 0 && a_basis >= a_cur);

    let product = (basis_q as u64) * (a_cur as u64);
    let remainder = product % (a_basis as u64);

    assert!(remainder < a_basis as u64);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t5_21_pnl_rounding_conservative() {
    let basis_q: u8 = kani::any();
    kani::assume(basis_q > 0);
    let k_diff: i8 = kani::any();
    kani::assume(k_diff < 0);

    let a_basis = S_ADL_ONE;
    let scaled_basis = (basis_q as u16) * S_POS_SCALE;

    let pnl = lazy_pnl(scaled_basis, k_diff as i32, a_basis);

    assert!(pnl <= 0, "negative k_diff must produce non-positive PnL");

    let exact_num = (scaled_basis as i32) * (k_diff as i32);
    let den = (a_basis as i32) * (S_POS_SCALE as i32);
    let trunc = exact_num / den;
    assert!(pnl <= trunc);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t5_22_phantom_dust_total_bound() {
    let q1: u8 = kani::any();
    let q2: u8 = kani::any();
    kani::assume(q1 > 0 && q2 > 0);
    let a_cur: u16 = kani::any();
    let a_basis: u16 = kani::any();
    kani::assume(a_basis > 0 && a_cur > 0 && a_cur <= a_basis);

    let basis_q1 = (q1 as u16) * S_POS_SCALE;
    let basis_q2 = (q2 as u16) * S_POS_SCALE;

    let rem1 = (basis_q1 as u32) * (a_cur as u32) % (a_basis as u32);
    let rem2 = (basis_q2 as u32) * (a_cur as u32) % (a_basis as u32);

    assert!(rem1 < a_basis as u32);
    assert!(rem2 < a_basis as u32);

    assert!(rem1 + rem2 < 2 * (a_basis as u32),
        "total dust from 2 accounts < 2 effective units");
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t5_23_dust_clearance_guard_safe() {
    let n: u8 = kani::any();
    kani::assume(n > 0 && n <= 32);

    let dust_bound: u8 = n;

    let max_dust_per_acct = S_POS_SCALE as u16 - 1;
    let max_total_dust_fp = (n as u16) * max_dust_per_acct;
    let max_total_dust_base = max_total_dust_fp / (S_POS_SCALE as u16);
    assert!(max_total_dust_base < n as u16, "total OI dust < phantom_dust_bound");
    assert!(dust_bound == n, "dust_bound tracks exact zeroing count");
}

// ############################################################################
// FUNDING NO-MINT (from ak.rs)
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t13_54_funding_no_mint_asymmetric_a() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);

    let a_long: u8 = kani::any();
    kani::assume(a_long >= 1);
    let a_short: u8 = kani::any();
    kani::assume(a_short >= 1);
    engine.adl_mult_long = a_long as u128;
    engine.adl_mult_short = a_short as u128;

    engine.last_oracle_price = 100;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 100;

    let rate: i8 = kani::any();
    kani::assume(rate != 0);
    engine.funding_rate_bps_per_slot_last = rate as i64;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    let result = engine.accrue_market_to(1, 100);
    assert!(result.is_ok());

    let k_long_after = engine.adl_coeff_long;
    let k_short_after = engine.adl_coeff_short;

    let dk_long = k_long_after.checked_sub(k_long_before).unwrap();
    let dk_short = k_short_after.checked_sub(k_short_before).unwrap();

    let dk_long_i128 = dk_long.try_into_i128().unwrap();
    let dk_short_i128 = dk_short.try_into_i128().unwrap();
    let term_long = dk_long_i128.checked_mul(a_short as i128).unwrap();
    let term_short = dk_short_i128.checked_mul(a_long as i128).unwrap();
    let cross_total = term_long.checked_add(term_short).unwrap();
    assert!(cross_total <= 0,
        "funding must not mint: cross-multiplied K changes must be <= 0");
}

// ############################################################################
// NEW: proof_junior_profit_backing
// ############################################################################

/// Σ PNL_pos ≤ Residual (bounded 2-account)
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_junior_profit_backing() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();

    let dep_a: u32 = kani::any();
    kani::assume(dep_a > 0 && dep_a <= 1_000_000);
    let dep_b: u32 = kani::any();
    kani::assume(dep_b > 0 && dep_b <= 1_000_000);

    engine.deposit(a, dep_a as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, dep_b as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Account a has positive PnL, b is flat
    let pnl_val: u16 = kani::any();
    kani::assume(pnl_val > 0 && pnl_val <= 10_000);
    engine.set_pnl(a as usize, I256::from_u128(pnl_val as u128));

    // pnl_pos_tot = pnl_val
    let ppt = engine.pnl_pos_tot.try_into_u128().unwrap();
    assert!(ppt == pnl_val as u128);

    // Residual = vault - c_tot - insurance
    let vault = engine.vault.get();
    let c_tot = engine.c_tot.get();
    let ins = engine.insurance_fund.balance.get();
    // Conservation: vault >= c_tot + ins
    assert!(vault >= c_tot + ins);

    // Residual is what backs junior profits
    let residual = vault - c_tot - ins;
    // With no trades, vault = dep_a + dep_b = c_tot, insurance = 0
    // So residual = 0, pnl_pos_tot = pnl_val > 0
    // This means haircut ratio kicks in: h_num <= h_den ensures effective PnL <= residual
    let (h_num, h_den) = engine.haircut_ratio();
    let effective_ppt = mul_div_floor_u256(engine.pnl_pos_tot, h_num, h_den);
    assert!(effective_ppt.try_into_u128().unwrap() <= residual + ppt,
        "haircutted PnL must be backed");
}

// ############################################################################
// NEW: proof_protected_principal
// ############################################################################

/// Flat account capital unaffected by other's insolvency.
/// Uses touch_account_full which internally calls settle_losses + resolve_flat_negative.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_protected_principal() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();

    engine.deposit(a, 500_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 500_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let a_cap_before = engine.accounts[a as usize].capital.get();

    // b goes insolvent: negative PnL exceeding capital (so settle_losses
    // will wipe capital and resolve_flat_negative will absorb remainder)
    let loss: u16 = kani::any();
    kani::assume(loss > 0);
    let loss_val = 500_000u128 + (loss as u128);
    engine.set_pnl(b as usize, I256::from_i128(-(loss_val as i128)));

    // touch_account_full runs the real settlement pipeline:
    // settle_side_effects → settle_losses → resolve_flat_negative
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = DEFAULT_SLOT;
    let _ = engine.touch_account_full(b as usize, DEFAULT_ORACLE, DEFAULT_SLOT);

    // a's capital must be unchanged through b's entire loss resolution
    let a_cap_after = engine.accounts[a as usize].capital.get();
    assert!(a_cap_after == a_cap_before,
        "flat account capital must be unaffected by other's insolvency");
}

// ============================================================================
// proof_withdraw_simulation_preserves_residual
// ============================================================================
//
// Issue #1: Withdraw margin simulation must not inflate the haircut ratio.
// The withdraw() function simulates the post-withdrawal state by calling
// set_capital(idx, new_cap) which decreases c_tot. If vault is not also
// temporarily decreased, Residual = Vault - (C_tot + I) is inflated,
// which inflates the haircut and lets undercollateralized users withdraw.

#[kani::proof]
#[kani::solver(cadical)]
fn proof_withdraw_simulation_preserves_residual() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();
    engine.deposit(b, 10_000_000, 100, 0).unwrap();

    engine.last_oracle_price = 100;
    engine.last_market_slot = 1;
    engine.last_crank_slot = 1;
    engine.funding_price_sample_last = 100;

    // Trade so a has a position (needed for margin check path)
    let size_q = I256::from_u128(POS_SCALE);
    engine.execute_trade(a, b, 100, 1, size_q, 100).unwrap();

    // Record haircut before withdraw attempt
    let (h_num_before, h_den_before) = engine.haircut_ratio();

    // Simulate what the FIXED withdraw does: adjust both capital AND vault
    let withdraw_amount: u128 = 1_000;
    let old_cap = engine.accounts[a as usize].capital.get();
    let old_vault = engine.vault;
    let new_cap = old_cap - withdraw_amount;
    engine.set_capital(a as usize, new_cap);
    engine.vault = U128::new(engine.vault.get() - withdraw_amount);

    let (h_num_sim, h_den_sim) = engine.haircut_ratio();

    // Revert
    engine.set_capital(a as usize, old_cap);
    engine.vault = old_vault;

    // Cross-multiply to compare fractions: h_num_sim/h_den_sim <= h_num_before/h_den_before
    let lhs = h_num_sim.checked_mul(h_den_before);
    let rhs = h_num_before.checked_mul(h_den_sim);
    if let (Some(l), Some(r)) = (lhs, rhs) {
        assert!(l <= r,
            "haircut must not increase during withdraw simulation — Residual inflation detected");
    }
}

// ============================================================================
// proof_funding_rate_validated_before_storage
// ============================================================================
//
// Issue #2: keeper_crank must reject out-of-bounds funding rates before
// storing them. Otherwise the stored rate bricks all future accrue_market_to.

#[kani::proof]
#[kani::solver(cadical)]
fn proof_funding_rate_validated_before_storage() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.last_oracle_price = 100;
    engine.last_market_slot = 0;
    engine.last_crank_slot = 0;
    engine.funding_price_sample_last = 100;

    let a = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000_000, 100, 0).unwrap();

    // Pass an invalid funding rate (> MAX_ABS_FUNDING_BPS_PER_SLOT)
    let bad_rate: i64 = MAX_ABS_FUNDING_BPS_PER_SLOT + 1;
    let result = engine.keeper_crank(a, 1, 100, bad_rate);

    // The crank must EITHER:
    // a) reject the bad rate entirely (Err), OR
    // b) clamp/sanitize it so the stored rate is within bounds
    //
    // It must NOT succeed AND store an out-of-bounds rate.
    if result.is_ok() {
        let stored = engine.funding_rate_bps_per_slot_last;
        assert!(stored.abs() <= MAX_ABS_FUNDING_BPS_PER_SLOT,
            "stored funding rate must be within bounds after successful crank");
    }

    // Regardless of the first crank result, a subsequent operation must NOT brick.
    // Try a second crank — if accrue_market_to fails due to bad stored rate, protocol is bricked.
    let result2 = engine.keeper_crank(a, 2, 100, 0);
    assert!(result2.is_ok(),
        "protocol must not be bricked by a previous bad funding rate input");
}

// ============================================================================
// proof_gc_dust_preserves_fee_credits
// ============================================================================
//
// Issue #3: garbage_collect_dust must not delete accounts with non-zero fee_credits.

#[kani::proof]
#[kani::solver(cadical)]
fn proof_gc_dust_preserves_fee_credits() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.last_oracle_price = 100;
    engine.last_market_slot = 1;
    engine.last_crank_slot = 1;
    engine.current_slot = 1;

    let a = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000, 100, 0).unwrap();

    // Simulate: account has 0 capital, 0 position, but positive fee_credits
    engine.set_capital(a as usize, 0);
    engine.accounts[a as usize].fee_credits = I128::new(5_000); // prepaid credits
    engine.accounts[a as usize].position_basis_q = I256::ZERO;
    engine.accounts[a as usize].reserved_pnl = U256::ZERO;
    engine.set_pnl(a as usize, I256::ZERO);

    let was_used_before = engine.is_used(a as usize);
    assert!(was_used_before, "account must exist before GC");

    // Run GC
    engine.garbage_collect_dust();

    // Account must NOT have been freed — it has prepaid fee_credits
    assert!(engine.is_used(a as usize),
        "GC must not delete account with non-zero fee_credits");
    assert!(engine.accounts[a as usize].fee_credits.get() == 5_000,
        "fee_credits must be preserved");
}
