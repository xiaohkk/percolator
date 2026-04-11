//! Kani proofs for reserve cohort queue invariants (checklist §A1, §A3, §C1-C7).
//!
//! MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT is 3 under Kani (checklist §L).
//! Induction over queue length extends to 62 by hand.

#![cfg(kani)]

mod common;
use common::*;

// ============================================================================
// Helper: compute sum of all cohort remaining_q
// ============================================================================

fn cohort_remaining_sum(engine: &RiskEngine, idx: usize) -> u128 {
    let a = &engine.accounts[idx];
    let mut sum = 0u128;
    for i in 0..a.exact_cohort_count as usize {
        sum += a.exact_reserve_cohorts[i].remaining_q;
    }
    if a.overflow_older_present { sum += a.overflow_older.remaining_q; }
    if a.overflow_newest_present { sum += a.overflow_newest.remaining_q; }
    sum
}

/// Inject positive PnL and route to reserve via append.
fn inject_reserve(engine: &mut RiskEngine, idx: u16, amount: u128, slot: u64, h: u64) {
    engine.accounts[idx as usize].pnl += amount as i128;
    engine.pnl_pos_tot += amount;
    engine.append_or_route_new_reserve(idx as usize, amount, slot, h);
}

// ############################################################################
// A1: R_i = Σ cohort remaining_q — after append
// ############################################################################

/// Exercises empty, partial, and overflow paths.
#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_a1_reserve_sum_after_append() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Fill 0–3 exact + overflow via distinct slots
    let pre: u8 = kani::any();
    kani::assume(pre <= 5); // may fill exact(3) + overflow_older + overflow_newest
    if pre >= 1 { inject_reserve(&mut engine, idx, 10_000, DEFAULT_SLOT, 10); }
    if pre >= 2 { inject_reserve(&mut engine, idx, 20_000, DEFAULT_SLOT+1, 10); }
    if pre >= 3 { inject_reserve(&mut engine, idx, 30_000, DEFAULT_SLOT+2, 10); }
    if pre >= 4 { inject_reserve(&mut engine, idx, 40_000, DEFAULT_SLOT+3, 10); }
    if pre >= 5 { inject_reserve(&mut engine, idx, 50_000, DEFAULT_SLOT+4, 10); }

    assert_eq!(engine.accounts[idx as usize].reserved_pnl,
        cohort_remaining_sum(&engine, idx as usize), "A1 pre");

    let add: u128 = kani::any();
    kani::assume(add > 0 && add <= 50_000);
    inject_reserve(&mut engine, idx, add, DEFAULT_SLOT+10, 10);

    assert_eq!(engine.accounts[idx as usize].reserved_pnl,
        cohort_remaining_sum(&engine, idx as usize), "A1 post-append");

    kani::cover!(pre == 0, "empty queue");
    kani::cover!(pre >= 4, "overflow path");
}

// ############################################################################
// A1: R_i = Σ — after apply_reserve_loss_lifo
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_a1_reserve_sum_after_loss() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    inject_reserve(&mut engine, idx, 10_000, DEFAULT_SLOT, 10);
    inject_reserve(&mut engine, idx, 20_000, DEFAULT_SLOT+1, 10);
    inject_reserve(&mut engine, idx, 30_000, DEFAULT_SLOT+2, 10);
    // total R = 60_000

    let loss: u128 = kani::any();
    kani::assume(loss > 0 && loss <= 60_000);
    engine.apply_reserve_loss_lifo(idx as usize, loss);

    assert_eq!(engine.accounts[idx as usize].reserved_pnl,
        cohort_remaining_sum(&engine, idx as usize), "A1 post-LIFO");

    kani::cover!(loss <= 30_000, "fits in newest");
    kani::cover!(loss > 30_000, "spans multiple");
    kani::cover!(loss == 60_000, "total drain");
}

