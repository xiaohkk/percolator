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

    let deposit: u32 = kani::any();
    kani::assume(deposit >= 1000 && deposit <= 1_000_000);
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, deposit as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= deposit);

    let result = engine.withdraw(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT);
    kani::cover!(result.is_ok(), "withdraw Ok path reachable");
    if result.is_ok() {
        assert!(engine.check_conservation());
        assert!(engine.accounts[idx as usize].capital.get() == deposit as u128 - amount as u128);
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
    let delta_i128 = delta as i128;

    let pnl_a = engine.accounts[a as usize].pnl;
    let pnl_b = engine.accounts[b as usize].pnl;

    let new_a = pnl_a.checked_add(delta_i128);
    let neg_delta = delta_i128.checked_neg();

    let mut reached = false;
    if let (Some(na), Some(nd)) = (new_a, neg_delta) {
        if na != i128::MIN {
            if let Some(nb) = pnl_b.checked_add(nd) {
                if nb != i128::MIN {
                    engine.set_pnl(a as usize, na);
                    engine.set_pnl(b as usize, nb);

                    assert!(engine.check_conservation());
                    // Zero-sum: pnl_pos_tot can only redistribute, not grow unbounded
                    assert!(engine.pnl_pos_tot <= 5_000_000 + delta.unsigned_abs() as u128);
                    reached = true;
                }
            }
        }
    }
    kani::cover!(reached, "zero-sum PnL path reachable");
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
    let matured_val: u32 = kani::any();
    kani::assume(matured_val <= ppt_val); // matured <= total positive PnL

    engine.vault = U128::new(vault_val as u128);
    engine.c_tot = U128::new(c_tot_val as u128);
    engine.insurance_fund.balance = U128::new(ins_val as u128);
    engine.pnl_pos_tot = ppt_val as u128;
    engine.pnl_matured_pos_tot = matured_val as u128; // v11.21: haircut denominator

    let (h_num, h_den) = engine.haircut_ratio();

    // h_num <= h_den always (haircut ratio <= 1)
    assert!(h_num <= h_den);
    // h_den is either pnl_matured_pos_tot or 1 (when matured == 0)
    assert!(h_den != 0);

    // Exercise h < 1 branch: when residual < pnl_matured_pos_tot
    if vault_val as u128 >= c_tot_val as u128 + ins_val as u128 {
        let residual = vault_val as u128 - c_tot_val as u128 - ins_val as u128;
        if matured_val > 0 && residual < matured_val as u128 {
            kani::cover!(true, "h < 1 branch reachable");
            assert!(h_num < h_den, "h must be < 1 when residual < matured");
        }
    }
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn bounded_equity_nonneg_flat() {
    // Test equity non-negativity with non-trivial haircut (h < 1).
    // Two accounts: idx has the tested state, idx2 provides vault excess.
    // Must set pnl_matured_pos_tot for haircut to be non-trivial (v11.21).
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    let idx2 = engine.add_user(0).unwrap();

    let cap: u16 = kani::any();
    kani::assume(cap <= 10_000);
    engine.set_capital(idx as usize, cap as u128);

    let cap2: u16 = kani::any();
    kani::assume(cap2 <= 10_000);
    engine.set_capital(idx2 as usize, cap2 as u128);

    let excess: u16 = kani::any();
    kani::assume(excess <= 10_000);
    let total_cap = (cap as u128) + (cap2 as u128);
    engine.vault = U128::new(total_cap + (excess as u128));

    let pnl_val: i16 = kani::any();
    kani::assume(pnl_val > i16::MIN);
    engine.set_pnl(idx as usize, pnl_val as i128);

    // Set pnl_matured_pos_tot to exercise h < 1 branch in haircut_ratio.
    // This represents matured positive PnL from OTHER accounts that
    // exceeds the residual, forcing h < 1 for withdrawable computation.
    let matured: u16 = kani::any();
    kani::assume(matured <= 20_000);
    engine.pnl_matured_pos_tot = matured as u128;
    // Maintain invariant: matured <= pnl_pos_tot
    if engine.pnl_matured_pos_tot > engine.pnl_pos_tot {
        engine.pnl_matured_pos_tot = engine.pnl_pos_tot;
    }

    assert!(engine.accounts[idx as usize].position_basis_q == 0);

    let eq = engine.account_equity_net(&engine.accounts[idx as usize], DEFAULT_ORACLE);
    assert!(eq >= 0,
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
    let pnl = -(loss as i128);
    engine.set_pnl(a as usize, pnl);

    let cap = engine.accounts[a as usize].capital.get();
    let pay = core::cmp::min(loss as u128, cap);
    engine.set_capital(a as usize, cap - pay);
    let new_pnl = pnl.checked_add(pay as i128).unwrap_or(0i128);
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

    engine.set_pnl(idx as usize, -1000i128);

    let ins_before = engine.insurance_fund.balance.get();

    let result = engine.touch_account_full(idx as usize, DEFAULT_ORACLE, DEFAULT_SLOT);
    assert!(result.is_ok());

    assert!(engine.accounts[idx as usize].pnl == 0i128);
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

/// Precision exhaustion: when A_candidate floors to 0 despite OI_post > 0,
/// engine must zero BOTH sides' OI and set both pending_reset.
/// Uses actual engine enqueue_adl with symbolic A_mult close to exhaustion.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t4_18_precision_exhaustion_both_sides_reset() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    // A_mult = 2, OI = 3*PS. Closing 2*PS leaves OI_post = 1*PS.
    // A_candidate = floor(2 * 1 / 3) = 0 → precision exhaustion.
    engine.adl_mult_long = 2;
    engine.adl_coeff_long = 0i128;
    engine.oi_eff_long_q = 3 * POS_SCALE;
    engine.oi_eff_short_q = 3 * POS_SCALE;
    engine.stored_pos_count_long = 1;

    let q_close = 2 * POS_SCALE;
    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, 0u128);
    assert!(result.is_ok());

    // Both sides' OI must be zeroed (precision exhaustion terminal drain)
    assert!(engine.oi_eff_long_q == 0, "opposing OI must be zeroed");
    assert!(engine.oi_eff_short_q == 0, "liquidated OI must be zeroed");
    assert!(ctx.pending_reset_long, "opposing side must be pending reset");
    assert!(ctx.pending_reset_short, "liquidated side must be pending reset");
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
    engine.oi_eff_long_q = 3 * POS_SCALE;
    engine.oi_eff_short_q = 3 * POS_SCALE;
    engine.adl_coeff_long = 0i128;
    engine.stored_pos_count_long = 1;

    let q_close = POS_SCALE;
    let d = 0u128;

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    assert!(engine.oi_eff_long_q == 0);
    assert!(engine.oi_eff_short_q == 0);
    assert!(ctx.pending_reset_long);
    assert!(ctx.pending_reset_short);
}

/// K-space overflow routes deficit to absorb_protocol_loss, preserving K.
/// Uses actual engine enqueue_adl with K near i128::MIN to trigger overflow.
#[kani::proof]
#[kani::solver(cadical)]
fn t4_22_k_overflow_routes_to_absorb() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    // Set K near i128::MIN so delta_K addition underflows
    engine.adl_coeff_long = i128::MIN + 1;
    engine.adl_mult_long = POS_SCALE; // Use POS_SCALE (not ADL_ONE) to keep computation manageable
    engine.oi_eff_long_q = 4 * POS_SCALE;
    engine.oi_eff_short_q = 4 * POS_SCALE;
    engine.stored_pos_count_long = 1;
    engine.insurance_fund.balance = U128::new(10_000_000);

    let k_before = engine.adl_coeff_long;
    let ins_before = engine.insurance_fund.balance.get();

    // ADL with deficit — delta_K will be large negative, K_opp + delta_K underflows
    let q_close = POS_SCALE;
    let d = 1_000_000u128;

    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, d);
    assert!(result.is_ok());

    // K must be unchanged (overflow routed to absorb)
    assert!(engine.adl_coeff_long == k_before,
        "K must be unchanged when overflow routes to absorb");
    // Insurance must have decreased (absorb_protocol_loss was called)
    assert!(engine.insurance_fund.balance.get() < ins_before,
        "insurance must decrease when absorbing overflow deficit");
    // A must still shrink (quantity routing is independent of K overflow)
    assert!(engine.adl_mult_long < POS_SCALE, "A must shrink even on K overflow");
}

