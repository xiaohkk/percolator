//! Pure decision logic for the keeper tick — no RPC, no async, no clocks.
//!
//! The runtime (`main.rs`) fetches slab bytes + oracle bytes + current slot
//! from RPC, feeds them into `decide()`, receives an `Action`, then executes
//! the matching on-chain instruction. Keeping the decision pure makes the
//! ProgramTest harness tractable: tests construct an in-memory slab state
//! (either via the real instructions or by poking engine fields) and
//! directly call `decide()` to assert the chosen action.

use solana_program::pubkey::Pubkey;

/// Snapshot of one slab + its referenced oracle feed, captured at `now_slot`.
/// Enough for the keeper to decide without calling back into RPC.
#[derive(Clone, Debug)]
pub struct SlabSnapshot {
    pub slab: Pubkey,
    pub now_slot: u64,
    /// Raw engine bytes (beginning at `ENGINE_OFFSET`).
    pub engine_bytes: Vec<u8>,
    /// Oracle price currently published by the feed, or `None` if stale
    /// (`now_slot - last_update_slot > STALE_SLOTS`).
    pub oracle_price: Option<u64>,
    /// `last_update_slot` from the feed. The keeper uses this to decide
    /// whether to fire `Oracle.Update` before anything else.
    pub oracle_last_update_slot: u64,
    /// `STALE_SLOTS` threshold copied from the oracle crate.
    pub stale_slots: u64,
    /// How many slots the funding crank lags before we should fire it.
    /// The keeper config sets this; 64 is the default in production.
    pub funding_staleness_slots: u64,
}

/// Action the keeper should submit next for this slab. `Skip` = do nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `Oracle.Update(feed, source)`. Must run before any price-dependent
    /// action on this slab.
    RefreshOracle,
    /// `Liquidate(victim_slot)`. The only user-targeting action.
    Liquidate {
        victim_slot: u16,
        /// Estimated lamports-of-token bounty. Runtime compares this
        /// against the tx fee estimate and refuses to submit if
        /// fee > estimated_bounty.
        estimated_bounty: u128,
    },
    /// `Crank(Funding)` — advance accrual.
    CrankFunding,
    /// `Crank(AdlReset)` — advance the side-mode state machine on any
    /// side that's transition-ready.
    CrankAdlReset,
    /// `Crank(Gc)` — reclaim dust slots.
    CrankGc,
    /// Nothing to do this tick.
    Skip,
}

/// The bounty basis points applied by `process_liquidate` (copy — keep in
/// sync with `percolator_program::processor::LIQ_BOUNTY_BPS`).
pub const LIQ_BOUNTY_BPS: u128 = 50;

/// Minimum bounty the keeper will take a tx for. Prevents a fee loop where
/// the keeper pays more in SOL tx fees than it earns in the reward token.
/// Set in caller units (mint-native). The main loop lets operators override.
pub const MIN_BOUNTY_THRESHOLD: u128 = 1_000;

