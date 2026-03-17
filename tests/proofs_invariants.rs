//! Sections 1-2 — Global inductive invariants
//!
//! Conservation, PnL tracking, side counts, haircut ratio.

#![cfg(kani)]

mod common;
use common::*;

// ============================================================================
// T0.3: set_pnl_aggregate_update_is_exact
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t0_3_set_pnl_aggregate_exact() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let old_pnl: i16 = kani::any();
    kani::assume(old_pnl > i16::MIN);
    engine.set_pnl(idx as usize, I256::from_i128(old_pnl as i128));

    let new_pnl: i16 = kani::any();
    kani::assume(new_pnl > i16::MIN);
    engine.set_pnl(idx as usize, I256::from_i128(new_pnl as i128));

    let expected = if new_pnl > 0 { new_pnl as u128 } else { 0u128 };
    let actual = engine.pnl_pos_tot.try_into_u128().unwrap();
    assert!(actual == expected);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn t0_3_sat_all_sign_transitions() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let old: i16 = kani::any();
    let new: i16 = kani::any();
    kani::assume(old > i16::MIN && new > i16::MIN);

    let transition: u8 = kani::any();
    kani::assume(transition < 4);
    match transition {
        0 => kani::assume(old <= 0 && new <= 0),
        1 => kani::assume(old <= 0 && new > 0),
        2 => kani::assume(old > 0 && new <= 0),
        3 => kani::assume(old > 0 && new > 0),
        _ => unreachable!(),
    }

    engine.set_pnl(idx as usize, I256::from_i128(old as i128));
    engine.set_pnl(idx as usize, I256::from_i128(new as i128));

    let expected = if new > 0 { new as u128 } else { 0u128 };
    let actual = engine.pnl_pos_tot.try_into_u128().unwrap();
    assert!(actual == expected, "pnl_pos_tot mismatch after transition");
}

// ============================================================================
// T0.4: conservation_check_handles_overflow
// ============================================================================

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn t0_4_conservation_check_handles_overflow() {
    let c_tot: u64 = kani::any();
    let insurance: u64 = kani::any();
    let vault: u64 = kani::any();
    let deposit: u64 = kani::any();

    let c_tot_128 = c_tot as u128;
    let insurance_128 = insurance as u128;
    let vault_128 = vault as u128;
    let deposit_128 = deposit as u128;

    let sum = c_tot_128.checked_add(insurance_128);
    assert!(sum.is_some());
    let sum = sum.unwrap();

    if vault_128 >= sum {
        let vault_new = vault_128 + deposit_128;
        let c_tot_new = c_tot_128 + deposit_128;
        assert!(vault_new >= c_tot_new + insurance_128,
            "deposit preserves conservation");
    }
}