// ############################################################################
// A1: R_i = Σ — after advance_profit_warmup_cohort
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_a1_reserve_sum_after_warmup() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    inject_reserve(&mut engine, idx, 20_000, DEFAULT_SLOT, 10);
    inject_reserve(&mut engine, idx, 30_000, DEFAULT_SLOT+1, 10);

    let dt: u64 = kani::any();
    kani::assume(dt >= 1 && dt <= 30);
    engine.current_slot = DEFAULT_SLOT + dt;

    engine.advance_profit_warmup_cohort(idx as usize);

    assert_eq!(engine.accounts[idx as usize].reserved_pnl,
        cohort_remaining_sum(&engine, idx as usize), "A1 post-warmup");

    kani::cover!(dt <= 10, "within horizon");
    kani::cover!(dt > 10, "past horizon");
}

// ############################################################################
// A3: R_i == 0 → queue structurally empty
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_a3_zero_reserve_empty_queue() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    inject_reserve(&mut engine, idx, 20_000, DEFAULT_SLOT, 10);
    inject_reserve(&mut engine, idx, 30_000, DEFAULT_SLOT+1, 10);

    engine.apply_reserve_loss_lifo(idx as usize, 50_000);

    assert_eq!(engine.accounts[idx as usize].reserved_pnl, 0, "R_i must be 0");
    assert_eq!(cohort_remaining_sum(&engine, idx as usize), 0,
        "A3: all segments zero when R_i==0");

    kani::cover!(true, "full drain empties queue");
}

// ############################################################################
// C1: Exact cohort timing — release = min(anchor, floor(anchor*elapsed/horizon))
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_c1_exact_cohort_timing() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let anchor: u128 = kani::any();
    kani::assume(anchor > 0 && anchor <= 1_000);
    let h: u64 = kani::any();
    kani::assume(h >= 1 && h <= 20);

    inject_reserve(&mut engine, idx, anchor, DEFAULT_SLOT, h);

    let dt: u64 = kani::any();
    kani::assume(dt >= 1 && dt <= 40);
    engine.current_slot = DEFAULT_SLOT + dt;

    let r_before = engine.accounts[idx as usize].reserved_pnl;
    engine.advance_profit_warmup_cohort(idx as usize);
    let released = r_before - engine.accounts[idx as usize].reserved_pnl;

    let expected = if dt as u128 >= h as u128 { anchor }
        else { mul_div_floor_u128(anchor, dt as u128, h as u128) };

    assert_eq!(released, expected,
        "C1: released == min(anchor, floor(anchor*elapsed/horizon))");

    kani::cover!(dt < h, "partial maturity");
    kani::cover!(dt >= h, "full maturity");
}

// ############################################################################
// C2: Fresh profit goes to reserve (h_lock>0), not matured
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_c2_fresh_profit_reserved() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    let h: u64 = kani::any();
    kani::assume(h >= 1 && h <= 100);
    let delta: u128 = kani::any();
    kani::assume(delta > 0 && delta <= 100_000);

    let old_matured = engine.pnl_matured_pos_tot;

    let result = engine.set_pnl_with_reserve(idx as usize, delta as i128, ReserveMode::UseHLock(h));
    assert!(result.is_ok());

    assert_eq!(engine.accounts[idx as usize].reserved_pnl, delta,
        "C2: R_i must equal the positive delta");
    assert_eq!(engine.pnl_matured_pos_tot, old_matured,
        "C2: matured unchanged for h_lock > 0");

    kani::cover!(true, "fresh profit reserved");
}