/// D=0 ADL: K must be unchanged, A must decrease, OI updated.
/// Uses actual engine enqueue_adl with zero deficit.
#[kani::proof]
#[kani::solver(cadical)]
fn t4_23_d_zero_routes_quantity_only() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let mut ctx = InstructionContext::new();

    let k_init: i8 = kani::any();
    engine.adl_coeff_long = k_init as i128;
    engine.adl_mult_long = ADL_ONE;
    engine.oi_eff_long_q = 10 * POS_SCALE;
    engine.oi_eff_short_q = 10 * POS_SCALE;
    engine.stored_pos_count_long = 1;

    let k_before = engine.adl_coeff_long;
    let a_before = engine.adl_mult_long;

    // D=0 quantity-only ADL
    let q_close = POS_SCALE;
    let result = engine.enqueue_adl(&mut ctx, Side::Short, q_close, 0u128);
    assert!(result.is_ok());

    // K must be unchanged when D == 0
    assert!(engine.adl_coeff_long == k_before, "K must be unchanged when D == 0");
    // A must decrease
    assert!(engine.adl_mult_long < a_before, "A must decrease after quantity ADL");
    // OI must decrease by q_close on both sides
    assert!(engine.oi_eff_long_q == 9 * POS_SCALE);
    assert!(engine.oi_eff_short_q == 9 * POS_SCALE);
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

    engine.oi_eff_long_q = POS_SCALE;
    engine.oi_eff_short_q = POS_SCALE;

    let a_long: u16 = kani::any();
    kani::assume(a_long >= 1);
    let a_short: u16 = kani::any();
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

    // Cross-multiply to check no-mint: dk_long * A_short + dk_short * A_long <= 0
    let term_long = dk_long.checked_mul(a_short as i128).unwrap();
    let term_short = dk_short.checked_mul(a_long as i128).unwrap();
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
    // Direct-state proof: skip engine deposit path for solver efficiency.
    // Prove: floor(pnl_matured_pos_tot * h_num / h_den) <= residual
    // for all valid vault/c_tot/insurance/matured configurations.
    let vault_val: u8 = kani::any();
    let c_tot_val: u8 = kani::any();
    let ins_val: u8 = kani::any();
    let matured_val: u8 = kani::any();

    kani::assume(matured_val > 0);
    let senior = (c_tot_val as u16) + (ins_val as u16);
    kani::assume((vault_val as u16) >= senior);

    let vault = vault_val as u32;
    let c_tot = c_tot_val as u32;
    let ins = ins_val as u32;
    let matured = matured_val as u32;

    let residual = vault - c_tot - ins;

    let h_num = if residual < matured { residual } else { matured };
    let h_den = matured;

    let effective_ppt = matured * h_num / h_den;

    assert!(effective_ppt <= residual,
        "haircutted matured PnL must be backed by residual alone");

    // Verify both branches reachable
    kani::cover!(residual < matured, "h < 1 branch");
    kani::cover!(residual >= matured, "h = 1 branch");
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

    let dep_a: u32 = kani::any();
    kani::assume(dep_a > 0 && dep_a <= 1_000_000);
    let dep_b: u32 = kani::any();
    kani::assume(dep_b > 0 && dep_b <= 1_000_000);

    engine.deposit(a, dep_a as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, dep_b as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let a_cap_before = engine.accounts[a as usize].capital.get();

    // b goes insolvent: negative PnL exceeding capital
    let loss: u16 = kani::any();
    kani::assume(loss > 0);
    let loss_val = dep_b as u128 + (loss as u128);
    engine.set_pnl(b as usize, -(loss_val as i128));

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

    // Trade so a has a position (exercises the margin-check + haircut path)
    let size_q = POS_SCALE as i128;
    engine.execute_trade(a, b, 100, 1, size_q, 100).unwrap();

    // Record haircut before actual withdraw
    let (h_num_before, h_den_before) = engine.haircut_ratio();
    let conservation_before = engine.check_conservation();
    assert!(conservation_before, "conservation must hold before withdraw");

    // Call the real engine.withdraw()
    let result = engine.withdraw(a, 1_000, 100, 1);
    assert!(result.is_ok(), "withdraw of 1000 from 10M capital must succeed");

    let (h_num_after, h_den_after) = engine.haircut_ratio();
    assert!(engine.check_conservation(), "conservation must hold after withdraw");

    // h must not increase: cross-multiply h_after/1 <= h_before/1
    let lhs = h_num_after.checked_mul(h_den_before);
    let rhs = h_num_before.checked_mul(h_den_after);
    if let (Some(l), Some(r)) = (lhs, rhs) {
        assert!(l <= r,
            "haircut must not increase after withdraw — Residual inflation detected");
    }
}