/// Decide one action for a slab snapshot. Ordering:
///
/// 1. If the oracle is stale → RefreshOracle. Every downstream decision
///    needs a fresh price.
/// 2. Scan accounts; if any is below maintenance margin → Liquidate.
/// 3. If funding hasn't accrued recently → CrankFunding.
/// 4. If any side is DrainOnly with zero OI (ripe for ResetPending) →
///    CrankAdlReset.
/// 5. If any non-LP slot holds dust → CrankGc.
/// 6. Else Skip.
pub fn decide(snap: &SlabSnapshot) -> Action {
    // 1. Stale oracle.
    if snap.oracle_price.is_none()
        || snap.now_slot.saturating_sub(snap.oracle_last_update_slot) > snap.stale_slots
    {
        return Action::RefreshOracle;
    }
    let oracle_price = snap.oracle_price.unwrap();

    // Cast the bytes into an engine reference. SAFETY: the byte buffer is
    // laid out by `RiskEngine::init_in_place`; callers of this library must
    // pass exactly `ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()`.
    let engine: &percolator::RiskEngine =
        unsafe { &*(snap.engine_bytes.as_ptr() as *const percolator::RiskEngine) };

    // 2. Liquidation candidate scan.
    //
    // The engine's `is_above_maintenance_margin` would be the exact gate,
    // but it reads `account.pnl` which is the realized PnL at the
    // engine's last settle. Without calling `accrue_market_to +
    // touch_account_live_local` (both `&mut self`) the keeper can't
    // update that field on a snapshot. Instead we run a conservative
    // mark-to-market: if the un-settled notional movement alone eats the
    // maintenance buffer, flag it. Final correctness is the engine's
    // problem — it will reject `AccountHealthy` if we guessed wrong.
    //
    // Formula: capital + pnl - |basis_q * oracle / POS_SCALE| * mm_bps/10000
    // must be positive. If negative → probably underwater.
    let max = (engine.params.max_accounts as usize).min(percolator::MAX_ACCOUNTS);
    let mm_bps = engine.params.maintenance_margin_bps as u128;
    const POS_SCALE: u128 = 1_000_000;
    // Slot 0 is the protocol LP — never liquidated by the keeper.
    for i in 1..max {
        if !engine.is_used(i) {
            continue;
        }
        let acct = &engine.accounts[i];
        let basis = acct.position_basis_q;
        if basis == 0 {
            continue;
        }
        // Conservative notional and MM requirement at the live oracle price.
        let abs_basis = basis.unsigned_abs();
        let notional = abs_basis
            .saturating_mul(oracle_price as u128)
            / POS_SCALE;
        let mm_req = notional.saturating_mul(mm_bps) / 10_000;

        // Keeper-side equity proxy: capital + realized pnl (signed).
        // Any adverse price move since the last settle isn't yet reflected
        // in pnl, so this over-estimates equity on the wrong side. To
        // compensate, we subtract a marked-loss guess using `adl_a_basis`
        // as a 1e15-scaled notional-at-attach reference. Cheap heuristic;
        // the engine is still the final arbiter.
        let equity_low: i128 = {
            let cap_i = acct.capital.get() as i128;
            cap_i.saturating_add(acct.pnl)
        };
        // Marked loss guess: if long (basis>0) and oracle < reference,
        // loss ~ notional * (ref - oracle) / ref. Reference isn't directly
        // stored, so we compare against what the engine "thought" the
        // price was at last accrue (`last_oracle_price`) — if the current
        // oracle has moved against the position since then, that's the
        // unrealized loss.
        let last_px = engine.last_oracle_price as u128;
        let marked_loss: u128 = if last_px > 0 {
            let (ref_px, live_px) = if basis > 0 {
                (last_px, oracle_price as u128)
            } else {
                (oracle_price as u128, last_px)
            };
            if live_px < ref_px {
                abs_basis.saturating_mul(ref_px - live_px) / POS_SCALE
            } else {
                0
            }
        } else {
            0
        };

        let est_equity = equity_low.saturating_sub(marked_loss as i128);
        if est_equity >= mm_req as i128 {
            continue;
        }

        let est = acct.capital.get().saturating_mul(LIQ_BOUNTY_BPS) / 10_000;
        if est < MIN_BOUNTY_THRESHOLD {
            continue;
        }
        return Action::Liquidate {
            victim_slot: i as u16,
            estimated_bounty: est,
        };
    }

    // 3. Funding stale.
    if snap.now_slot.saturating_sub(engine.last_market_slot) > snap.funding_staleness_slots {
        return Action::CrankFunding;
    }

    // 4. ADL reset ready — DrainOnly with zero OI on that side is a
    //    candidate for the Normal/ResetPending transition.
    if engine.side_mode_long == percolator::SideMode::DrainOnly && engine.oi_eff_long_q == 0
        || engine.side_mode_short == percolator::SideMode::DrainOnly && engine.oi_eff_short_q == 0
    {
        return Action::CrankAdlReset;
    }

    // 5. GC dust.
    let min_cap = engine.params.min_initial_deposit.get();
    for i in 1..max {
        if !engine.is_used(i) {
            continue;
        }
        let a = &engine.accounts[i];
        if a.position_basis_q == 0
            && a.pnl == 0
            && a.reserved_pnl == 0
            && a.sched_present == 0
            && a.pending_present == 0
            && a.capital.get() < min_cap
        {
            return Action::CrankGc;
        }
    }

    Action::Skip
}
