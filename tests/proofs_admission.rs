//! v12.18 admission-pair + sticky h_max + touch acceleration proofs (§4.7, §4.9)
//!
//! Proof groups:
//!   AH: Admission with pair + sticky rule (§4.7)
//!   AC: Acceleration on touch (§4.9)
//!   IN: Instruction-level invariants specific to v12.18

#![cfg(kani)]

mod common;
use common::*;

// ============================================================================
// AH-1: Single admission returns exactly admit_h_min or admit_h_max.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ah1_single_admission_range() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    // Inject some vault/c_tot to make residual non-degenerate
    engine.vault = U128::new(1000);
    engine.c_tot = U128::new(500);

    let fresh: u8 = kani::any();
    kani::assume(fresh > 0);

    let admit_h_min: u8 = kani::any();
    let admit_h_max: u8 = kani::any();
    kani::assume(admit_h_min as u64 <= admit_h_max as u64);
    kani::assume(admit_h_max > 0);
    kani::assume(admit_h_max as u64 <= engine.params.h_max);

    let mut ctx = InstructionContext::new_with_admission(
        admit_h_min as u64, admit_h_max as u64);

    let h_eff = engine.admit_fresh_reserve_h_lock(
        idx as usize, fresh as u128, &mut ctx,
        admit_h_min as u64, admit_h_max as u64);

    // Returned horizon is exactly one of the two inputs
    assert!(h_eff == admit_h_min as u64 || h_eff == admit_h_max as u64);

    // Admission law check
    let senior = engine.c_tot.get() + engine.insurance_fund.balance.get();
    let residual = engine.vault.get().saturating_sub(senior);
    let matured_plus_fresh = engine.pnl_matured_pos_tot.saturating_add(fresh as u128);
    if matured_plus_fresh <= residual {
        assert!(h_eff == admit_h_min as u64);
    } else {
        assert!(h_eff == admit_h_max as u64);
        assert!(ctx.is_h_max_sticky(idx));
    }
}

// ============================================================================
// AH-2: Sticky-H_max is absorbing. Once sticky, always returns admit_h_max.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ah2_sticky_is_absorbing() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.vault = U128::new(10_000); // plenty of residual — admission WOULD normally give h_min

    let admit_h_min: u8 = kani::any();
    let admit_h_max: u8 = kani::any();
    kani::assume((admit_h_min as u64) < (admit_h_max as u64)); // non-degenerate
    kani::assume(admit_h_max > 0);
    kani::assume(admit_h_max as u64 <= engine.params.h_max);

    let mut ctx = InstructionContext::new_with_admission(
        admit_h_min as u64, admit_h_max as u64);
    // Force idx into sticky set
    ctx.mark_h_max_sticky(idx);

    let fresh: u8 = kani::any();
    kani::assume(fresh > 0);

    let h_eff = engine.admit_fresh_reserve_h_lock(
        idx as usize, fresh as u128, &mut ctx,
        admit_h_min as u64, admit_h_max as u64);

    // Sticky forces h_max regardless of residual
    assert!(h_eff == admit_h_max as u64);
    assert!(ctx.is_h_max_sticky(idx));
}

// ============================================================================
// AH-3: No under-admission (v12.18 core fix).
// After first admission forces h_max, second call on same account cannot
// return h_min even if current state would suggest it.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ah3_no_under_admission() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    // Start constrained: residual = 0 so first fresh triggers h_max
    engine.vault = U128::new(100);
    engine.c_tot = U128::new(100);
    engine.pnl_matured_pos_tot = 0;

    let admit_h_min: u8 = kani::any();
    let admit_h_max: u8 = kani::any();
    kani::assume((admit_h_min as u64) < (admit_h_max as u64));
    kani::assume(admit_h_max > 0);
    kani::assume(admit_h_max as u64 <= engine.params.h_max);

    let mut ctx = InstructionContext::new_with_admission(
        admit_h_min as u64, admit_h_max as u64);

    // First admission: residual = 0, any positive fresh overflows → h_max
    let fresh1: u8 = kani::any();
    kani::assume(fresh1 > 0);
    let h1 = engine.admit_fresh_reserve_h_lock(
        idx as usize, fresh1 as u128, &mut ctx,
        admit_h_min as u64, admit_h_max as u64);
    assert!(h1 == admit_h_max as u64);
    assert!(ctx.is_h_max_sticky(idx));

    // Simulate arbitrary state evolution: residual could grow huge
    engine.vault = U128::new(u128::MAX / 2);

    // Second admission: state now admits h_min, but sticky forces h_max
    let fresh2: u8 = kani::any();
    kani::assume(fresh2 > 0);
    let h2 = engine.admit_fresh_reserve_h_lock(
        idx as usize, fresh2 as u128, &mut ctx,
        admit_h_min as u64, admit_h_max as u64);
    assert!(h2 == admit_h_max as u64);
}