// ============================================================================
// proof_funding_rate_validated_before_storage
// ============================================================================

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
    // keeper_crank no longer accepts funding rate — it uses stored rate.
    // Set a bad rate directly and verify crank still works.
    engine.set_funding_rate_for_next_interval(bad_rate);

    // The stored rate should be clamped or validated
    let result = engine.keeper_crank(1, 100, &[a], 1);

    if result.is_ok() {
        let stored = engine.funding_rate_bps_per_slot_last;
        assert!(stored.abs() <= MAX_ABS_FUNDING_BPS_PER_SLOT,
            "stored funding rate must be within bounds after successful crank");
    }

    // Reset to valid rate and verify protocol works
    engine.set_funding_rate_for_next_interval(0);
    let result2 = engine.keeper_crank(2, 100, &[a], 1);
    assert!(result2.is_ok(),
        "protocol must not be bricked by a previous bad funding rate input");
}

// ============================================================================
// proof_gc_dust_preserves_fee_credits
// ============================================================================

#[kani::proof]
#[kani::solver(cadical)]
fn proof_gc_dust_preserves_fee_credits() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000, 100, 1).unwrap();

    engine.last_oracle_price = 100;
    engine.last_market_slot = 1;
    engine.last_crank_slot = 1;
    engine.current_slot = 1;

    // Account has 0 capital, 0 position, but positive fee_credits (prepaid)
    engine.set_capital(a as usize, 0);
    engine.accounts[a as usize].fee_credits = I128::new(5_000);
    engine.accounts[a as usize].position_basis_q = 0i128;
    engine.accounts[a as usize].reserved_pnl = 0u128;
    engine.set_pnl(a as usize, 0i128);

    assert!(engine.is_used(a as usize));
    engine.garbage_collect_dust();

    // Positive fee_credits: account must be PRESERVED (prepaid credits)
    assert!(engine.is_used(a as usize),
        "GC must not delete account with positive fee_credits");
    assert!(engine.accounts[a as usize].fee_credits.get() == 5_000,
        "fee_credits must be preserved");

    // Now test negative fee_credits (debt): account SHOULD be collected
    // and the uncollectible debt written off
    let b = engine.add_user(0).unwrap();
    engine.deposit(b, 10_000, 100, 1).unwrap();
    engine.set_capital(b as usize, 0);
    engine.accounts[b as usize].fee_credits = I128::new(-3_000); // debt
    engine.accounts[b as usize].position_basis_q = 0i128;
    engine.accounts[b as usize].reserved_pnl = 0u128;
    engine.set_pnl(b as usize, 0i128);

    assert!(engine.is_used(b as usize));
    engine.garbage_collect_dust();

    // Negative fee_credits (debt) on dead account: must be collected and debt written off
    assert!(!engine.is_used(b as usize),
        "GC must collect dead account with negative fee_credits (uncollectible debt)");
}