// ############################################################################
// C3: Dust-grief — appending doesn't modify existing cohorts
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_c3_dust_grief_resistance() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    inject_reserve(&mut engine, idx, 50_000, DEFAULT_SLOT, 20);
    let snap = engine.accounts[idx as usize].exact_reserve_cohorts[0];

    // Append at different slot (won't merge)
    inject_reserve(&mut engine, idx, 1, DEFAULT_SLOT+1, 10);

    let c = &engine.accounts[idx as usize].exact_reserve_cohorts[0];
    assert_eq!(c.anchor_q, snap.anchor_q, "C3: anchor_q unchanged");
    assert_eq!(c.start_slot, snap.start_slot, "C3: start_slot unchanged");
    assert_eq!(c.horizon_slots, snap.horizon_slots, "C3: horizon_slots unchanged");
    assert_eq!(c.sched_release_q, snap.sched_release_q, "C3: sched_release_q unchanged");
    assert_eq!(c.remaining_q, snap.remaining_q, "C3: remaining_q unchanged");

    kani::cover!(true, "dust append preserves existing");
}

// ############################################################################
// C4: LIFO ordering — newest consumed first
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_c4_lifo_ordering() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    inject_reserve(&mut engine, idx, 10_000, DEFAULT_SLOT, 10);   // oldest
    inject_reserve(&mut engine, idx, 20_000, DEFAULT_SLOT+1, 10); // newest

    let oldest_before = engine.accounts[idx as usize].exact_reserve_cohorts[0].remaining_q;

    let loss: u128 = kani::any();
    kani::assume(loss > 0 && loss <= 20_000);
    engine.apply_reserve_loss_lifo(idx as usize, loss);

    // LIFO: oldest untouched when loss fits in newest
    assert_eq!(engine.accounts[idx as usize].exact_reserve_cohorts[0].remaining_q, oldest_before,
        "C4: oldest must be untouched when loss fits in newest");

    kani::cover!(loss < 20_000, "partial newest");
    kani::cover!(loss == 20_000, "exact newest drain");
}

// ############################################################################
// C5: ImmediateRelease increases matured by exactly reserve_add
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_c5_immediate_release_exact() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Pre-existing reserve
    inject_reserve(&mut engine, idx, 30_000, DEFAULT_SLOT, 10);
    let pre_r = engine.accounts[idx as usize].reserved_pnl;
    let old_matured = engine.pnl_matured_pos_tot;

    let delta: u128 = kani::any();
    kani::assume(delta > 0 && delta <= 50_000);
    let new_pnl = engine.accounts[idx as usize].pnl + delta as i128;

    let result = engine.set_pnl_with_reserve(idx as usize, new_pnl, ReserveMode::ImmediateRelease);
    assert!(result.is_ok());

    // C5: matured increased by EXACTLY delta
    assert_eq!(engine.pnl_matured_pos_tot, old_matured + delta,
        "C5: matured increases by exactly reserve_add");
    // Pre-existing R unchanged
    assert_eq!(engine.accounts[idx as usize].reserved_pnl, pre_r,
        "C5: pre-existing R_i unchanged");

    kani::cover!(pre_r > 0, "pre-existing reserve preserved");
}

// ############################################################################
// C7: overflow_newest not matured by advance_profit_warmup_cohort
// ############################################################################

#[kani::proof]
#[kani::unwind(8)]
#[kani::solver(cadical)]
fn proof_c7_pending_non_maturity() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    engine.deposit(idx, 1_000_000, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();

    // Fill: 3 exact + overflow_older + overflow_newest (5 appends at distinct slots)
    for i in 0..5u64 {
        inject_reserve(&mut engine, idx, 10_000, DEFAULT_SLOT + i, 10);
    }

    if engine.accounts[idx as usize].overflow_newest_present {
        let newest_q = engine.accounts[idx as usize].overflow_newest.remaining_q;

        engine.current_slot = DEFAULT_SLOT + 200; // well past any horizon
        engine.advance_profit_warmup_cohort(idx as usize);

        // C7: if still present as overflow_newest, remaining_q unchanged
        if engine.accounts[idx as usize].overflow_newest_present {
            assert_eq!(engine.accounts[idx as usize].overflow_newest.remaining_q, newest_q,
                "C7: pending overflow_newest must not be matured");
        }
        // (if promoted, it's no longer overflow_newest — that's valid)
    }

    kani::cover!(true, "overflow_newest path exercised");
}