// ============================================================================
// AH-4: h_min=0 admission preserves h=1 invariant.
// If admission returns 0 and caller instantly matures, residual still >= matured.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ah4_hmin_zero_preserves_h_equals_one() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    // Small bounded values
    let v: u16 = kani::any();
    let ct: u16 = kani::any();
    kani::assume(ct as u128 <= v as u128);
    engine.vault = U128::new(v as u128);
    engine.c_tot = U128::new(ct as u128);
    let matured: u16 = kani::any();
    let residual = (v as u128).saturating_sub(ct as u128);
    kani::assume(matured as u128 <= residual); // precondition: h = 1
    engine.pnl_matured_pos_tot = matured as u128;
    engine.pnl_pos_tot = matured as u128;

    let admit_h_min = 0u64;
    let admit_h_max: u8 = kani::any();
    kani::assume(admit_h_max > 0);
    kani::assume(admit_h_max as u64 <= engine.params.h_max);
    let mut ctx = InstructionContext::new_with_admission(
        admit_h_min, admit_h_max as u64);

    let fresh: u8 = kani::any();
    kani::assume(fresh > 0);

    let h_eff = engine.admit_fresh_reserve_h_lock(
        idx as usize, fresh as u128, &mut ctx,
        admit_h_min, admit_h_max as u64);

    if h_eff == 0 {
        // Simulate §4.8 clause 10: instant release
        let new_matured = engine.pnl_matured_pos_tot.saturating_add(fresh as u128);
        let senior = engine.c_tot.get() + engine.insurance_fund.balance.get();
        let new_residual = engine.vault.get().saturating_sub(senior);
        // h = 1 still holds
        assert!(new_matured <= new_residual);
    }
}

// ============================================================================
// AH-5: Cross-account sticky isolation.
// Sticky set for account a does NOT force h_max for account b.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ah5_cross_account_sticky_isolation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let a = engine.add_user(0).unwrap();
    let b = engine.add_user(0).unwrap();
    // Healthy residual: admission would give h_min
    engine.vault = U128::new(10_000);
    engine.c_tot = U128::new(0);

    let admit_h_min: u8 = kani::any();
    let admit_h_max: u8 = kani::any();
    kani::assume((admit_h_min as u64) < (admit_h_max as u64));
    kani::assume(admit_h_max > 0);
    kani::assume(admit_h_max as u64 <= engine.params.h_max);

    let mut ctx = InstructionContext::new_with_admission(
        admit_h_min as u64, admit_h_max as u64);
    // Mark only a sticky
    ctx.mark_h_max_sticky(a);

    // Admission for b: should return h_min since b is NOT sticky
    let fresh_b: u8 = kani::any();
    kani::assume(fresh_b > 0);
    kani::assume(fresh_b as u128 <= 100); // stays under residual

    let h_b = engine.admit_fresh_reserve_h_lock(
        b as usize, fresh_b as u128, &mut ctx,
        admit_h_min as u64, admit_h_max as u64);
    assert!(h_b == admit_h_min as u64);
    // b not sticky (h_min was returned)
    assert!(!ctx.is_h_max_sticky(b));
}

// ============================================================================
// AH-6: admit_h_min > 0 is a floor. Result is never below admit_h_min.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ah6_positive_hmin_floor() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();

    let admit_h_min: u8 = kani::any();
    kani::assume(admit_h_min > 0);
    let admit_h_max: u8 = kani::any();
    kani::assume(admit_h_min as u64 <= admit_h_max as u64);
    kani::assume(admit_h_max as u64 <= engine.params.h_max);

    let mut ctx = InstructionContext::new_with_admission(
        admit_h_min as u64, admit_h_max as u64);

    let fresh: u8 = kani::any();
    kani::assume(fresh > 0);

    let h_eff = engine.admit_fresh_reserve_h_lock(
        idx as usize, fresh as u128, &mut ctx,
        admit_h_min as u64, admit_h_max as u64);

    // Result >= admit_h_min (never below the floor)
    assert!(h_eff >= admit_h_min as u64);
}

// ============================================================================
// AC-1: Acceleration is all-or-nothing.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ac1_acceleration_all_or_nothing() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap() as usize;

    // Set up account with scheduled bucket
    let r: u8 = kani::any();
    kani::assume(r > 0);
    engine.accounts[idx].reserved_pnl = r as u128;
    engine.accounts[idx].pnl = r as i128;
    engine.pnl_pos_tot = r as u128;
    engine.accounts[idx].sched_present = 1;
    engine.accounts[idx].sched_remaining_q = r as u128;
    engine.accounts[idx].sched_anchor_q = r as u128;
    engine.accounts[idx].sched_horizon = 10;
    engine.accounts[idx].sched_start_slot = 0;

    let r_before = engine.accounts[idx].reserved_pnl;
    let matured_before = engine.pnl_matured_pos_tot;
    let sched_start_before = engine.accounts[idx].sched_start_slot;
    let sched_horizon_before = engine.accounts[idx].sched_horizon;

    // Arbitrary vault/c_tot state
    let v: u16 = kani::any();
    let ct: u16 = kani::any();
    engine.vault = U128::new(v as u128);
    engine.c_tot = U128::new(ct as u128);

    let result = engine.admit_outstanding_reserve_on_touch(idx);

    if result.is_ok() {
        let r_after = engine.accounts[idx].reserved_pnl;
        let matured_after = engine.pnl_matured_pos_tot;

        // Either accelerated (all reserve cleared) or unchanged
        let accelerated = r_after == 0 && r_before > 0;
        let unchanged = r_after == r_before && matured_after == matured_before;

        assert!(accelerated || unchanged);

        if accelerated {
            // All moved to matured
            assert!(matured_after == matured_before + r_before);
            // Buckets cleared
            assert!(engine.accounts[idx].sched_present == 0);
            assert!(engine.accounts[idx].pending_present == 0);
        } else {
            // Bucket fields preserved byte-identical
            assert!(engine.accounts[idx].sched_start_slot == sched_start_before);
            assert!(engine.accounts[idx].sched_horizon == sched_horizon_before);
        }
    }
}