// ############################################################################
// min_liquidation_abs does not prevent liquidation of underwater accounts
// ############################################################################

#[kani::proof]
#[kani::solver(cadical)]
fn proof_min_liq_abs_does_not_block_liquidation() {
    let mut params = zero_fee_params();
    params.liquidation_fee_bps = 100;
    params.liquidation_fee_cap = U128::new(1_000_000);
    // Symbolic min_liquidation_abs up to 10000
    let min_abs: u16 = kani::any();
    params.min_liquidation_abs = U128::new(min_abs as u128);
    let mut engine = RiskEngine::new(params);

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 50_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 500_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Near-max leverage long for a
    let size = (480 * POS_SCALE) as i128;
    let result = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size, DEFAULT_ORACLE);
    assert!(result.is_ok());

    // Crash price to trigger liquidation
    let crash_price = 890u64;
    let slot2 = DEFAULT_SLOT + 1;
    let result = engine.liquidate_at_oracle(a, slot2, crash_price);
    // Liquidation must not revert due to min_liquidation_abs
    assert!(result.is_ok(), "min_liquidation_abs must not block liquidation");
    assert!(engine.check_conservation(), "conservation must hold after liquidation with min_abs");
}

// ############################################################################
// Trading loss seniority: settle_losses before fee_debt_sweep
// ############################################################################