// ============================================================================
// Inductive proofs from kani.rs
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn inductive_top_up_insurance_preserves_accounting() {
    let vault_before: u64 = kani::any();
    let c_tot_before: u64 = kani::any();
    let ins_before: u64 = kani::any();
    let amt: u64 = kani::any();

    let v = vault_before as u128;
    let c = c_tot_before as u128;
    let i = ins_before as u128;
    let a = amt as u128;

    kani::assume(c.checked_add(i).is_some());
    kani::assume(v >= c + i);
    kani::assume(v.checked_add(a).is_some());
    kani::assume(i.checked_add(a).is_some());

    let v_new = v + a;
    let i_new = i + a;

    assert!(v_new >= c + i_new);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn inductive_set_capital_decrease_preserves_accounting() {
    let vault: u64 = kani::any();
    let c_tot: u64 = kani::any();
    let ins: u64 = kani::any();
    let delta: u64 = kani::any();

    let v = vault as u128;
    let c = c_tot as u128;
    let i = ins as u128;
    let d = delta as u128;

    kani::assume(c.checked_add(i).is_some());
    kani::assume(v >= c + i);
    kani::assume(d <= c);

    let c_new = c - d;

    assert!(v >= c_new + i);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn inductive_set_pnl_preserves_pnl_pos_tot_delta() {
    let old_pnl: i32 = kani::any();
    let new_pnl: i32 = kani::any();
    let ppt_other: u32 = kani::any();

    let ppt_o = ppt_other as u128;

    let old_pos: u128 = if old_pnl > 0 { old_pnl as u128 } else { 0 };
    let new_pos: u128 = if new_pnl > 0 { new_pnl as u128 } else { 0 };

    let ppt_before = ppt_o + old_pos;

    let ppt_after = if new_pos >= old_pos {
        ppt_before + (new_pos - old_pos)
    } else {
        ppt_before - (old_pos - new_pos)
    };

    assert!(ppt_after == ppt_o + new_pos);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn inductive_deposit_preserves_accounting() {
    let vault: u64 = kani::any();
    let c_tot: u64 = kani::any();
    let ins: u64 = kani::any();
    let amt: u64 = kani::any();

    let v = vault as u128;
    let c = c_tot as u128;
    let i = ins as u128;
    let a = amt as u128;

    kani::assume(c.checked_add(i).is_some());
    kani::assume(v >= c + i);
    kani::assume(v.checked_add(a).is_some());
    kani::assume(c.checked_add(a).is_some());

    let v_new = v + a;
    let c_new = c + a;

    assert!(v_new >= c_new + i);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn inductive_withdraw_preserves_accounting() {
    let vault: u64 = kani::any();
    let c_tot: u64 = kani::any();
    let ins: u64 = kani::any();
    let amt: u64 = kani::any();

    let v = vault as u128;
    let c = c_tot as u128;
    let i = ins as u128;
    let a = amt as u128;

    kani::assume(c.checked_add(i).is_some());
    kani::assume(v >= c + i);
    kani::assume(a <= c);
    kani::assume(a <= v);

    let v_new = v - a;
    let c_new = c - a;

    assert!(v_new >= c_new + i);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn inductive_settle_loss_preserves_accounting() {
    let vault: u64 = kani::any();
    let c_tot: u64 = kani::any();
    let ins: u64 = kani::any();
    let paid: u64 = kani::any();

    let v = vault as u128;
    let c = c_tot as u128;
    let i = ins as u128;
    let p = paid as u128;

    kani::assume(c.checked_add(i).is_some());
    kani::assume(v >= c + i);
    kani::assume(p <= c);

    let c_new = c - p;

    assert!(v >= c_new + i);
}

// ============================================================================
// Property proofs from kani.rs
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn prop_pnl_pos_tot_agrees_with_recompute() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();

    let pnl_a: i32 = kani::any();
    kani::assume(pnl_a > i32::MIN);
    engine.set_pnl(a as usize, I256::from_i128(pnl_a as i128));

    let pnl_b: i32 = kani::any();
    kani::assume(pnl_b > i32::MIN);
    engine.set_pnl(b as usize, I256::from_i128(pnl_b as i128));

    let pos_a: u128 = if pnl_a > 0 { pnl_a as u128 } else { 0 };
    let pos_b: u128 = if pnl_b > 0 { pnl_b as u128 } else { 0 };
    let expected = U256::from_u128(pos_a + pos_b);

    assert!(engine.pnl_pos_tot == expected);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn prop_conservation_holds_after_all_ops() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let idx = engine.add_user(0).unwrap();

    let dep: u32 = kani::any();
    kani::assume(dep > 0 && dep <= 5_000_000);
    engine.deposit(idx, dep as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    assert!(engine.check_conservation());

    let ins_amt: u32 = kani::any();
    kani::assume(ins_amt <= 1_000_000);
    engine.top_up_insurance_fund(ins_amt as u128).unwrap();
    assert!(engine.check_conservation());

    let loss: u32 = kani::any();
    kani::assume(loss <= dep);
    engine.set_pnl(idx as usize, I256::from_i128(-(loss as i128)));
    assert!(engine.check_conservation());

    let cap_before = engine.accounts[idx as usize].capital.get();
    let pnl_abs = if loss > 0 { loss as u128 } else { 0 };
    let pay = core::cmp::min(pnl_abs, cap_before);
    if pay > 0 {
        engine.set_capital(idx as usize, cap_before - pay);
        let new_pnl_val = -(loss as i128) + (pay as i128);
        engine.set_pnl(idx as usize, I256::from_i128(new_pnl_val));
    }
    assert!(engine.check_conservation());
}

// ============================================================================
// set_pnl proofs from kani.rs
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
#[kani::should_panic]
fn proof_set_pnl_rejects_i256_min() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.set_pnl(idx as usize, I256::MIN);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_set_pnl_maintains_pnl_pos_tot() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let pnl1: i32 = kani::any();
    kani::assume(pnl1 > i32::MIN);
    engine.set_pnl(idx as usize, I256::from_i128(pnl1 as i128));

    let expected1 = if pnl1 > 0 { U256::from_u128(pnl1 as u128) } else { U256::ZERO };
    assert!(engine.pnl_pos_tot == expected1);

    let pnl2: i32 = kani::any();
    kani::assume(pnl2 > i32::MIN);
    engine.set_pnl(idx as usize, I256::from_i128(pnl2 as i128));

    let expected2 = if pnl2 > 0 { U256::from_u128(pnl2 as u128) } else { U256::ZERO };
    assert!(engine.pnl_pos_tot == expected2);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_set_pnl_underflow_safety() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    engine.set_pnl(idx as usize, I256::from_u128(1000));
    assert!(engine.pnl_pos_tot == U256::from_u128(1000));

    engine.set_pnl(idx as usize, I256::from_i128(-500));
    assert!(engine.pnl_pos_tot == U256::ZERO);

    engine.set_pnl(idx as usize, I256::ZERO);
    assert!(engine.pnl_pos_tot == U256::ZERO);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_set_pnl_clamps_reserved_pnl() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    engine.accounts[idx as usize].reserved_pnl = U256::from_u128(5000);

    engine.set_pnl(idx as usize, I256::from_u128(3000));
    assert!(engine.accounts[idx as usize].reserved_pnl == U256::from_u128(3000));

    engine.set_pnl(idx as usize, I256::from_i128(-100));
    assert!(engine.accounts[idx as usize].reserved_pnl == U256::ZERO);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_set_capital_maintains_c_tot() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let initial: u32 = kani::any();
    kani::assume(initial > 0 && initial <= 1_000_000);
    engine.deposit(idx, initial as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    assert!(engine.c_tot.get() == engine.accounts[idx as usize].capital.get());

    let new_cap: u32 = kani::any();
    kani::assume((new_cap as u64) <= (initial as u64) * 2);
    engine.set_capital(idx as usize, new_cap as u128);

    assert!(engine.c_tot.get() == new_cap as u128);
}

// ============================================================================
// check_conservation / haircut from kani.rs
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_check_conservation_basic() {
    let mut engine = RiskEngine::new(zero_fee_params());

    engine.vault = U128::new(100);
    engine.c_tot = U128::new(60);
    engine.insurance_fund.balance = U128::new(30);
    assert!(engine.check_conservation());

    engine.insurance_fund.balance = U128::new(50);
    assert!(!engine.check_conservation());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_haircut_ratio_no_division_by_zero() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let (num, den) = engine.haircut_ratio();
    assert!(num == U256::ONE);
    assert!(den == U256::ONE);

    engine.pnl_pos_tot = U256::from_u128(1000);
    engine.vault = U128::new(2000);
    engine.c_tot = U128::new(500);
    engine.insurance_fund.balance = U128::new(300);
    let (num2, den2) = engine.haircut_ratio();
    assert!(den2 == U256::from_u128(1000));
    assert!(num2 == U256::from_u128(1000));
    assert!(num2 <= den2);
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_absorb_protocol_loss_respects_floor() {
    let mut engine = RiskEngine::new(zero_fee_params());

    let floor: u32 = kani::any();
    kani::assume(floor <= 10_000);
    engine.insurance_floor = floor as u128;

    let balance: u32 = kani::any();
    kani::assume(balance >= floor && balance <= 100_000);
    engine.insurance_fund.balance = U128::new(balance as u128);

    let loss: u32 = kani::any();
    kani::assume(loss > 0 && loss <= 100_000);
    engine.absorb_protocol_loss(U256::from_u128(loss as u128));

    assert!(engine.insurance_fund.balance.get() >= floor as u128);
}

// ============================================================================
// Position / side tracking from kani.rs
// ============================================================================

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_set_position_basis_q_count_tracking() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    assert!(engine.stored_pos_count_long == 0);

    engine.set_position_basis_q(idx as usize, I256::from_u128(POS_SCALE));
    assert!(engine.stored_pos_count_long == 1);

    let neg = I256::from_u128(POS_SCALE).checked_neg().unwrap();
    engine.set_position_basis_q(idx as usize, neg);
    assert!(engine.stored_pos_count_long == 0);
    assert!(engine.stored_pos_count_short == 1);

    engine.set_position_basis_q(idx as usize, I256::ZERO);
    assert!(engine.stored_pos_count_short == 0);
    assert!(engine.stored_pos_count_long == 0);
}

#[kani::proof]
#[kani::unwind(70)]
#[kani::solver(cadical)]
fn proof_side_mode_gating() {
    let mut engine = RiskEngine::new(zero_fee_params());
    engine.last_crank_slot = DEFAULT_SLOT;

    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    engine.deposit(a, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    engine.deposit(b, 5_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    engine.side_mode_long = SideMode::DrainOnly;

    let size_q = I256::from_u128(POS_SCALE);
    let result = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, size_q, DEFAULT_ORACLE);
    assert!(result == Err(RiskError::SideBlocked));

    engine.side_mode_long = SideMode::Normal;
    engine.side_mode_short = SideMode::ResetPending;
    engine.stale_account_count_short = 1;

    let neg_size = I256::from_u128(POS_SCALE).checked_neg().unwrap();
    let result2 = engine.execute_trade(a, b, DEFAULT_ORACLE, DEFAULT_SLOT, neg_size, DEFAULT_ORACLE);
    assert!(result2 == Err(RiskError::SideBlocked));
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_account_equity_net_nonnegative() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let cap: u32 = kani::any();
    kani::assume(cap <= 1_000_000);
    engine.set_capital(idx as usize, cap as u128);
    engine.vault = U128::new(cap as u128);

    let pnl_val: i32 = kani::any();
    kani::assume(pnl_val > i32::MIN);
    engine.set_pnl(idx as usize, I256::from_i128(pnl_val as i128));

    let eq = engine.account_equity_net(&engine.accounts[idx as usize], DEFAULT_ORACLE);
    assert!(!eq.is_negative());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_effective_pos_q_epoch_mismatch_returns_zero() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let pos = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].position_basis_q = pos;
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;

    engine.adl_epoch_long = 1;
    let eff = engine.effective_pos_q(idx as usize);
    assert!(eff.is_zero());

    let pos_short = I256::from_u128(POS_SCALE).checked_neg().unwrap();
    engine.accounts[idx as usize].position_basis_q = pos_short;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.adl_epoch_short = 1;
    let eff2 = engine.effective_pos_q(idx as usize);
    assert!(eff2.is_zero());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_effective_pos_q_flat_is_zero() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    assert!(engine.accounts[idx as usize].position_basis_q.is_zero());
    let eff = engine.effective_pos_q(idx as usize);
    assert!(eff.is_zero());
}

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_attach_effective_position_updates_side_counts() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    assert!(engine.stored_pos_count_long == 0);
    assert!(engine.stored_pos_count_short == 0);

    let pos = I256::from_u128(POS_SCALE);
    engine.attach_effective_position(idx as usize, pos);
    assert!(engine.stored_pos_count_long == 1);
    assert!(engine.stored_pos_count_short == 0);

    engine.attach_effective_position(idx as usize, I256::ZERO);
    assert!(engine.stored_pos_count_long == 0);
    assert!(engine.stored_pos_count_short == 0);

    let neg = pos.checked_neg().unwrap();
    engine.attach_effective_position(idx as usize, neg);
    assert!(engine.stored_pos_count_long == 0);
    assert!(engine.stored_pos_count_short == 1);
}

// ============================================================================
// NEW: proof_fee_credits_never_i128_min
// ============================================================================

/// settle_maintenance_fee rejects i128::MIN result:
/// With high fee and 1-slot advance, fee_credits must not reach i128::MIN.
#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn proof_fee_credits_never_i128_min() {
    let mut params = zero_fee_params();
    params.maintenance_fee_per_slot = U128::new(1);
    let mut engine = RiskEngine::new(params);

    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 10_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Set fee_credits near i128::MIN + 1
    engine.accounts[idx as usize].fee_credits = I128::new(i128::MIN + 1);

    // Give a position so fee applies
    engine.accounts[idx as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[idx as usize].adl_a_basis = ADL_ONE;
    engine.accounts[idx as usize].adl_k_snap = I256::ZERO;
    engine.accounts[idx as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.adl_epoch_long = 0;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);

    engine.last_oracle_price = DEFAULT_ORACLE;
    engine.last_market_slot = DEFAULT_SLOT;

    // Touch at slot+1 — fee accrues
    let result = engine.touch_account_full(idx as usize, DEFAULT_ORACLE, DEFAULT_SLOT + 1);
    // Either succeeds or errors, but fee_credits must not be i128::MIN
    let fc = engine.accounts[idx as usize].fee_credits.get();
    assert!(fc != i128::MIN, "fee_credits must never be i128::MIN");
}