// ============================================================================
// AC-2: Acceleration fires iff state admits.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ac2_acceleration_fires_iff_admits() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap() as usize;

    let r: u8 = kani::any();
    engine.accounts[idx].reserved_pnl = r as u128;
    engine.accounts[idx].pnl = r as i128;
    engine.pnl_pos_tot = r as u128;
    if r > 0 {
        engine.accounts[idx].sched_present = 1;
        engine.accounts[idx].sched_remaining_q = r as u128;
        engine.accounts[idx].sched_anchor_q = r as u128;
        engine.accounts[idx].sched_horizon = 10;
    }

    let v: u16 = kani::any();
    let ct: u16 = kani::any();
    engine.vault = U128::new(v as u128);
    engine.c_tot = U128::new(ct as u128);
    let matured: u8 = kani::any();
    engine.pnl_matured_pos_tot = matured as u128;
    kani::assume(engine.pnl_matured_pos_tot <= engine.pnl_pos_tot);

    let r_before = engine.accounts[idx].reserved_pnl;
    let residual = (v as u128).saturating_sub(ct as u128);
    let admits = r_before > 0
        && (matured as u128).saturating_add(r_before) <= residual;

    let _ = engine.admit_outstanding_reserve_on_touch(idx);

    let r_after = engine.accounts[idx].reserved_pnl;
    let fired = r_after == 0 && r_before > 0;

    // Fired iff state admitted
    if admits {
        assert!(fired);
    } else {
        assert!(!fired || r_before == 0);
    }
}

// ============================================================================
// AC-4: Acceleration preserves conservation & matured monotonicity.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn ac4_acceleration_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap() as usize;

    let r: u8 = kani::any();
    engine.accounts[idx].reserved_pnl = r as u128;
    engine.accounts[idx].pnl = r as i128;
    engine.pnl_pos_tot = r as u128;
    if r > 0 {
        engine.accounts[idx].sched_present = 1;
        engine.accounts[idx].sched_remaining_q = r as u128;
        engine.accounts[idx].sched_anchor_q = r as u128;
        engine.accounts[idx].sched_horizon = 10;
    }

    let v: u16 = kani::any();
    let ct: u16 = kani::any();
    kani::assume(ct as u128 <= v as u128); // conservation precondition
    engine.vault = U128::new(v as u128);
    engine.c_tot = U128::new(ct as u128);

    let matured_before = engine.pnl_matured_pos_tot;

    let _ = engine.admit_outstanding_reserve_on_touch(idx);

    // Matured monotone non-decreasing
    assert!(engine.pnl_matured_pos_tot >= matured_before);
    // Matured <= total pos
    assert!(engine.pnl_matured_pos_tot <= engine.pnl_pos_tot);
    // Vault conservation (V doesn't change)
    assert!(engine.vault.get() == v as u128);
    // V >= C_tot + I
    let senior = engine.c_tot.get() + engine.insurance_fund.balance.get();
    assert!(engine.vault.get() >= senior);
}

// ============================================================================
// IN-1: No live bypass via ImmediateReleaseResolvedOnly.
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn in1_no_live_immediate_release() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap() as usize;
    // Live mode (default on new engine)

    let new_pnl: u8 = kani::any();
    kani::assume(new_pnl > 0);

    // Snapshot state before
    let pnl_before = engine.accounts[idx].pnl;
    let pnl_pos_before = engine.pnl_pos_tot;

    let result = engine.set_pnl_with_reserve(
        idx, new_pnl as i128, ReserveMode::ImmediateReleaseResolvedOnly, None);

    // Must fail on Live
    assert!(result.is_err());
    // State unchanged
    assert!(engine.accounts[idx].pnl == pnl_before);
    assert!(engine.pnl_pos_tot == pnl_pos_before);
}

// ============================================================================
// K-9: validate_admission_pair rejects admit_h_max == 0 (Bug 9)
// Prevents wrapper bypass of admission by passing (0, 0).
// ============================================================================

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn k9_admission_pair_rejects_zero_max() {
    let engine = RiskEngine::new(zero_fee_params());
    let admit_h_min: u8 = kani::any();
    let admit_h_max = 0u64;
    let r = RiskEngine::validate_admission_pair(
        admit_h_min as u64, admit_h_max, &engine.params);
    assert!(r.is_err());
}