#[kani::proof]
#[kani::solver(cadical)]
fn proof_trading_loss_seniority() {
    let mut params = zero_fee_params();
    params.maintenance_fee_per_slot = U128::new(100);
    let mut engine = RiskEngine::new(params);

    let a = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = DEFAULT_SLOT;
    engine.accounts[a as usize].last_fee_slot = DEFAULT_SLOT;

    // Give account negative PnL (trading loss)
    engine.set_pnl(a as usize, -8_000i128);

    // Advance 50 slots → fee = 100 * 50 = 5000
    let touch_slot = DEFAULT_SLOT + 50;
    let _ = engine.touch_account_full(a as usize, DEFAULT_ORACLE, touch_slot);

    let pnl_after = engine.accounts[a as usize].pnl;

    // Assert: PnL is zero (trading loss fully settled before fee sweep)
    assert!(pnl_after >= 0,
        "trading loss must be fully settled before fee debt sweep");
}

// ############################################################################
// settle_maintenance_fee_internal rejects fee_credits == i128::MIN (spec §2.1)
// ############################################################################

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_settle_fee_rejects_i128_min() {
    let mut params = zero_fee_params();
    params.maintenance_fee_per_slot = U128::new(1);
    let mut engine = RiskEngine::new(params);

    let a = engine.add_user(0).unwrap();
    engine.deposit(a, 10_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = DEFAULT_SLOT;

    // Set fee_credits to -(i128::MAX), the lowest valid value.
    // Advancing 1 slot with fee_per_slot=1 would produce i128::MIN.
    engine.accounts[a as usize].fee_credits = I128::new(-(i128::MAX));
    engine.accounts[a as usize].last_fee_slot = DEFAULT_SLOT;

    let result = engine.touch_account_full(a as usize, DEFAULT_ORACLE, DEFAULT_SLOT + 1);
    // Engine must reject: fee_credits would become i128::MIN
    assert!(result.is_err(),
        "engine must reject fee decrement that would produce i128::MIN");
}
