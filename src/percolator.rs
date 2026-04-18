//! Formally Verified Risk Engine for Perpetual DEX — v12.18.0
//!
//! Implements the v12.18.0 spec.
//!
//! This module implements a formally verified risk engine that guarantees:
//! 1. Protected principal for flat accounts
//! 2. PNL warmup prevents instant withdrawal of manipulated profits
//! 3. ADL via lazy A/K side indices on the opposing OI side
//! 4. Conservation of funds across all operations (V >= C_tot + I)
//! 5. Bankruptcy socialization primarily through explicit A/K state. In the rare
//!    case of K-space i128 overflow during ADL, the remaining deficit falls to
//!    implicit global haircut (h) rather than panicking — preserving liquidation
//!    liveness at the cost of reducing the opposing side's junior PnL claims.
//!
//! # Atomicity Model
//!
//! Public functions suffixed with `_not_atomic` can return `Err` after partial
//! state mutation. **Callers MUST abort the entire transaction on `Err`** —
//! they must not retry, suppress, or continue with mutated state.
//!
//! On Solana SVM, any `Err` return from an instruction aborts the transaction
//! and rolls back all account state automatically. This is the expected
//! deployment model.
//!
//! Public functions WITHOUT the suffix (`top_up_insurance_fund`,
//! `deposit_fee_credits`, `accrue_market_to`) use validate-then-mutate:
//! `Err` means no state was changed.
//!
//! Internal helpers (`enqueue_adl`, `liquidate_at_oracle_internal`, etc.)
//! are not individually atomic — they rely on the calling `_not_atomic`
//! method to propagate `Err` to the transaction boundary.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(kani)]
extern crate kani;

// ============================================================================
// Conditional visibility macro
// ============================================================================

// ============================================================================
// Conditional visibility macro
// ============================================================================

/// Internal methods that proof harnesses and integration tests need direct
/// access to. Private in production builds, `pub` under test/kani.
/// Each invocation emits two mutually-exclusive cfg-gated copies of the same
/// function: one `pub`, one private.
macro_rules! test_visible {
    (
        $(#[$meta:meta])*
        fn $name:ident($($args:tt)*) $(-> $ret:ty)? $body:block
    ) => {
        $(#[$meta])*
        #[cfg(any(feature = "test", feature = "stress", kani))]
        pub fn $name($($args)*) $(-> $ret)? $body

        $(#[$meta])*
        #[cfg(not(any(feature = "test", feature = "stress", kani)))]
        fn $name($($args)*) $(-> $ret)? $body
    };
}

// ============================================================================
// Constants
// ============================================================================

#[cfg(kani)]
pub const MAX_ACCOUNTS: usize = 4;

#[cfg(all(feature = "test", not(kani)))]
pub const MAX_ACCOUNTS: usize = 64;

#[cfg(all(not(kani), not(feature = "test")))]
pub const MAX_ACCOUNTS: usize = 4096;

pub const BITMAP_WORDS: usize = (MAX_ACCOUNTS + 63) / 64;
pub const MAX_ROUNDING_SLACK: u128 = MAX_ACCOUNTS as u128;
const ACCOUNT_IDX_MASK: usize = MAX_ACCOUNTS - 1;
const _: () = assert!(MAX_ACCOUNTS.is_power_of_two());

pub const GC_CLOSE_BUDGET: u32 = 32;
pub const ACCOUNTS_PER_CRANK: u16 = 128;
pub const LIQ_BUDGET_PER_CRANK: u16 = 64;

/// POS_SCALE = 1_000_000 (spec §1.2)
pub const POS_SCALE: u128 = 1_000_000;

/// ADL_ONE = 1_000_000 (spec §1.3)
pub const ADL_ONE: u128 = 1_000_000_000_000_000;

/// MIN_A_SIDE = 1_000 (spec §1.4)
pub const MIN_A_SIDE: u128 = 100_000_000_000_000;

/// MAX_ORACLE_PRICE = 1_000_000_000_000 (spec §1.4)
pub const MAX_ORACLE_PRICE: u64 = 1_000_000_000_000;

/// FUNDING_DEN = 1_000_000_000 (spec v12.15 §5.4)
pub const FUNDING_DEN: u128 = 1_000_000_000;

/// MAX_ABS_FUNDING_E9_PER_SLOT = 1_000_000_000 (spec §1.4, parts-per-billion)
pub const MAX_ABS_FUNDING_E9_PER_SLOT: i128 = 1_000_000_000;

// Normative bounds (spec §1.4)
pub const MAX_VAULT_TVL: u128 = 10_000_000_000_000_000;
pub const MAX_POSITION_ABS_Q: u128 = 100_000_000_000_000;
pub const MAX_ACCOUNT_NOTIONAL: u128 = 100_000_000_000_000_000_000;
pub const MAX_TRADE_SIZE_Q: u128 = MAX_POSITION_ABS_Q; // spec §1.4
pub const MAX_OI_SIDE_Q: u128 = 100_000_000_000_000;
pub const MAX_MATERIALIZED_ACCOUNTS: u64 = 1_000_000;
pub const MAX_ACCOUNT_POSITIVE_PNL: u128 = 100_000_000_000_000_000_000_000_000_000_000;
pub const MAX_PNL_POS_TOT: u128 = 100_000_000_000_000_000_000_000_000_000_000_000_000;
pub const MAX_TRADING_FEE_BPS: u64 = 10_000;
pub const MAX_MARGIN_BPS: u64 = 10_000;
pub const MAX_LIQUIDATION_FEE_BPS: u64 = 10_000;
pub const MAX_PROTOCOL_FEE_ABS: u128 = 1_000_000_000_000_000_000_000_000_000_000_000_000; // 10^36, spec §1.4

pub const MAX_WARMUP_SLOTS: u64 = u64::MAX;
pub const MAX_RESOLVE_PRICE_DEVIATION_BPS: u64 = 10_000;

// ============================================================================
// BPF-Safe 128-bit Types
// ============================================================================
pub mod i128;
pub use i128::{I128, U128};

// ============================================================================
// Wide 256-bit Arithmetic (used for transient intermediates only)
// ============================================================================
pub mod wide_math;
use wide_math::{
    U256, I256,
    mul_div_floor_u128, mul_div_ceil_u128,
    wide_mul_div_floor_u128,
    wide_signed_mul_div_floor_from_k_pair,
    wide_mul_div_ceil_u128_or_over_i128max, OverI128Magnitude,
    fee_debt_u128_checked,
    mul_div_floor_u256_with_rem,
    ceil_div_positive_checked,
};

// ============================================================================
// Core Data Structures
// ============================================================================

// AccountKind as plain u8 — eliminates UB risk from invalid enum discriminants
// when casting raw slab bytes to &Account via zero-copy. u8 has no invalid
// representations, so &*(ptr as *const Account) is always sound.
// pub enum AccountKind { User = 0, LP = 1 }  // replaced by constants below

/// Market mode (spec §2.2)
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketMode {
    Live = 0,
    Resolved = 1,
}

/// Resolve-branch selector for `resolve_market_not_atomic` (spec §9.8 v12.18.5).
///
/// Explicit selector per Goal 51: "the ordinary vs degenerate resolve_market
/// branch MUST be chosen only from an explicit trusted wrapper mode input.
/// Equality of economic values such as `live_oracle_price == P_last` or
/// `funding_rate_e9_per_slot == 0` MUST NOT by itself force the degenerate
/// branch." Value-based branch inference is forbidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveMode {
    /// Self-synchronizing live-sync branch. Accrues market to `now_slot` using
    /// the supplied `live_oracle_price` and `funding_rate_e9_per_slot`, then
    /// enforces the deviation-band check against `resolved_price`.
    Ordinary = 0,
    /// Privileged recovery branch. Skips additional live accrual after
    /// `slot_last` and skips the deviation-band check. MUST be entered only
    /// when the wrapper explicitly selects it AND supplies `live_oracle_price
    /// == P_last` AND `funding_rate_e9_per_slot == 0`.
    Degenerate = 1,
}

/// Reserve mode for set_pnl (spec §4.8)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReserveMode {
    /// Admission-pair: engine decides h_eff from (h_min, h_max) at reserve creation time
    UseAdmissionPair(u64, u64),
    /// Immediate release, only valid in Resolved mode (fails on Live)
    ImmediateReleaseResolvedOnly,
    /// Positive increase is forbidden (returns Err)
    NoPositiveIncreaseAllowed,
}

/// Side mode for OI sides (spec §2.4)
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideMode {
    Normal = 0,
    DrainOnly = 1,
    ResetPending = 2,
}

/// Max accounts that can be touched in a single instruction
pub const MAX_TOUCHED_PER_INSTRUCTION: usize = 64;

/// Instruction context for deferred reset scheduling (spec §5.7-5.8)
/// and shared touched-account tracking (spec §7.8, v12.14.0).
pub struct InstructionContext {
    pub pending_reset_long: bool,
    pub pending_reset_short: bool,
    /// Shared admission pair for this instruction
    pub admit_h_min_shared: u64,
    pub admit_h_max_shared: u64,
    /// Deduplicated touched accounts (ascending order)
    pub touched_accounts: [u16; MAX_TOUCHED_PER_INSTRUCTION],
    pub touched_count: u8,
    /// Per-instruction sticky set: accounts that required admit_h_max
    pub h_max_sticky_accounts: [u16; MAX_TOUCHED_PER_INSTRUCTION],
    pub h_max_sticky_count: u8,
}

impl InstructionContext {
    pub fn new() -> Self {
        Self {
            pending_reset_long: false,
            pending_reset_short: false,
            admit_h_min_shared: 0,
            admit_h_max_shared: 0,
            touched_accounts: [0; MAX_TOUCHED_PER_INSTRUCTION],
            touched_count: 0,
            h_max_sticky_accounts: [0; MAX_TOUCHED_PER_INSTRUCTION],
            h_max_sticky_count: 0,
        }
    }

    pub fn new_with_admission(admit_h_min: u64, admit_h_max: u64) -> Self {
        Self {
            pending_reset_long: false,
            pending_reset_short: false,
            admit_h_min_shared: admit_h_min,
            admit_h_max_shared: admit_h_max,
            touched_accounts: [0; MAX_TOUCHED_PER_INSTRUCTION],
            touched_count: 0,
            h_max_sticky_accounts: [0; MAX_TOUCHED_PER_INSTRUCTION],
            h_max_sticky_count: 0,
        }
    }

    /// Check if account is in sticky set
    pub fn is_h_max_sticky(&self, idx: u16) -> bool {
        let count = self.h_max_sticky_count as usize;
        for i in 0..count {
            if self.h_max_sticky_accounts[i] == idx { return true; }
        }
        false
    }

    /// Insert account into sticky set
    pub fn mark_h_max_sticky(&mut self, idx: u16) -> bool {
        if self.is_h_max_sticky(idx) { return true; }
        let count = self.h_max_sticky_count as usize;
        if count < MAX_TOUCHED_PER_INSTRUCTION {
            self.h_max_sticky_accounts[count] = idx;
            self.h_max_sticky_count += 1;
            true
        } else {
            false
        }
    }

    /// Add account to touched set if not already present
    pub fn add_touched(&mut self, idx: u16) -> bool {
        let count = self.touched_count as usize;
        for i in 0..count {
            if self.touched_accounts[i] == idx { return true; } // dedup
        }
        if count < MAX_TOUCHED_PER_INSTRUCTION {
            self.touched_accounts[count] = idx;
            self.touched_count += 1;
            true
        } else {
            false // capacity exceeded — caller MUST fail
        }
    }
}

/// Unified account (spec §2.1)
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Account {
    pub capital: U128,
    /// Wrapper-owned account-kind annotation (spec §2.1.1, non-normative).
    /// The engine stores and canonicalizes `kind` but MUST NOT read it for
    /// any spec-normative decision (margin, liquidation, fees, accrual,
    /// resolution). `is_lp()` / `is_user()` are wrapper conveniences only.
    pub kind: u8,  // 0 = User, 1 = LP

    /// Realized PnL (i128, spec §2.1)
    pub pnl: i128,

    /// Reserved positive PnL (u128, spec §2.1)
    pub reserved_pnl: u128,

    /// Signed fixed-point base quantity basis (i128, spec §2.1)
    pub position_basis_q: i128,

    /// Side multiplier snapshot at last explicit position attachment (u128)
    pub adl_a_basis: u128,

    /// K coefficient snapshot (i128)
    pub adl_k_snap: i128,

    /// Per-account funding snapshot at last attachment (v12.15)
    pub f_snap: i128,

    /// Side epoch snapshot
    pub adl_epoch_snap: u64,

    /// Wrapper-owned matching-engine bindings (spec §2.1.1, non-normative).
    /// Opaque payload stored by the engine but never read for any
    /// spec-normative decision. Typical use: CPI routing by the wrapper's
    /// LP/matching-engine integration.
    pub matcher_program: [u8; 32],
    pub matcher_context: [u8; 32],

    /// Wrapper-owned owner pubkey (spec §2.1.1, non-normative).
    /// Authorization is a wrapper responsibility; the engine never reads
    /// `owner` for any spec-normative decision. `set_owner` is a defensive
    /// helper that preserves the "zero iff unclaimed" convention — it
    /// refuses to overwrite a nonzero owner and refuses to write zero.
    pub owner: [u8; 32],

    /// Fee credits
    pub fee_credits: I128,

    /// Per-account recurring-fee checkpoint (spec §2.1, §4.6.1 v12.18.4).
    /// Anchors the slot at which this account's wrapper-owned recurring
    /// maintenance fee was last realized. On materialization, set to the
    /// materialization slot; on free_slot, reset to 0. Invariant:
    ///   market Live     → last_fee_slot_i <= current_slot
    ///   market Resolved → last_fee_slot_i <= resolved_slot
    pub last_fee_slot: u64,

    // ---- Two-bucket warmup reserve (spec §4.3) ----
    /// Scheduled reserve bucket (older, matures linearly)
    pub sched_present: u8,
    pub sched_remaining_q: u128,
    pub sched_anchor_q: u128,
    pub sched_start_slot: u64,
    pub sched_horizon: u64,
    pub sched_release_q: u128,
    /// Pending reserve bucket (newest, does not mature while pending)
    pub pending_present: u8,
    pub pending_remaining_q: u128,
    pub pending_horizon: u64,
    pub pending_created_slot: u64,
}

impl Account {
    pub const KIND_USER: u8 = 0;
    pub const KIND_LP: u8 = 1;

    pub fn is_lp(&self) -> bool {
        self.kind == Self::KIND_LP
    }

    pub fn is_user(&self) -> bool {
        self.kind == Self::KIND_USER
    }
}

fn empty_account() -> Account {
    Account {
        capital: U128::ZERO,
        kind: Account::KIND_USER,
        pnl: 0i128,
        reserved_pnl: 0u128,
        position_basis_q: 0i128,
        adl_a_basis: ADL_ONE,
        adl_k_snap: 0i128,
        f_snap: 0i128,
        adl_epoch_snap: 0,
        matcher_program: [0; 32],
        matcher_context: [0; 32],
        owner: [0; 32],
        fee_credits: I128::ZERO,
        last_fee_slot: 0,
        sched_present: 0,
        sched_remaining_q: 0,
        sched_anchor_q: 0,
        sched_start_slot: 0,
        sched_horizon: 0,
        sched_release_q: 0,
        pending_present: 0,
        pending_remaining_q: 0,
        pending_horizon: 0,
        pending_created_slot: 0,
    }
}

/// Insurance fund state
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InsuranceFund {
    pub balance: U128,
}

/// Risk engine parameters
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskParams {
    pub maintenance_margin_bps: u64,
    pub initial_margin_bps: u64,
    pub trading_fee_bps: u64,
    pub max_accounts: u64,
    pub max_crank_staleness_slots: u64,
    pub liquidation_fee_bps: u64,
    pub liquidation_fee_cap: U128,
    pub min_liquidation_abs: U128,
    pub min_initial_deposit: U128,
    /// Absolute nonzero-position margin floors (spec §9.1)
    pub min_nonzero_mm_req: u128,
    pub min_nonzero_im_req: u128,
    /// Insurance fund floor (spec §1.4: 0 <= I_floor <= MAX_VAULT_TVL)
    pub insurance_floor: U128,
    /// Warmup horizon bounds (spec §6.1)
    pub h_min: u64,
    pub h_max: u64,
    /// Resolved settlement price deviation bound (spec §10.7)
    pub resolve_price_deviation_bps: u64,
    /// Max dt allowed in a single accrue_market_to call (spec §5.5 clause 6).
    /// Init-time invariant: ADL_ONE * MAX_ORACLE_PRICE *
    /// max_abs_funding_e9_per_slot * max_accrual_dt_slots <= i128::MAX
    /// ensures F_side_num cannot overflow in a single envelope-respecting call.
    pub max_accrual_dt_slots: u64,
    /// Max |funding_rate_e9_per_slot| allowed (spec §1.4).
    pub max_abs_funding_e9_per_slot: u64,
    /// Per-market active-positions cap per side (spec §1.4).
    /// Invariant: max_active_positions_per_side <= max_accounts <= MAX_ACCOUNTS.
    pub max_active_positions_per_side: u64,
}

/// Main risk engine state (spec §2.2)
#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskEngine {
    pub vault: U128,
    pub insurance_fund: InsuranceFund,
    pub params: RiskParams,
    pub current_slot: u64,

    /// Market mode (spec §2.2)
    pub market_mode: MarketMode,
    /// Resolved market state
    pub resolved_price: u64,
    pub resolved_slot: u64,
    /// Resolved terminal payout snapshot — locked after all positions zeroed.
    /// h_num/h_den frozen once, used for all terminal closes (order-invariant).
    pub resolved_payout_h_num: u128,
    pub resolved_payout_h_den: u128,
    pub resolved_payout_ready: u8, // 0 = not ready, 1 = snapshot locked
    /// Resolved terminal K deltas (spec §9.7 step 8).
    /// Stored separately from live K_side to avoid K headroom exhaustion during resolution.
    pub resolved_k_long_terminal_delta: i128,
    pub resolved_k_short_terminal_delta: i128,
    /// Live oracle price used for the live-sync leg of resolve_market
    pub resolved_live_price: u64,

    // Keeper crank tracking
    pub last_crank_slot: u64,

    // O(1) aggregates (spec §2.2)
    pub c_tot: U128,
    pub pnl_pos_tot: u128,
    pub pnl_matured_pos_tot: u128,

    // Crank cursors
    pub gc_cursor: u16,

    // ADL side state (spec §2.2)
    pub adl_mult_long: u128,
    pub adl_mult_short: u128,
    pub adl_coeff_long: i128,
    pub adl_coeff_short: i128,
    pub adl_epoch_long: u64,
    pub adl_epoch_short: u64,
    pub adl_epoch_start_k_long: i128,
    pub adl_epoch_start_k_short: i128,
    pub oi_eff_long_q: u128,
    pub oi_eff_short_q: u128,
    pub side_mode_long: SideMode,
    pub side_mode_short: SideMode,
    pub stored_pos_count_long: u64,
    pub stored_pos_count_short: u64,
    pub stale_account_count_long: u64,
    pub stale_account_count_short: u64,

    /// Dynamic phantom dust bounds (spec §4.6, §5.7)
    pub phantom_dust_bound_long_q: u128,
    pub phantom_dust_bound_short_q: u128,

    /// Materialized account count (spec §2.2)
    pub materialized_account_count: u64,

    /// Count of accounts with PNL < 0 (spec §4.7, v12.16.4)
    pub neg_pnl_account_count: u64,

    /// Last oracle price used in accrue_market_to (P_last, spec §5.5)
    pub last_oracle_price: u64,
    /// Last funding-sample price (fund_px_last, spec §5.5 step 11)
    pub fund_px_last: u64,
    /// Last slot used in accrue_market_to
    pub last_market_slot: u64,
    /// Cumulative funding numerator for long side (v12.15)
    pub f_long_num: i128,
    /// Cumulative funding numerator for short side (v12.15)
    pub f_short_num: i128,
    /// F snapshot at epoch start for long side (v12.15)
    pub f_epoch_start_long_num: i128,
    /// F snapshot at epoch start for short side (v12.15)
    pub f_epoch_start_short_num: i128,


    // Insurance floor is read from self.params.insurance_floor (no duplicate field)

    // Slab management
    pub used: [u64; BITMAP_WORDS],
    pub num_used_accounts: u16,
    pub free_head: u16,
    /// Forward pointer in the doubly-linked free list. Only meaningful when
    /// the slot is free. u16::MAX terminates the list.
    pub next_free: [u16; MAX_ACCOUNTS],
    /// Backward pointer — mirror of next_free. Enables O(1) removal at any
    /// position (used by materialize_at, which unlinks an arbitrary free
    /// slot rather than the head). Previously materialize_at did a linear
    /// scan over the full list; doubly-linked fix makes missing-account
    /// deposit O(1) worst-case.
    pub prev_free: [u16; MAX_ACCOUNTS],
    pub accounts: [Account; MAX_ACCOUNTS],
}

// ============================================================================
// Error Types
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiskError {
    InsufficientBalance,
    Undercollateralized,
    Unauthorized,
    PnlNotWarmedUp,
    Overflow,
    AccountNotFound,
    SideBlocked,
    CorruptState,
}

pub type Result<T> = core::result::Result<T, RiskError>;

/// Result of force_close_resolved_not_atomic (spec §10.8).
/// Eliminates the Ok(0) ambiguity between "deferred" and "closed with zero payout."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedCloseResult {
    /// Phase 1 reconciled but terminal payout not yet ready.
    /// Account is still open. Re-call after all accounts reconciled.
    ProgressOnly,
    /// Account closed and freed. Payout is the returned capital.
    Closed(u128),
}

impl ResolvedCloseResult {
    /// Extract capital if Closed, panic if Deferred.
    pub fn expect_closed(self, msg: &str) -> u128 {
        match self {
            Self::Closed(cap) => cap,
            Self::ProgressOnly => panic!("{}", msg),
        }
    }

    /// True if the account was deferred (still open).
    pub fn is_progress_only(self) -> bool {
        matches!(self, Self::ProgressOnly)
    }
}

/// Liquidation policy (spec §10.6)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidationPolicy {
    FullClose,
    ExactPartial(u128), // q_close_q
}

/// Outcome of a keeper crank operation
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrankOutcome {
    pub advanced: bool,
    pub num_liquidations: u32,
    pub num_gc_closed: u32,
}

// ============================================================================
// Small Helpers
// ============================================================================

#[inline]
fn add_u128(a: u128, b: u128) -> u128 {
    a.checked_add(b).expect("add_u128 overflow")
}

#[inline]
fn sub_u128(a: u128, b: u128) -> u128 {
    a.checked_sub(b).expect("sub_u128 underflow")
}

/// Determine which side a signed position is on. Positive = long, negative = short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Long,
    Short,
}

fn side_of_i128(v: i128) -> Option<Side> {
    if v == 0 {
        None
    } else if v > 0 {
        Some(Side::Long)
    } else {
        Some(Side::Short)
    }
}

fn opposite_side(s: Side) -> Side {
    match s {
        Side::Long => Side::Short,
        Side::Short => Side::Long,
    }
}

/// Clamp i128 max(v, 0) as u128
fn i128_clamp_pos(v: i128) -> u128 {
    if v > 0 {
        v as u128
    } else {
        0u128
    }
}

// ============================================================================
// Core Implementation
// ============================================================================

impl RiskEngine {
    /// Validate configuration parameters (spec §1.4, §2.2.1).
    /// Panics on invalid configuration to prevent deployment with unsafe params.
    fn validate_params(params: &RiskParams) {
        // Capacity: max_accounts within compile-time slab (spec §1.4)
        assert!(
            (params.max_accounts as usize) <= MAX_ACCOUNTS && params.max_accounts > 0,
            "max_accounts must be in 1..=MAX_ACCOUNTS"
        );

        // Per-market active-positions cap (spec §1.4):
        // 0 < max_active_positions_per_side <= max_accounts.
        assert!(
            params.max_active_positions_per_side > 0,
            "max_active_positions_per_side must be > 0 (spec §1.4)"
        );
        assert!(
            params.max_active_positions_per_side <= params.max_accounts,
            "max_active_positions_per_side must be <= max_accounts (spec §1.4)"
        );

        // Margin ordering: 0 <= maintenance_bps <= initial_bps <= 10_000 (spec §1.4)
        assert!(
            params.maintenance_margin_bps <= params.initial_margin_bps,
            "maintenance_margin_bps must be <= initial_margin_bps (spec §1.4)"
        );
        assert!(
            params.initial_margin_bps <= 10_000,
            "initial_margin_bps must be <= 10_000"
        );

        // BPS bounds (spec §1.4)
        assert!(
            params.trading_fee_bps <= 10_000,
            "trading_fee_bps must be <= 10_000"
        );
        assert!(
            params.liquidation_fee_bps <= 10_000,
            "liquidation_fee_bps must be <= 10_000"
        );

        // Nonzero margin floor ordering: 0 < mm < im <= min_initial_deposit (spec §1.4)
        assert!(
            params.min_nonzero_mm_req > 0,
            "min_nonzero_mm_req must be > 0"
        );
        assert!(
            params.min_nonzero_mm_req < params.min_nonzero_im_req,
            "min_nonzero_mm_req must be strictly less than min_nonzero_im_req"
        );
        assert!(
            params.min_nonzero_im_req <= params.min_initial_deposit.get(),
            "min_nonzero_im_req must be <= min_initial_deposit (spec §1.4)"
        );

        // MIN_INITIAL_DEPOSIT bounds: 0 < min_initial_deposit <= MAX_VAULT_TVL (spec §1.4)
        assert!(
            params.min_initial_deposit.get() > 0,
            "min_initial_deposit must be > 0 (spec §1.4)"
        );
        assert!(
            params.min_initial_deposit.get() <= MAX_VAULT_TVL,
            "min_initial_deposit must be <= MAX_VAULT_TVL"
        );

        // Liquidation fee ordering: 0 <= min_liquidation_abs <= liquidation_fee_cap (spec §1.4)
        assert!(
            params.min_liquidation_abs.get() <= params.liquidation_fee_cap.get(),
            "min_liquidation_abs must be <= liquidation_fee_cap (spec §1.4)"
        );
        assert!(
            params.liquidation_fee_cap.get() <= MAX_PROTOCOL_FEE_ABS,
            "liquidation_fee_cap must be <= MAX_PROTOCOL_FEE_ABS (spec §1.4)"
        );

        // Insurance floor (spec §1.4: 0 <= I_floor <= MAX_VAULT_TVL)
        assert!(
            params.insurance_floor.get() <= MAX_VAULT_TVL,
            "insurance_floor must be <= MAX_VAULT_TVL (spec §1.4)"
        );

        // Warmup horizon bounds (spec §6.1)
        assert!(
            params.h_min <= params.h_max,
            "h_min must be <= h_max (spec §6.1)"
        );
        // A market with cfg_h_max == 0 is dead on arrival: every live
        // instruction that creates fresh reserve requires admit_h_max > 0
        // AND admit_h_max <= cfg_h_max. The intersection is empty when
        // cfg_h_max == 0, so every live op would later fail at the
        // admission_pair gate. Reject the misconfiguration here for a
        // clear init-time error rather than a cryptic runtime brick.
        assert!(
            params.h_max > 0,
            "h_max must be > 0 (live admission_pair requires h_max > 0 per spec §1.4)"
        );

        // Resolve price deviation (spec §10.7)
        assert!(
            params.resolve_price_deviation_bps <= MAX_RESOLVE_PRICE_DEVIATION_BPS,
            "resolve_price_deviation_bps must be <= MAX_RESOLVE_PRICE_DEVIATION_BPS"
        );

        // Funding/accrual envelope (spec §1.4):
        // ADL_ONE * MAX_ORACLE_PRICE * max_abs_funding_e9_per_slot *
        //   max_accrual_dt_slots <= i128::MAX
        // This ensures F_side_num cannot overflow in a single envelope-respecting call.
        assert!(
            params.max_accrual_dt_slots > 0,
            "max_accrual_dt_slots must be > 0 (spec §1.4)"
        );
        assert!(
            (params.max_abs_funding_e9_per_slot as i128) <= MAX_ABS_FUNDING_E9_PER_SLOT,
            "max_abs_funding_e9_per_slot must be <= MAX_ABS_FUNDING_E9_PER_SLOT"
        );
        // Check envelope: product must fit in i128. Use U256 to compute exactly.
        let envelope_ok = {
            let adl = U256::from_u128(ADL_ONE);
            let px = U256::from_u128(MAX_ORACLE_PRICE as u128);
            let rate = U256::from_u128(params.max_abs_funding_e9_per_slot as u128);
            let dt = U256::from_u128(params.max_accrual_dt_slots as u128);
            let p1 = adl.checked_mul(px);
            let p2 = p1.and_then(|v| v.checked_mul(rate));
            let p3 = p2.and_then(|v| v.checked_mul(dt));
            let i128_max = U256::from_u128(i128::MAX as u128);
            match p3 {
                Some(v) => v <= i128_max,
                None => false,
            }
        };
        assert!(envelope_ok,
            "funding envelope: ADL_ONE * MAX_ORACLE_PRICE * max_abs_funding_e9_per_slot * max_accrual_dt_slots must fit i128 (spec §1.4)"
        );
    }

    /// Create a new risk engine for testing. Initializes with
    /// init_oracle_price = 1 (spec §2.7 compliant).
    #[cfg(any(feature = "test", kani))]
    pub fn new(params: RiskParams) -> Self {
        Self::new_with_market(params, 0, 1)
    }

    /// Create a new risk engine with explicit market initialization (spec §2.7).
    /// Requires `0 < init_oracle_price <= MAX_ORACLE_PRICE` per spec §1.2.
    ///
    /// Test/kani only. Returns Self by value, which on SBF would require
    /// materializing ~MAX_ACCOUNTS * sizeof(Account) bytes on the stack
    /// (>>4KB limit). Production callers MUST use `init_in_place` on
    /// pre-allocated zero-initialized memory (SystemProgram.createAccount).
    #[cfg(any(feature = "test", kani))]
    pub fn new_with_market(params: RiskParams, init_slot: u64, init_oracle_price: u64) -> Self {
        Self::validate_params(&params);
        assert!(
            init_oracle_price > 0 && init_oracle_price <= MAX_ORACLE_PRICE,
            "init_oracle_price must be in (0, MAX_ORACLE_PRICE] per spec §2.7"
        );
        let mut engine = Self {
            vault: U128::ZERO,
            insurance_fund: InsuranceFund {
                balance: U128::ZERO,
            },
            params,
            current_slot: init_slot,
            market_mode: MarketMode::Live,
            resolved_price: 0,
            resolved_slot: 0,
            resolved_payout_h_num: 0,
            resolved_payout_h_den: 0,
            resolved_payout_ready: 0,
            resolved_k_long_terminal_delta: 0,
            resolved_k_short_terminal_delta: 0,
            resolved_live_price: 0,
            last_crank_slot: 0,
            c_tot: U128::ZERO,
            pnl_pos_tot: 0u128,
            pnl_matured_pos_tot: 0u128,
            gc_cursor: 0,
            adl_mult_long: ADL_ONE,
            adl_mult_short: ADL_ONE,
            adl_coeff_long: 0i128,
            adl_coeff_short: 0i128,
            adl_epoch_long: 0,
            adl_epoch_short: 0,
            adl_epoch_start_k_long: 0i128,
            adl_epoch_start_k_short: 0i128,
            oi_eff_long_q: 0u128,
            oi_eff_short_q: 0u128,
            side_mode_long: SideMode::Normal,
            side_mode_short: SideMode::Normal,
            stored_pos_count_long: 0,
            stored_pos_count_short: 0,
            stale_account_count_long: 0,
            stale_account_count_short: 0,
            phantom_dust_bound_long_q: 0u128,
            phantom_dust_bound_short_q: 0u128,
            materialized_account_count: 0,
            neg_pnl_account_count: 0,
            last_oracle_price: init_oracle_price,
            fund_px_last: init_oracle_price,
            last_market_slot: init_slot,
            f_long_num: 0,
            f_short_num: 0,
            f_epoch_start_long_num: 0,
            f_epoch_start_short_num: 0,
            used: [0; BITMAP_WORDS],
            num_used_accounts: 0,
            free_head: 0,
            next_free: [0; MAX_ACCOUNTS],
            prev_free: [0; MAX_ACCOUNTS],
            accounts: [empty_account(); MAX_ACCOUNTS],
        };

        // Build the doubly-linked free list 0 → 1 → ... → N-1 → NIL.
        engine.prev_free[0] = u16::MAX; // head has no prev
        for i in 0..MAX_ACCOUNTS - 1 {
            engine.next_free[i] = (i + 1) as u16;
            engine.prev_free[i + 1] = i as u16;
        }
        engine.next_free[MAX_ACCOUNTS - 1] = u16::MAX;

        engine
    }

    /// Initialize in place (for Solana BPF zero-copy, spec §2.7).
    /// Fully canonicalizes all state — safe even on non-zeroed memory.
    pub fn init_in_place(&mut self, params: RiskParams, init_slot: u64, init_oracle_price: u64) {
        Self::validate_params(&params);
        assert!(
            init_oracle_price > 0 && init_oracle_price <= MAX_ORACLE_PRICE,
            "init_oracle_price must be in (0, MAX_ORACLE_PRICE] per spec §2.7"
        );
        self.vault = U128::ZERO;
        self.insurance_fund = InsuranceFund { balance: U128::ZERO };
        self.params = params;
        self.current_slot = init_slot;
        self.market_mode = MarketMode::Live;
        self.resolved_price = 0;
        self.resolved_slot = 0;
        self.resolved_payout_h_num = 0;
        self.resolved_payout_h_den = 0;
        self.resolved_payout_ready = 0;
        self.resolved_k_long_terminal_delta = 0;
        self.resolved_k_short_terminal_delta = 0;
        self.resolved_live_price = 0;
        self.last_crank_slot = 0;
        self.c_tot = U128::ZERO;
        self.pnl_pos_tot = 0;
        self.pnl_matured_pos_tot = 0;
        self.gc_cursor = 0;
        self.adl_mult_long = ADL_ONE;
        self.adl_mult_short = ADL_ONE;
        self.adl_coeff_long = 0;
        self.adl_coeff_short = 0;
        self.adl_epoch_long = 0;
        self.adl_epoch_short = 0;
        self.adl_epoch_start_k_long = 0;
        self.adl_epoch_start_k_short = 0;
        self.oi_eff_long_q = 0;
        self.oi_eff_short_q = 0;
        self.side_mode_long = SideMode::Normal;
        self.side_mode_short = SideMode::Normal;
        self.stored_pos_count_long = 0;
        self.stored_pos_count_short = 0;
        self.stale_account_count_long = 0;
        self.stale_account_count_short = 0;
        self.phantom_dust_bound_long_q = 0;
        self.phantom_dust_bound_short_q = 0;
        self.materialized_account_count = 0;
        self.neg_pnl_account_count = 0;
        self.last_oracle_price = init_oracle_price;
        self.fund_px_last = init_oracle_price;
        self.last_market_slot = init_slot;
        self.f_long_num = 0;
        self.f_short_num = 0;
        self.f_epoch_start_long_num = 0;
        self.f_epoch_start_short_num = 0;
        // insurance_floor is now read directly from self.params.insurance_floor
        self.used = [0; BITMAP_WORDS];
        self.num_used_accounts = 0;
        self.free_head = 0;
        // Fully canonicalize every account in-place (SBF-safe: per-field
        // assignment, never constructs a temporary Account on the stack).
        // Previously only adl_a_basis was reset, relying on
        // SystemProgram.createAccount zero-init. That's correct in normal
        // Solana flow but the method's doc promises canonicalization of
        // "non-zeroed memory", so we must reset every field explicitly.
        for i in 0..MAX_ACCOUNTS {
            let a = &mut self.accounts[i];
            a.kind = Account::KIND_USER;
            a.capital = U128::ZERO;
            a.pnl = 0;
            a.reserved_pnl = 0;
            a.position_basis_q = 0;
            a.adl_a_basis = ADL_ONE;
            a.adl_k_snap = 0;
            a.f_snap = 0;
            a.adl_epoch_snap = 0;
            a.matcher_program = [0; 32];
            a.matcher_context = [0; 32];
            a.owner = [0; 32];
            a.fee_credits = I128::ZERO;
            a.last_fee_slot = 0;
            a.sched_present = 0;
            a.sched_remaining_q = 0;
            a.sched_anchor_q = 0;
            a.sched_start_slot = 0;
            a.sched_horizon = 0;
            a.sched_release_q = 0;
            a.pending_present = 0;
            a.pending_remaining_q = 0;
            a.pending_horizon = 0;
            a.pending_created_slot = 0;
        }
        self.prev_free[0] = u16::MAX;
        for i in 0..MAX_ACCOUNTS - 1 {
            self.next_free[i] = (i + 1) as u16;
            self.prev_free[i + 1] = i as u16;
        }
        self.next_free[MAX_ACCOUNTS - 1] = u16::MAX;
    }

    // ========================================================================
    // Bitmap Helpers
    // ========================================================================

    pub fn is_used(&self, idx: usize) -> bool {
        if idx >= MAX_ACCOUNTS {
            return false;
        }
        let w = idx >> 6;
        let b = idx & 63;
        ((self.used[w] >> b) & 1) == 1
    }

    fn set_used(&mut self, idx: usize) {
        let w = idx >> 6;
        let b = idx & 63;
        self.used[w] |= 1u64 << b;
    }

    fn clear_used(&mut self, idx: usize) {
        let w = idx >> 6;
        let b = idx & 63;
        self.used[w] &= !(1u64 << b);
    }

    fn for_each_used<F: FnMut(usize, &Account)>(&self, mut f: F) {
        for (block, word) in self.used.iter().copied().enumerate() {
            let mut w = word;
            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                let idx = block * 64 + bit;
                w &= w - 1;
                if idx >= MAX_ACCOUNTS {
                    continue;
                }
                f(idx, &self.accounts[idx]);
            }
        }
    }

    // ========================================================================
    // Freelist
    // ========================================================================

    test_visible! {
    fn free_slot(&mut self, idx: u16) -> Result<()> {
        let i = idx as usize;
        if i >= MAX_ACCOUNTS { return Err(RiskError::AccountNotFound); }
        // Reject double-free: slot MUST be marked used. Without this guard
        // a second free_slot on the same idx corrupted the freelist by
        // creating a self-cycle at free_head and decremented counters past
        // zero. Hardens the allocator against internal bugs.
        if !self.is_used(i) { return Err(RiskError::CorruptState); }
        if self.accounts[i].pnl != 0 { return Err(RiskError::CorruptState); }
        if self.accounts[i].reserved_pnl != 0 { return Err(RiskError::CorruptState); }
        if self.accounts[i].position_basis_q != 0 { return Err(RiskError::CorruptState); }
        if self.accounts[i].sched_present != 0 || self.accounts[i].pending_present != 0 {
            return Err(RiskError::CorruptState);
        }
        // Defense-in-depth: capital and fee_credits must be zero. Previously
        // only pnl/reserved/basis/bucket-flags were checked; an upstream bug
        // could leave capital > 0 or fee_credits != 0 and free_slot would
        // silently zero them — leaking C_tot accounting vs account table
        // (capital disappears from account but c_tot stays elevated).
        if !self.accounts[i].capital.is_zero() {
            return Err(RiskError::CorruptState);
        }
        if self.accounts[i].fee_credits.get() != 0 {
            return Err(RiskError::CorruptState);
        }
        let a = &mut self.accounts[i];
        a.capital = U128::ZERO;
        a.kind = Account::KIND_USER;
        a.pnl = 0;
        a.reserved_pnl = 0;
        a.position_basis_q = 0;
        a.adl_a_basis = ADL_ONE;
        a.adl_k_snap = 0;
        a.f_snap = 0;
        a.adl_epoch_snap = 0;
        a.matcher_program = [0; 32];
        a.matcher_context = [0; 32];
        a.owner = [0; 32];
        a.fee_credits = I128::ZERO;
        a.last_fee_slot = 0;
        a.sched_present = 0;
        a.sched_remaining_q = 0;
        a.sched_anchor_q = 0;
        a.sched_start_slot = 0;
        a.sched_horizon = 0;
        a.sched_release_q = 0;
        a.pending_present = 0;
        a.pending_remaining_q = 0;
        a.pending_horizon = 0;
        a.pending_created_slot = 0;
        self.clear_used(i);
        // Push to head of doubly-linked free list.
        self.next_free[i] = self.free_head;
        self.prev_free[i] = u16::MAX;
        if self.free_head != u16::MAX {
            self.prev_free[self.free_head as usize] = idx;
        }
        self.free_head = idx;
        self.num_used_accounts = self.num_used_accounts.checked_sub(1)
            .ok_or(RiskError::CorruptState)?;
        self.materialized_account_count = self.materialized_account_count.checked_sub(1)
            .ok_or(RiskError::CorruptState)?;
        Ok(())
    }
    }

    /// materialize_account(i, slot_anchor) — spec §2.5.
    /// Materializes a missing account at a specific slot index.
    /// The slot must not be currently in use.
    test_visible! {
    fn materialize_at(&mut self, idx: u16, slot_anchor: u64) -> Result<()> {
        if idx as usize >= MAX_ACCOUNTS {
            return Err(RiskError::AccountNotFound);
        }

        let used_count = self.num_used_accounts as u64;
        if used_count >= self.params.max_accounts {
            return Err(RiskError::Overflow);
        }

        // Enforce materialized_account_count bound (spec §10.0)
        self.materialized_account_count = self.materialized_account_count
            .checked_add(1).ok_or(RiskError::Overflow)?;
        if self.materialized_account_count > MAX_MATERIALIZED_ACCOUNTS {
            self.materialized_account_count -= 1;
            return Err(RiskError::Overflow);
        }

        // O(1) unlink from doubly-linked free list. If idx is not actually
        // free (no prev/next pointers in a consistent free-list state AND
        // bitmap says used), the pre-check above via !is_used in callers
        // should have already prevented this path. We require idx to be
        // marked unused (i.e., currently in the free list).
        if self.is_used(idx as usize) {
            self.materialized_account_count -= 1;
            return Err(RiskError::CorruptState);
        }
        let i = idx as usize;
        let next = self.next_free[i];
        let prev = self.prev_free[i];
        // Freelist-link consistency. Two layers of defense:
        //   (a) local back-pointer agreement — prev/next's reciprocal
        //       pointer must point to idx;
        //   (b) neighbor-used check — a truly-free neighbor is marked
        //       unused in the bitmap. If a corrupt neighbor pointer
        //       lands on an allocated slot, reject.
        if prev == u16::MAX {
            if self.free_head != idx {
                self.materialized_account_count -= 1;
                return Err(RiskError::CorruptState);
            }
        } else {
            if self.next_free[prev as usize] != idx {
                self.materialized_account_count -= 1;
                return Err(RiskError::CorruptState);
            }
            if self.is_used(prev as usize) {
                self.materialized_account_count -= 1;
                return Err(RiskError::CorruptState);
            }
        }
        if next != u16::MAX {
            if self.prev_free[next as usize] != idx {
                self.materialized_account_count -= 1;
                return Err(RiskError::CorruptState);
            }
            if self.is_used(next as usize) {
                self.materialized_account_count -= 1;
                return Err(RiskError::CorruptState);
            }
        }
        // Links verified — perform the unlink.
        if prev == u16::MAX {
            self.free_head = next;
        } else {
            self.next_free[prev as usize] = next;
        }
        if next != u16::MAX {
            self.prev_free[next as usize] = prev;
        }
        // Clear idx's freelist pointers now that it's allocated. Prevents
        // stale values from later masquerading as valid free-list state
        // if this slot is corrupted while in use.
        self.next_free[i] = u16::MAX;
        self.prev_free[i] = u16::MAX;

        self.set_used(idx as usize);
        self.num_used_accounts = self.num_used_accounts.checked_add(1)
            .expect("num_used_accounts overflow — slot leak corruption");

        // Initialize per spec §2.5 — field-by-field to avoid constructing
        // a ~4KB temporary Account on the stack (SBF stack limit is 4KB).
        {
            let a = &mut self.accounts[idx as usize];
            a.kind = Account::KIND_USER;
            a.capital = U128::ZERO;
            a.pnl = 0i128;
            a.reserved_pnl = 0u128;
            a.position_basis_q = 0i128;
            a.adl_a_basis = ADL_ONE;
            a.adl_k_snap = 0i128;
            a.f_snap = 0i128;
            a.adl_epoch_snap = 0;
            a.matcher_program = [0; 32];
            a.matcher_context = [0; 32];
            a.owner = [0; 32];
            a.fee_credits = I128::ZERO;
            // Spec §2.7 v12.18.4: anchor recurring-fee checkpoint at the
            // caller-supplied materialization slot. Prevents newly created
            // accounts from being back-charged for time before materialization
            // (Goal 47).
            a.last_fee_slot = slot_anchor;
            a.sched_present = 0;
            a.sched_remaining_q = 0;
            a.sched_anchor_q = 0;
            a.sched_start_slot = 0;
            a.sched_horizon = 0;
            a.sched_release_q = 0;
            a.pending_present = 0;
            a.pending_remaining_q = 0;
            a.pending_horizon = 0;
            a.pending_created_slot = 0;
        }

        Ok(())
    }
    }

    // ========================================================================
    // O(1) Aggregate Helpers (spec §4)
    // ========================================================================


    /// admit_fresh_reserve_h_lock (spec §4.7): decide effective horizon for fresh reserve.
    /// Returns admit_h_min if instant release preserves h=1, admit_h_max otherwise.
    /// Sticky: once an account gets h_max in this instruction, all later increments also get h_max.
    ///
    /// Internal helper. Not part of the public engine surface — callers should
    /// go through set_pnl_with_reserve with ReserveMode::UseAdmissionPair.
    test_visible! {
    fn admit_fresh_reserve_h_lock(
        &self, idx: usize, fresh_positive_pnl: u128,
        ctx: &mut InstructionContext, admit_h_min: u64, admit_h_max: u64,
    ) -> Result<u64> {
        // Step 1: sticky check
        if ctx.is_h_max_sticky(idx as u16) { return Ok(admit_h_max); }

        // Step 2: headroom check. Use checked arithmetic; saturating would
        // mask overflows or a broken V >= C_tot + I invariant, producing a
        // wrong residual and a wrong admission decision.
        let senior = self.c_tot.get()
            .checked_add(self.insurance_fund.balance.get())
            .ok_or(RiskError::Overflow)?;
        // Residual requires V >= senior (engine invariant). Anything less is
        // corruption; fail rather than return 0.
        let residual = self.vault.get()
            .checked_sub(senior)
            .ok_or(RiskError::CorruptState)?;
        let matured_plus_fresh = self.pnl_matured_pos_tot
            .checked_add(fresh_positive_pnl)
            .ok_or(RiskError::Overflow)?;

        let admitted_h_eff = if matured_plus_fresh <= residual {
            admit_h_min
        } else {
            admit_h_max
        };

        // Step 3: mark sticky if h_max. mark_h_max_sticky returns false on
        // capacity exhaustion; propagate as failure rather than silently
        // skipping the sticky — later calls would otherwise not see this
        // account as sticky and could re-admit at h_min.
        if admitted_h_eff == admit_h_max {
            if !ctx.mark_h_max_sticky(idx as u16) {
                return Err(RiskError::Overflow);
            }
        }
        Ok(admitted_h_eff)
    }
    }

    /// admit_outstanding_reserve_on_touch (spec §4.9): accelerate existing reserve if h=1 holds.
    ///
    /// Internal helper. Not part of the public engine surface — called by
    /// touch_account_live_local as part of the live-touch pipeline.
    test_visible! {
    fn admit_outstanding_reserve_on_touch(&mut self, idx: usize) -> Result<()> {
        if self.market_mode != MarketMode::Live { return Ok(()); }

        // Validate reserve integrity BEFORE any arithmetic or mutation.
        // Previously, malformed state (e.g., sched_remaining mismatching
        // reserved_pnl, or reserved_pnl > max(pnl, 0)) could be accelerated
        // through — turning "corrupt reserve" into "clean matured PnL" and
        // laundering the corruption into aggregates.
        self.validate_reserve_shape(idx)?;

        // Phase 1: compute everything with checked arithmetic. No mutation yet.
        // Previously used saturating_add/saturating_sub which could mask
        // overflow or a broken V >= C_tot + I invariant. Also, the
        // matured > pnl_pos_tot check ran AFTER state mutations, violating
        // the validate-then-mutate contract for no-_not_atomic public helpers.
        let a = &self.accounts[idx];
        let sched_r = if a.sched_present != 0 { a.sched_remaining_q } else { 0 };
        let pend_r = if a.pending_present != 0 { a.pending_remaining_q } else { 0 };
        let reserve_total = sched_r.checked_add(pend_r).ok_or(RiskError::CorruptState)?;
        if reserve_total == 0 { return Ok(()); }

        let senior = self.c_tot.get()
            .checked_add(self.insurance_fund.balance.get())
            .ok_or(RiskError::Overflow)?;
        let residual = self.vault.get()
            .checked_sub(senior)
            .ok_or(RiskError::CorruptState)?;
        let new_matured = self.pnl_matured_pos_tot
            .checked_add(reserve_total)
            .ok_or(RiskError::Overflow)?;

        if new_matured > residual {
            // Does not admit — no mutation.
            return Ok(());
        }

        // Pre-validate the global invariant BEFORE any mutation.
        if new_matured > self.pnl_pos_tot {
            return Err(RiskError::CorruptState);
        }

        // Phase 2: all checks passed — commit.
        self.pnl_matured_pos_tot = new_matured;
        let a = &mut self.accounts[idx];
        a.sched_present = 0;
        a.sched_remaining_q = 0;
        a.sched_anchor_q = 0;
        a.sched_start_slot = 0;
        a.sched_horizon = 0;
        a.sched_release_q = 0;
        a.pending_present = 0;
        a.pending_remaining_q = 0;
        a.pending_horizon = 0;
        a.pending_created_slot = 0;
        a.reserved_pnl = 0;
        Ok(())
    }
    }

    /// set_pnl: thin wrapper routing through set_pnl_with_reserve(ImmediateRelease).
    /// All PnL mutations go through one canonical path. ImmediateRelease routes
    /// positive increases directly to matured (no reserve queue), and decreases
    /// go through apply_reserve_loss_newest_first — replacing the old saturating_sub.
    test_visible! {
    fn set_pnl(&mut self, idx: usize, new_pnl: i128) -> Result<()> {
        self.set_pnl_with_reserve(idx, new_pnl, ReserveMode::ImmediateReleaseResolvedOnly, None)
    }
    }

    /// set_pnl with reserve_mode (spec §4.5, v12.14.0).
    /// Canonical PNL mutation that routes positive increases through the cohort queue.
    test_visible! {
    fn set_pnl_with_reserve(&mut self, idx: usize, new_pnl: i128, reserve_mode: ReserveMode, ctx: Option<&mut InstructionContext>) -> Result<()> {
        if new_pnl == i128::MIN { return Err(RiskError::Overflow); }

        let old = self.accounts[idx].pnl;
        let old_pos = i128_clamp_pos(old);
        // Entry invariant: R_i <= max(PNL_i, 0) (spec §2.1). Reject before any mutation.
        if self.accounts[idx].reserved_pnl > old_pos {
            return Err(RiskError::CorruptState);
        }
        let old_rel = if self.market_mode == MarketMode::Live {
            old_pos.checked_sub(self.accounts[idx].reserved_pnl).ok_or(RiskError::CorruptState)?
        } else {
            if self.accounts[idx].reserved_pnl != 0 { return Err(RiskError::CorruptState); }
            old_pos
        };
        let new_pos = i128_clamp_pos(new_pnl);

        // Pre-validate reserve mode BEFORE any mutation
        if new_pos > old_pos {
            match reserve_mode {
                ReserveMode::NoPositiveIncreaseAllowed => {
                    return Err(RiskError::Overflow);
                }
                ReserveMode::ImmediateReleaseResolvedOnly => {
                    if self.market_mode == MarketMode::Live {
                        return Err(RiskError::Unauthorized);
                    }
                }
                ReserveMode::UseAdmissionPair(_, _) => {
                    if self.market_mode != MarketMode::Live {
                        return Err(RiskError::Unauthorized);
                    }
                }
            }
        }

        if self.market_mode == MarketMode::Live && new_pos > MAX_ACCOUNT_POSITIVE_PNL {
            return Err(RiskError::Overflow);
        }

        // Pre-validate aggregate cap before mutation
        if new_pos > old_pos {
            let delta = new_pos - old_pos;
            let new_tot = self.pnl_pos_tot.checked_add(delta).ok_or(RiskError::Overflow)?;
            if self.market_mode == MarketMode::Live && new_tot > MAX_PNL_POS_TOT {
                return Err(RiskError::Overflow);
            }
        }

        if new_pos > old_pos {
            let delta = new_pos - old_pos;
            self.pnl_pos_tot = self.pnl_pos_tot.checked_add(delta).ok_or(RiskError::Overflow)?;
        } else if old_pos > new_pos {
            let delta = old_pos - new_pos;
            self.pnl_pos_tot = self.pnl_pos_tot.checked_sub(delta).ok_or(RiskError::Overflow)?;
        }

        if new_pos > old_pos {
            let reserve_add = new_pos - old_pos;
            if old < 0 && new_pnl >= 0 {
                self.neg_pnl_account_count = self.neg_pnl_account_count.checked_sub(1)
                    .ok_or(RiskError::CorruptState)?;
            } else if old >= 0 && new_pnl < 0 {
                self.neg_pnl_account_count = self.neg_pnl_account_count.checked_add(1)
                    .ok_or(RiskError::CorruptState)?;
            }
            self.accounts[idx].pnl = new_pnl;

            match reserve_mode {
                ReserveMode::NoPositiveIncreaseAllowed => {
                    return Err(RiskError::Overflow); // unreachable: pre-validated
                }
                ReserveMode::ImmediateReleaseResolvedOnly => {
                    // Only valid in Resolved mode (pre-validated above)
                    self.pnl_matured_pos_tot = self.pnl_matured_pos_tot.checked_add(reserve_add)
                        .ok_or(RiskError::Overflow)?;
                    // Spec §4.8 step 18 (v12.18.5): invariant pair.
                    if self.pnl_matured_pos_tot > self.pnl_pos_tot { return Err(RiskError::CorruptState); }
                    let pos_pnl_final: u128 = if new_pnl > 0 { new_pnl as u128 } else { 0 };
                    if self.accounts[idx].reserved_pnl > pos_pnl_final { return Err(RiskError::CorruptState); }
                    return Ok(());
                }
                ReserveMode::UseAdmissionPair(admit_h_min, admit_h_max) => {
                    // Admission-pair: engine decides effective horizon (spec §4.7)
                    let ctx = ctx.ok_or(RiskError::CorruptState)?;
                    let admitted_h_eff = self.admit_fresh_reserve_h_lock(
                        idx, reserve_add, ctx, admit_h_min, admit_h_max)?;
                    if admitted_h_eff == 0 {
                        self.pnl_matured_pos_tot = self.pnl_matured_pos_tot.checked_add(reserve_add)
                            .ok_or(RiskError::Overflow)?;
                    } else {
                        self.append_or_route_new_reserve(idx, reserve_add, self.current_slot, admitted_h_eff)?;
                    }
                    // Spec §4.8 step 18 (v12.18.5): invariant pair.
                    if self.pnl_matured_pos_tot > self.pnl_pos_tot { return Err(RiskError::CorruptState); }
                    let pos_pnl_final: u128 = if new_pnl > 0 { new_pnl as u128 } else { 0 };
                    if self.accounts[idx].reserved_pnl > pos_pnl_final { return Err(RiskError::CorruptState); }
                    return Ok(());
                }
            }
        } else {
            // Case B: no positive increase
            let pos_loss = old_pos - new_pos;
            if self.market_mode == MarketMode::Live {
                let reserve_loss = core::cmp::min(pos_loss, self.accounts[idx].reserved_pnl);
                if reserve_loss > 0 {
                    self.apply_reserve_loss_newest_first(idx, reserve_loss)?;
                }
                let matured_loss = pos_loss - reserve_loss;
                if matured_loss > 0 {
                    self.pnl_matured_pos_tot = self.pnl_matured_pos_tot.checked_sub(matured_loss)
                        .ok_or(RiskError::CorruptState)?;
                }
            } else {
                // Resolved: R_i must be 0
                if self.accounts[idx].reserved_pnl != 0 { return Err(RiskError::CorruptState); }
                if pos_loss > 0 {
                    self.pnl_matured_pos_tot = self.pnl_matured_pos_tot.checked_sub(pos_loss)
                        .ok_or(RiskError::CorruptState)?;
                }
            }
            // Track neg_pnl_account_count sign transitions (spec §4.7)
            if old < 0 && new_pnl >= 0 {
                self.neg_pnl_account_count = self.neg_pnl_account_count.checked_sub(1)
                    .ok_or(RiskError::CorruptState)?;
            } else if old >= 0 && new_pnl < 0 {
                self.neg_pnl_account_count = self.neg_pnl_account_count.checked_add(1)
                    .ok_or(RiskError::CorruptState)?;
            }
            self.accounts[idx].pnl = new_pnl;

            // Step 20: if new_pos == 0 and Live, require empty queue
            if new_pos == 0 && self.market_mode == MarketMode::Live {
                if self.accounts[idx].reserved_pnl != 0 { return Err(RiskError::CorruptState); }
                if self.accounts[idx].sched_present != 0 { return Err(RiskError::CorruptState); }
                if self.accounts[idx].pending_present != 0 { return Err(RiskError::CorruptState); }
            }

            // Spec §4.8 step 18 (v12.18.5): invariant pair.
            if self.pnl_matured_pos_tot > self.pnl_pos_tot { return Err(RiskError::CorruptState); }
            let pos_pnl_final: u128 = if new_pnl > 0 { new_pnl as u128 } else { 0 };
            if self.accounts[idx].reserved_pnl > pos_pnl_final { return Err(RiskError::CorruptState); }
            return Ok(());
        }
    }
    }

    /// consume_released_pnl (spec §4.4.1): remove only matured released positive PnL,
    /// leaving R_i unchanged.
    test_visible! {
    fn consume_released_pnl(&mut self, idx: usize, x: u128) -> Result<()> {
        if x == 0 { return Err(RiskError::CorruptState); }

        let old_pos = i128_clamp_pos(self.accounts[idx].pnl);
        let old_r = self.accounts[idx].reserved_pnl;
        let old_rel = old_pos.checked_sub(old_r).ok_or(RiskError::CorruptState)?;
        if x > old_rel { return Err(RiskError::CorruptState); }

        let new_pos = old_pos.checked_sub(x).ok_or(RiskError::CorruptState)?;
        let new_rel = old_rel.checked_sub(x).ok_or(RiskError::CorruptState)?;
        if new_pos < old_r { return Err(RiskError::CorruptState); }

        // Update pnl_pos_tot
        self.pnl_pos_tot = self.pnl_pos_tot.checked_sub(x)
            .ok_or(RiskError::CorruptState)?;

        // Update pnl_matured_pos_tot
        self.pnl_matured_pos_tot = self.pnl_matured_pos_tot.checked_sub(x)
            .ok_or(RiskError::CorruptState)?;
        if self.pnl_matured_pos_tot > self.pnl_pos_tot { return Err(RiskError::CorruptState); }

        // PNL_i = checked_sub_i128(PNL_i, checked_cast_i128(x))
        let x_i128: i128 = x.try_into().map_err(|_| RiskError::Overflow)?;
        let new_pnl = self.accounts[idx].pnl.checked_sub(x_i128)
            .ok_or(RiskError::Overflow)?;
        if new_pnl == i128::MIN { return Err(RiskError::Overflow); }
        self.accounts[idx].pnl = new_pnl;
        // R_i remains unchanged
        Ok(())
    }
    }

    /// set_capital (spec §4.2): checked signed-delta update of C_tot
    test_visible! {
    fn set_capital(&mut self, idx: usize, new_capital: u128) -> Result<()> {
        let old = self.accounts[idx].capital.get();
        if new_capital >= old {
            let delta = new_capital - old;
            self.c_tot = U128::new(self.c_tot.get().checked_add(delta)
                .ok_or(RiskError::Overflow)?);
        } else {
            let delta = old - new_capital;
            self.c_tot = U128::new(self.c_tot.get().checked_sub(delta)
                .ok_or(RiskError::CorruptState)?);
        }
        self.accounts[idx].capital = U128::new(new_capital);
        Ok(())
    }
    }

    /// set_position_basis_q (spec §4.4): update stored pos counts based on sign changes
    test_visible! {
    fn set_position_basis_q(&mut self, idx: usize, new_basis: i128) -> Result<()> {
        let old = self.accounts[idx].position_basis_q;
        let old_side = side_of_i128(old);
        let new_side = side_of_i128(new_basis);

        // Decrement old side count
        if let Some(s) = old_side {
            match s {
                Side::Long => {
                    self.stored_pos_count_long = self.stored_pos_count_long
                        .checked_sub(1).ok_or(RiskError::CorruptState)?;
                }
                Side::Short => {
                    self.stored_pos_count_short = self.stored_pos_count_short
                        .checked_sub(1).ok_or(RiskError::CorruptState)?;
                }
            }
        }

        // Increment new side count
        if let Some(s) = new_side {
            match s {
                Side::Long => {
                    self.stored_pos_count_long = self.stored_pos_count_long
                        .checked_add(1).ok_or(RiskError::CorruptState)?;
                    if self.stored_pos_count_long > self.params.max_active_positions_per_side {
                        return Err(RiskError::Overflow);
                    }
                }
                Side::Short => {
                    self.stored_pos_count_short = self.stored_pos_count_short
                        .checked_add(1).ok_or(RiskError::CorruptState)?;
                    if self.stored_pos_count_short > self.params.max_active_positions_per_side {
                        return Err(RiskError::Overflow);
                    }
                }
            }
        }

        self.accounts[idx].position_basis_q = new_basis;
        Ok(())
    }
    }

    /// attach_effective_position (spec §4.5)
    test_visible! {
    fn attach_effective_position(&mut self, idx: usize, new_eff_pos_q: i128) -> Result<()> {
        // Before replacing a nonzero same-epoch basis, account for the fractional
        // remainder that will be orphaned (dynamic dust accounting).
        let old_basis = self.accounts[idx].position_basis_q;
        if old_basis != 0 {
            if let Some(old_side) = side_of_i128(old_basis) {
                let epoch_snap = self.accounts[idx].adl_epoch_snap;
                let epoch_side = self.get_epoch_side(old_side);
                if epoch_snap == epoch_side {
                    let a_basis = self.accounts[idx].adl_a_basis;
                    if a_basis != 0 {
                        let a_side = self.get_a_side(old_side);
                        let abs_basis = old_basis.unsigned_abs();
                        // Use U256 for the intermediate product to avoid u128 overflow
                        let product = U256::from_u128(abs_basis)
                            .checked_mul(U256::from_u128(a_side));
                        if let Some(p) = product {
                            let rem = p.checked_rem(U256::from_u128(a_basis));
                            if let Some(r) = rem {
                                if !r.is_zero() {
                                    self.inc_phantom_dust_bound(old_side)?;
                                }
                            }
                        }
                    }
                }
            }
        }

        if new_eff_pos_q == 0 {
            self.set_position_basis_q(idx, 0i128)?;
            // Reset to canonical zero-position defaults (spec §2.4)
            self.accounts[idx].adl_a_basis = ADL_ONE;
            self.accounts[idx].adl_k_snap = 0i128;
            self.accounts[idx].f_snap = 0i128;
            self.accounts[idx].adl_epoch_snap = 0;
        } else {
            // Spec §4.6: abs(new_eff_pos_q) <= MAX_POSITION_ABS_Q
            if new_eff_pos_q.unsigned_abs() > MAX_POSITION_ABS_Q {
                return Err(RiskError::Overflow);
            }
            let side = side_of_i128(new_eff_pos_q).ok_or(RiskError::CorruptState)?;
            self.set_position_basis_q(idx, new_eff_pos_q)?;

            match side {
                Side::Long => {
                    self.accounts[idx].adl_a_basis = self.adl_mult_long;
                    self.accounts[idx].adl_k_snap = self.adl_coeff_long;
                    self.accounts[idx].f_snap = self.f_long_num;
                    self.accounts[idx].adl_epoch_snap = self.adl_epoch_long;
                }
                Side::Short => {
                    self.accounts[idx].adl_a_basis = self.adl_mult_short;
                    self.accounts[idx].adl_k_snap = self.adl_coeff_short;
                    self.accounts[idx].f_snap = self.f_short_num;
                    self.accounts[idx].adl_epoch_snap = self.adl_epoch_short;
                }
            }
        }
        Ok(())
    }
    }

    // ========================================================================
    // Side state accessors
    // ========================================================================

    fn get_a_side(&self, s: Side) -> u128 {
        match s {
            Side::Long => self.adl_mult_long,
            Side::Short => self.adl_mult_short,
        }
    }

    fn get_k_side(&self, s: Side) -> i128 {
        match s {
            Side::Long => self.adl_coeff_long,
            Side::Short => self.adl_coeff_short,
        }
    }

    fn get_epoch_side(&self, s: Side) -> u64 {
        match s {
            Side::Long => self.adl_epoch_long,
            Side::Short => self.adl_epoch_short,
        }
    }

    fn get_k_epoch_start(&self, s: Side) -> i128 {
        match s {
            Side::Long => self.adl_epoch_start_k_long,
            Side::Short => self.adl_epoch_start_k_short,
        }
    }

    fn get_f_side(&self, s: Side) -> i128 {
        match s {
            Side::Long => self.f_long_num,
            Side::Short => self.f_short_num,
        }
    }

    fn get_f_epoch_start(&self, s: Side) -> i128 {
        match s {
            Side::Long => self.f_epoch_start_long_num,
            Side::Short => self.f_epoch_start_short_num,
        }
    }

    fn get_side_mode(&self, s: Side) -> SideMode {
        match s {
            Side::Long => self.side_mode_long,
            Side::Short => self.side_mode_short,
        }
    }

    fn get_oi_eff(&self, s: Side) -> u128 {
        match s {
            Side::Long => self.oi_eff_long_q,
            Side::Short => self.oi_eff_short_q,
        }
    }

    fn set_oi_eff(&mut self, s: Side, v: u128) {
        match s {
            Side::Long => self.oi_eff_long_q = v,
            Side::Short => self.oi_eff_short_q = v,
        }
    }

    fn set_side_mode(&mut self, s: Side, m: SideMode) {
        match s {
            Side::Long => self.side_mode_long = m,
            Side::Short => self.side_mode_short = m,
        }
    }

    fn set_a_side(&mut self, s: Side, v: u128) {
        match s {
            Side::Long => self.adl_mult_long = v,
            Side::Short => self.adl_mult_short = v,
        }
    }

    fn set_k_side(&mut self, s: Side, v: i128) {
        match s {
            Side::Long => self.adl_coeff_long = v,
            Side::Short => self.adl_coeff_short = v,
        }
    }

    /// Compute per-account F-delta PnL (v12.15).
    /// result = floor(abs_basis * (f_now - f_snap) / (den * FUNDING_DEN))
    /// Uses I256/U256 wide arithmetic to avoid i128 overflow.
    /// Mirrors the pattern of wide_signed_mul_div_floor_from_k_pair.
    /// Combined K/F settlement helper (spec v12.17 §1.6).
    /// floor(abs_basis * ((k_now - k_then) * FUNDING_DEN + (f_now - f_then)) / (den * FUNDING_DEN))
    /// Uses exact 256-bit intermediates. Single floor on the combined numerator.
    fn compute_kf_pnl_delta(
        abs_basis: u128, k_snap: i128, k_now: i128,
        f_snap: i128, f_now: i128, den: u128
    ) -> Result<i128> {
        if abs_basis == 0 { return Ok(0); }
        // K_diff in I256 — can reach 2*i128::MAX for opposing-sign K snapshots.
        let k_diff = I256::from_i128(k_now).checked_sub(I256::from_i128(k_snap))
            .ok_or(RiskError::Overflow)?;
        // K_diff * FUNDING_DEN in exact I256 via abs/sign decomposition.
        // No narrowing through i128 or u128 — stays in U256/I256 throughout.
        let k_scaled = if k_diff.is_zero() {
            I256::ZERO
        } else {
            let neg = k_diff.is_negative();
            if k_diff == I256::MIN { return Err(RiskError::Overflow); }
            let abs_k = k_diff.abs_u256();
            let prod_u256 = abs_k.checked_mul(U256::from_u128(FUNDING_DEN))
                .ok_or(RiskError::Overflow)?;
            let pos = I256::from_u256_or_overflow(prod_u256)
                .ok_or(RiskError::Overflow)?;
            if neg { I256::ZERO.checked_sub(pos).ok_or(RiskError::Overflow)? }
            else { pos }
        };
        // F_diff
        let f_diff = I256::from_i128(f_now).checked_sub(I256::from_i128(f_snap))
            .ok_or(RiskError::Overflow)?;
        // Combined numerator = K_diff * FUNDING_DEN + F_diff
        let combined = k_scaled.checked_add(f_diff).ok_or(RiskError::Overflow)?;
        if combined.is_zero() { return Ok(0); }
        // abs_basis * |combined| / (den * FUNDING_DEN), floor toward -inf
        let negative = combined.is_negative();
        if combined == I256::MIN { return Err(RiskError::Overflow); }
        let abs_combined = combined.abs_u256();
        let abs_basis_u256 = U256::from_u128(abs_basis);
        let den_wide = U256::from_u128(den).checked_mul(U256::from_u128(FUNDING_DEN))
            .ok_or(RiskError::Overflow)?;
        let p = abs_basis_u256.checked_mul(abs_combined).ok_or(RiskError::Overflow)?;
        let (q, rem) = wide_math::div_rem_u256(p, den_wide);
        if negative {
            let mag = if !rem.is_zero() {
                q.checked_add(U256::ONE).ok_or(RiskError::Overflow)?
            } else { q };
            let mag_u128 = mag.try_into_u128().ok_or(RiskError::Overflow)?;
            if mag_u128 > i128::MAX as u128 { return Err(RiskError::Overflow); }
            Ok(-(mag_u128 as i128))
        } else {
            let q_u128 = q.try_into_u128().ok_or(RiskError::Overflow)?;
            if q_u128 > i128::MAX as u128 { return Err(RiskError::Overflow); }
            Ok(q_u128 as i128)
        }
    }


    /// Wide variant of compute_kf_pnl_delta that accepts I256 for k_now/f_now.
    /// Used by resolved reconciliation where K_epoch_start + terminal_delta may exceed i128.
    fn compute_kf_pnl_delta_wide(
        abs_basis: u128, k_snap: i128, k_now_wide: I256,
        f_snap: i128, f_now_wide: I256, den: u128
    ) -> Result<i128> {
        if abs_basis == 0 { return Ok(0); }
        let k_diff = k_now_wide.checked_sub(I256::from_i128(k_snap))
            .ok_or(RiskError::Overflow)?;
        let k_scaled = if k_diff.is_zero() {
            I256::ZERO
        } else {
            let neg = k_diff.is_negative();
            if k_diff == I256::MIN { return Err(RiskError::Overflow); }
            let abs_k = k_diff.abs_u256();
            let prod_u256 = abs_k.checked_mul(U256::from_u128(FUNDING_DEN))
                .ok_or(RiskError::Overflow)?;
            let pos = I256::from_u256_or_overflow(prod_u256)
                .ok_or(RiskError::Overflow)?;
            if neg { I256::ZERO.checked_sub(pos).ok_or(RiskError::Overflow)? }
            else { pos }
        };
        let f_diff = f_now_wide.checked_sub(I256::from_i128(f_snap))
            .ok_or(RiskError::Overflow)?;
        let combined = k_scaled.checked_add(f_diff).ok_or(RiskError::Overflow)?;
        if combined.is_zero() { return Ok(0); }
        let negative = combined.is_negative();
        if combined == I256::MIN { return Err(RiskError::Overflow); }
        let abs_combined = combined.abs_u256();
        let abs_basis_u256 = U256::from_u128(abs_basis);
        let den_wide = U256::from_u128(den).checked_mul(U256::from_u128(FUNDING_DEN))
            .ok_or(RiskError::Overflow)?;
        let p = abs_basis_u256.checked_mul(abs_combined).ok_or(RiskError::Overflow)?;
        let (q, rem) = wide_math::div_rem_u256(p, den_wide);
        if negative {
            let mag = if !rem.is_zero() {
                q.checked_add(U256::ONE).ok_or(RiskError::Overflow)?
            } else { q };
            let mag_u128 = mag.try_into_u128().ok_or(RiskError::Overflow)?;
            if mag_u128 > i128::MAX as u128 { return Err(RiskError::Overflow); }
            Ok(-(mag_u128 as i128))
        } else {
            let q_u128 = q.try_into_u128().ok_or(RiskError::Overflow)?;
            if q_u128 > i128::MAX as u128 { return Err(RiskError::Overflow); }
            Ok(q_u128 as i128)
        }
    }

    fn get_stale_count(&self, s: Side) -> u64 {
        match s {
            Side::Long => self.stale_account_count_long,
            Side::Short => self.stale_account_count_short,
        }
    }

    fn set_stale_count(&mut self, s: Side, v: u64) {
        match s {
            Side::Long => self.stale_account_count_long = v,
            Side::Short => self.stale_account_count_short = v,
        }
    }

    fn get_stored_pos_count(&self, s: Side) -> u64 {
        match s {
            Side::Long => self.stored_pos_count_long,
            Side::Short => self.stored_pos_count_short,
        }
    }

    /// Spec §4.6: increment phantom dust bound by 1 q-unit (checked).
    fn inc_phantom_dust_bound(&mut self, s: Side) -> Result<()> {
        match s {
            Side::Long => {
                self.phantom_dust_bound_long_q = self.phantom_dust_bound_long_q
                    .checked_add(1u128)
                    .ok_or(RiskError::Overflow)?;
            }
            Side::Short => {
                self.phantom_dust_bound_short_q = self.phantom_dust_bound_short_q
                    .checked_add(1u128)
                    .ok_or(RiskError::Overflow)?;
            }
        }
        Ok(())
    }

    /// Spec §4.6.1: increment phantom dust bound by amount_q (checked).
    fn inc_phantom_dust_bound_by(&mut self, s: Side, amount_q: u128) -> Result<()> {
        match s {
            Side::Long => {
                self.phantom_dust_bound_long_q = self.phantom_dust_bound_long_q
                    .checked_add(amount_q)
                    .ok_or(RiskError::Overflow)?;
            }
            Side::Short => {
                self.phantom_dust_bound_short_q = self.phantom_dust_bound_short_q
                    .checked_add(amount_q)
                    .ok_or(RiskError::Overflow)?;
            }
        }
        Ok(())
    }

    // ========================================================================
    // effective_pos_q (spec §5.2)
    // ========================================================================

    /// Compute effective position quantity for account idx.
    pub fn effective_pos_q(&self, idx: usize) -> i128 {
        let basis = self.accounts[idx].position_basis_q;
        if basis == 0 {
            return 0i128;
        }

        let side = side_of_i128(basis).unwrap();
        let epoch_snap = self.accounts[idx].adl_epoch_snap;
        let epoch_side = self.get_epoch_side(side);

        if epoch_snap != epoch_side {
            // Epoch mismatch → effective position is 0 for current-market risk
            return 0i128;
        }

        let a_side = self.get_a_side(side);
        let a_basis = self.accounts[idx].adl_a_basis;

        if a_basis == 0 {
            // a_basis==0 with nonzero basis is corrupt; with zero basis it's pre-attach/missing.
            // Both return 0 (treating as flat). Callers of mutation paths should
            // check basis != 0 && a_basis == 0 separately if they need to reject.
            return 0i128;
        }

        let abs_basis = basis.unsigned_abs();
        // floor(|basis| * A_s / a_basis)
        let effective_abs = mul_div_floor_u128(abs_basis, a_side, a_basis);

        if basis < 0 {
            if effective_abs == 0 {
                0i128
            } else {
                if effective_abs > i128::MAX as u128 { return 0; } // unreachable under configured bounds
                -(effective_abs as i128)
            }
        } else {
            if effective_abs > i128::MAX as u128 { return 0; } // unreachable under configured bounds
            effective_abs as i128
        }
    }

    /// settle_side_effects_live (spec §5.3, v12.14.0) — routes PnL delta
    /// through set_pnl_with_reserve with UseHLock for cohort queue.
    test_visible! {
    fn settle_side_effects_live(&mut self, idx: usize, ctx: &mut InstructionContext) -> Result<()> {
        let basis = self.accounts[idx].position_basis_q;
        if basis == 0 { return Ok(()); }

        let side = side_of_i128(basis).unwrap();
        let epoch_snap = self.accounts[idx].adl_epoch_snap;
        let epoch_side = self.get_epoch_side(side);
        let a_basis = self.accounts[idx].adl_a_basis;
        if a_basis == 0 { return Err(RiskError::CorruptState); }
        let abs_basis = basis.unsigned_abs();

        if epoch_snap == epoch_side {
            // Same epoch
            let a_side = self.get_a_side(side);
            let k_side = self.get_k_side(side);
            let k_snap = self.accounts[idx].adl_k_snap;
            let q_eff_new = mul_div_floor_u128(abs_basis, a_side, a_basis);
            let den = a_basis.checked_mul(POS_SCALE).ok_or(RiskError::Overflow)?;
            // Combined K/F settlement — single floor (spec v12.17 §1.6)
            let f_side = self.get_f_side(side);
            let f_snap = self.accounts[idx].f_snap;
            let pnl_delta = Self::compute_kf_pnl_delta(abs_basis, k_snap, k_side, f_snap, f_side, den)?;

            let new_pnl = self.accounts[idx].pnl.checked_add(pnl_delta)
                .ok_or(RiskError::Overflow)?;
            if new_pnl == i128::MIN { return Err(RiskError::Overflow); }

            self.set_pnl_with_reserve(idx, new_pnl, ReserveMode::UseAdmissionPair(ctx.admit_h_min_shared, ctx.admit_h_max_shared), Some(ctx))?;

            if q_eff_new == 0 {
                self.inc_phantom_dust_bound(side)?;
                self.set_position_basis_q(idx, 0i128)?;
                self.accounts[idx].adl_a_basis = ADL_ONE;
                self.accounts[idx].adl_k_snap = 0i128;
                self.accounts[idx].f_snap = 0i128;
                self.accounts[idx].adl_epoch_snap = 0;
            } else {
                self.accounts[idx].adl_k_snap = k_side;
                self.accounts[idx].f_snap = f_side;
                self.accounts[idx].adl_epoch_snap = epoch_side;
            }
        } else {
            // Epoch mismatch — validate then mutate
            let side_mode = self.get_side_mode(side);
            if side_mode != SideMode::ResetPending { return Err(RiskError::CorruptState); }
            if epoch_snap.checked_add(1) != Some(epoch_side) { return Err(RiskError::CorruptState); }

            let k_epoch_start = self.get_k_epoch_start(side);
            let k_snap = self.accounts[idx].adl_k_snap;
            let den = a_basis.checked_mul(POS_SCALE).ok_or(RiskError::Overflow)?;
            // Combined K/F settlement for epoch mismatch (spec v12.17 §1.6)
            let f_end = self.get_f_epoch_start(side);
            let f_snap = self.accounts[idx].f_snap;
            let pnl_delta = Self::compute_kf_pnl_delta(abs_basis, k_snap, k_epoch_start, f_snap, f_end, den)?;

            let new_pnl = self.accounts[idx].pnl.checked_add(pnl_delta)
                .ok_or(RiskError::Overflow)?;
            if new_pnl == i128::MIN { return Err(RiskError::Overflow); }

            let old_stale = self.get_stale_count(side);
            let new_stale = old_stale.checked_sub(1).ok_or(RiskError::CorruptState)?;

            // Mutate
            self.set_pnl_with_reserve(idx, new_pnl, ReserveMode::UseAdmissionPair(ctx.admit_h_min_shared, ctx.admit_h_max_shared), Some(ctx))?;
            self.set_position_basis_q(idx, 0i128)?;
            self.set_stale_count(side, new_stale);
            self.accounts[idx].adl_a_basis = ADL_ONE;
            self.accounts[idx].adl_k_snap = 0i128;
            self.accounts[idx].f_snap = 0i128;
            self.accounts[idx].adl_epoch_snap = 0;
        }

        Ok(())
    }

    }

    // ========================================================================
    // accrue_market_to (spec §5.4)
    // ========================================================================

    pub fn accrue_market_to(&mut self, now_slot: u64, oracle_price: u64, funding_rate_e9: i128) -> Result<()> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        // Validate funding rate bound (spec §1.4, folded into accrue per v12.16.4)
        if funding_rate_e9.unsigned_abs() > self.params.max_abs_funding_e9_per_slot as u128 {
            return Err(RiskError::Overflow);
        }

        // Time monotonicity (spec §5.4 preconditions)
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        if now_slot < self.last_market_slot {
            return Err(RiskError::Overflow);
        }

        // Step 4: snapshot OI at start (fixed for all sub-steps per spec §5.4)
        let long_live = self.oi_eff_long_q != 0;
        let short_live = self.oi_eff_short_q != 0;

        let total_dt = now_slot.saturating_sub(self.last_market_slot);
        if total_dt == 0 && self.last_oracle_price == oracle_price {
            // Step 5: no change — set current_slot and return (spec §5.4)
            self.current_slot = now_slot;
            return Ok(());
        }

        // Spec §5.5 clause 6: enforce per-call dt envelope.
        // Together with init-time envelope (§1.4), guarantees F_side_num fits i128.
        if total_dt > self.params.max_accrual_dt_slots {
            return Err(RiskError::Overflow);
        }

        // Use scratch K values for the entire mark + funding computation.
        // Only commit to engine state after ALL computations succeed.
        // This prevents partial K advancement on mid-function errors.
        let mut k_long = self.adl_coeff_long;
        let mut k_short = self.adl_coeff_short;

        // Step 5: Mark-to-market (once, spec §1.5 item 21)
        let current_price = self.last_oracle_price;
        let delta_p = (oracle_price as i128).checked_sub(current_price as i128)
            .ok_or(RiskError::Overflow)?;
        if delta_p != 0 {
            // Compute mark deltas in I256, only fail when final K doesn't fit i128.
            // This avoids false overflow when delta magnitude > i128::MAX but
            // current K has opposite sign so the sum still fits.
            let delta_p_wide = I256::from_i128(delta_p);
            if long_live {
                let a_long_wide = I256::from_u128(self.adl_mult_long);
                let dk_wide = a_long_wide.checked_mul_i256(delta_p_wide)
                    .ok_or(RiskError::Overflow)?;
                let k_long_wide = I256::from_i128(k_long).checked_add(dk_wide)
                    .ok_or(RiskError::Overflow)?;
                k_long = k_long_wide.try_into_i128().ok_or(RiskError::Overflow)?;
            }
            if short_live {
                let a_short_wide = I256::from_u128(self.adl_mult_short);
                let dk_wide = a_short_wide.checked_mul_i256(delta_p_wide)
                    .ok_or(RiskError::Overflow)?;
                let k_short_wide = I256::from_i128(k_short).checked_sub(dk_wide)
                    .ok_or(RiskError::Overflow)?;
                k_short = k_short_wide.try_into_i128().ok_or(RiskError::Overflow)?;
            }
        }

        // Step 8: Funding transfer — one exact total delta (spec v12.16.5 §5.5).
        // fund_num_total = fund_px_0 * funding_rate_e9_per_slot * dt
        // computed in exact wide signed domain. No substep loop.
        let mut f_long = self.f_long_num;
        let mut f_short = self.f_short_num;
        if funding_rate_e9 != 0 && total_dt > 0 && long_live && short_live {
            let fund_px_0 = self.fund_px_last;

            if fund_px_0 > 0 {
                // Exact computation in I256: fund_num_total = fund_px_0 * rate * dt
                // Only fail when final persisted F doesn't fit i128.
                let px_wide = I256::from_u128(fund_px_0 as u128);
                let rate_wide = I256::from_i128(funding_rate_e9);
                let dt_wide = I256::from_u128(total_dt as u128);
                let fund_num_total_wide = px_wide.checked_mul_i256(rate_wide)
                    .ok_or(RiskError::Overflow)?
                    .checked_mul_i256(dt_wide)
                    .ok_or(RiskError::Overflow)?;

                // F_long -= A_long * fund_num_total
                let a_long_wide = I256::from_u128(self.adl_mult_long);
                let df_long_wide = a_long_wide.checked_mul_i256(fund_num_total_wide)
                    .ok_or(RiskError::Overflow)?;
                let f_long_wide = I256::from_i128(f_long).checked_sub(df_long_wide)
                    .ok_or(RiskError::Overflow)?;
                f_long = f_long_wide.try_into_i128().ok_or(RiskError::Overflow)?;

                // F_short += A_short * fund_num_total
                let a_short_wide = I256::from_u128(self.adl_mult_short);
                let df_short_wide = a_short_wide.checked_mul_i256(fund_num_total_wide)
                    .ok_or(RiskError::Overflow)?;
                let f_short_wide = I256::from_i128(f_short).checked_add(df_short_wide)
                    .ok_or(RiskError::Overflow)?;
                f_short = f_short_wide.try_into_i128().ok_or(RiskError::Overflow)?;
            }
        }

        // ALL computations succeeded — commit K/F values and synchronize state
        self.adl_coeff_long = k_long;
        self.adl_coeff_short = k_short;
        self.f_long_num = f_long;
        self.f_short_num = f_short;
        self.current_slot = now_slot;
        self.last_market_slot = now_slot;
        self.last_oracle_price = oracle_price;
        self.fund_px_last = oracle_price;

        Ok(())
    }

    /// Validate h_lock before any state mutation.
    #[cfg_attr(any(feature = "test", feature = "stress", kani), doc(hidden))]
    pub fn validate_admission_pair(admit_h_min: u64, admit_h_max: u64, params: &RiskParams) -> Result<()> {
        // spec §1.4: for live instructions that may create fresh reserve,
        // admit_h_max > 0 and admit_h_max >= cfg_h_min.
        // admit_h_max == 0 would bypass admission entirely (0 returned regardless
        // of state), breaking the h=1 invariant. Reject.
        if admit_h_max == 0 { return Err(RiskError::Overflow); }
        if admit_h_max < params.h_min { return Err(RiskError::Overflow); }
        // 0 <= admit_h_min <= admit_h_max <= cfg_h_max
        if admit_h_min > admit_h_max { return Err(RiskError::Overflow); }
        if admit_h_max > params.h_max { return Err(RiskError::Overflow); }
        // if admit_h_min > 0, then admit_h_min >= cfg_h_min
        if admit_h_min > 0 && admit_h_min < params.h_min { return Err(RiskError::Overflow); }
        Ok(())
    }

    // ========================================================================
    // absorb_protocol_loss (spec §4.7)
    // ========================================================================

    /// use_insurance_buffer (spec §4.11): deduct loss from insurance down to floor,
    /// return the remaining uninsured loss.
    fn use_insurance_buffer(&mut self, loss: u128) -> u128 {
        if loss == 0 {
            return 0;
        }
        let ins_bal = self.insurance_fund.balance.get();
        let available = ins_bal.saturating_sub(self.params.insurance_floor.get());
        let pay = core::cmp::min(loss, available);
        if pay > 0 {
            self.insurance_fund.balance = U128::new(ins_bal - pay);
        }
        loss - pay
    }

    /// record_uninsured_protocol_loss (spec §4.17): bookkeeping no-op.
    ///
    /// After insurance is drained, any remaining uninsured loss is already
    /// implicitly represented by the junior haircut mechanism: the forgiven
    /// negative PnL leaves the matched positive PnL (matured_pos_tot) as an
    /// unchanged claim against Residual = V - C_tot - I. When
    /// matured_pos_tot > Residual, payouts scale by h = Residual/matured.
    ///
    /// MUST NOT drain V here — doing so would shrink Residual below its
    /// natural post-forgiveness value and double-penalize junior holders
    /// (first via h < 1, again via V reduction).
    ///
    /// Intuition: Alice +100, Bob -100, V = 50, insurance = 0. Forgiving Bob
    /// leaves matured = 100, residual = 50 → h = 0.5, Alice gets 50. If we
    /// also drained V by 50, residual would drop to 0 → Alice gets 0.
    #[allow(unused_variables)]
    fn record_uninsured_protocol_loss(&mut self, loss: u128) {
        // Intentional no-op. See doc comment.
    }

    /// absorb_protocol_loss (spec §4.17): use_insurance_buffer then
    /// record_uninsured_protocol_loss for any remainder.
    test_visible! {
    fn absorb_protocol_loss(&mut self, loss: u128) {
        if loss == 0 {
            return;
        }
        let rem = self.use_insurance_buffer(loss);
        self.record_uninsured_protocol_loss(rem);
    }
    }

    // ========================================================================
    // sync_account_fee_to_slot (spec §4.6.1, v12.18.4)
    // ========================================================================

    /// Internal helper that realizes wrapper-owned recurring maintenance fees
    /// for account `idx` over `[last_fee_slot, fee_slot_anchor]` at the given
    /// per-slot rate, then advances `last_fee_slot`.
    ///
    /// Preconditions:
    /// - `idx` is materialized
    /// - `fee_slot_anchor >= last_fee_slot` (monotonicity)
    /// - on Live:     `fee_slot_anchor <= current_slot`
    /// - on Resolved: `fee_slot_anchor <= resolved_slot`
    ///
    /// Behavior:
    /// - `fee_abs_raw = fee_rate_per_slot * dt` in wide U256 to prevent overflow.
    /// - Cap at `MAX_PROTOCOL_FEE_ABS` (spec §4.6.1 step 4 — liveness cap).
    /// - Route the capped amount through `charge_fee_to_insurance` so the
    ///   collectible portion moves C → I and any shortfall becomes local
    ///   fee debt; uncollectible tail is dropped.
    /// - Advance `last_fee_slot` to `fee_slot_anchor`.
    ///
    /// Kept test-visible so tests and Kani proofs can exercise the explicit
    /// anchor path. The public entrypoint (`sync_account_fee_to_slot_not_atomic`)
    /// does NOT accept a caller-supplied anchor; it derives the anchor from
    /// market mode (current_slot on Live, resolved_slot on Resolved).
    test_visible! {
    fn sync_account_fee_to_slot(
        &mut self,
        idx: usize,
        fee_slot_anchor: u64,
        fee_rate_per_slot: u128,
    ) -> Result<()> {
        if idx >= MAX_ACCOUNTS || !self.is_used(idx) {
            return Err(RiskError::AccountNotFound);
        }
        let last = self.accounts[idx].last_fee_slot;
        if fee_slot_anchor < last { return Err(RiskError::Overflow); }
        // Mode-specific upper bound on the anchor.
        match self.market_mode {
            MarketMode::Live => {
                if fee_slot_anchor > self.current_slot {
                    return Err(RiskError::Overflow);
                }
            }
            MarketMode::Resolved => {
                if fee_slot_anchor > self.resolved_slot {
                    return Err(RiskError::Overflow);
                }
            }
        }
        let dt = fee_slot_anchor - last;
        if dt == 0 {
            // No-op at same anchor; still idempotent-advance (already at anchor).
            return Ok(());
        }
        if fee_rate_per_slot == 0 {
            self.accounts[idx].last_fee_slot = fee_slot_anchor;
            return Ok(());
        }
        // Exact wide multiply; cap at MAX_PROTOCOL_FEE_ABS for liveness.
        let raw = U256::from_u128(fee_rate_per_slot)
            .checked_mul(U256::from_u128(dt as u128))
            .ok_or(RiskError::Overflow)?;
        let cap = U256::from_u128(MAX_PROTOCOL_FEE_ABS);
        let fee_abs_u256 = if raw > cap { cap } else { raw };
        let fee_abs: u128 = fee_abs_u256.try_into_u128().ok_or(RiskError::Overflow)?;
        if fee_abs > 0 {
            self.charge_fee_to_insurance(idx, fee_abs)?;
        }
        self.accounts[idx].last_fee_slot = fee_slot_anchor;
        Ok(())
    }
    }

    // ========================================================================
    // enqueue_adl (spec §5.6)
    // ========================================================================

    test_visible! {
    fn enqueue_adl(&mut self, ctx: &mut InstructionContext, liq_side: Side, q_close_q: u128, d: u128) -> Result<()> {
        let opp = opposite_side(liq_side);

        // Step 1: decrease liquidated side OI (checked — underflow is corrupt state)
        if q_close_q != 0 {
            let old_oi = self.get_oi_eff(liq_side);
            let new_oi = old_oi.checked_sub(q_close_q).ok_or(RiskError::CorruptState)?;
            self.set_oi_eff(liq_side, new_oi);
        }

        // Step 2 (§5.6 step 2): insurance-first deficit coverage
        let d_rem = if d > 0 { self.use_insurance_buffer(d) } else { 0u128 };

        // Step 3: read opposing OI
        let oi = self.get_oi_eff(opp);

        // Step 4 (§5.6 step 4): if OI == 0
        if oi == 0 {
            if d_rem > 0 {
                self.record_uninsured_protocol_loss(d_rem);
            }
            if self.get_oi_eff(liq_side) == 0 {
                set_pending_reset(ctx, liq_side);
                set_pending_reset(ctx, opp);
            }
            return Ok(());
        }

        // Step 5 (§5.6 step 5): if OI > 0 and stored_pos_count_opp == 0,
        // route deficit through record_uninsured and do NOT modify K_opp.
        if self.get_stored_pos_count(opp) == 0 {
            if q_close_q > oi {
                return Err(RiskError::CorruptState);
            }
            let oi_post = oi.checked_sub(q_close_q).ok_or(RiskError::Overflow)?;
            if d_rem > 0 {
                self.record_uninsured_protocol_loss(d_rem);
            }
            self.set_oi_eff(opp, oi_post);
            if oi_post == 0 {
                // Unconditionally reset the drained opp side (fixes phantom dust revert).
                set_pending_reset(ctx, opp);
                // Also reset liq_side only if it too has zero OI
                if self.get_oi_eff(liq_side) == 0 {
                    set_pending_reset(ctx, liq_side);
                }
            }
            return Ok(());
        }

        // Step 6 (§5.6 step 6): require q_close_q <= OI
        if q_close_q > oi {
            return Err(RiskError::CorruptState);
        }

        let a_old = self.get_a_side(opp);
        let oi_post = oi.checked_sub(q_close_q).ok_or(RiskError::Overflow)?;

        // Step 7 (§5.6 step 7): handle D_rem > 0 (quote deficit after insurance)
        // Fused delta_K_abs = ceil(D_rem * A_old * POS_SCALE / OI)
        // Per §1.5 Rule 14: if the quotient doesn't fit in i128, route to
        // record_uninsured_protocol_loss instead of panicking.
        if d_rem != 0 {
            let a_ps = a_old.checked_mul(POS_SCALE).ok_or(RiskError::Overflow)?;
            match wide_mul_div_ceil_u128_or_over_i128max(d_rem, a_ps, oi) {
                Ok(delta_k_abs) => {
                    let delta_k = -(delta_k_abs as i128);
                    let k_opp = self.get_k_side(opp);
                    match k_opp.checked_add(delta_k) {
                        Some(new_k) => {
                            self.set_k_side(opp, new_k);
                        }
                        None => {
                            // K-space overflow: route D_rem through record_uninsured (spec §1.7 clause 12).
                            self.record_uninsured_protocol_loss(d_rem);
                        }
                    }
                }
                Err(OverI128Magnitude) => {
                    // Quotient overflow: route D_rem through record_uninsured (spec §1.7 clause 12).
                    self.record_uninsured_protocol_loss(d_rem);
                }
            }
        }

        // Step 8 (§5.6 step 8): if OI_post == 0
        if oi_post == 0 {
            self.set_oi_eff(opp, 0u128);
            set_pending_reset(ctx, opp);
            if self.get_oi_eff(liq_side) == 0 {
                set_pending_reset(ctx, liq_side);
            }
            return Ok(());
        }

        // Steps 8-9: compute A_candidate and A_trunc_rem using U256 intermediates
        let a_old_u256 = U256::from_u128(a_old);
        let oi_post_u256 = U256::from_u128(oi_post);
        let oi_u256 = U256::from_u128(oi);
        let (a_candidate_u256, a_trunc_rem) = mul_div_floor_u256_with_rem(
            a_old_u256,
            oi_post_u256,
            oi_u256,
        );

        // Step 10: A_candidate > 0
        if !a_candidate_u256.is_zero() {
            let a_new = a_candidate_u256.try_into_u128().ok_or(RiskError::Overflow)?;
            self.set_a_side(opp, a_new);
            self.set_oi_eff(opp, oi_post);
            // Spec §5.6 step 10: increment phantom dust when OI actually decreased
            if oi_post < oi {
                let n_opp = self.get_stored_pos_count(opp) as u128;
                let n_opp_u256 = U256::from_u128(n_opp);
                // global_a_dust_bound = N_opp + ceil((OI_before + N_opp) / A_old)
                let oi_plus_n = oi_u256.checked_add(n_opp_u256).unwrap_or(U256::MAX);
                let ceil_term = ceil_div_positive_checked(oi_plus_n, a_old_u256);
                let global_a_dust_bound = n_opp_u256.checked_add(ceil_term)
                    .unwrap_or(U256::MAX);
                let bound_u128 = global_a_dust_bound.try_into_u128().unwrap_or(u128::MAX);
                self.inc_phantom_dust_bound_by(opp, bound_u128)?;
            }
            if a_new < MIN_A_SIDE {
                self.set_side_mode(opp, SideMode::DrainOnly);
            }
            return Ok(());
        }

        // Step 11: precision exhaustion terminal drain
        self.set_oi_eff(opp, 0u128);
        self.set_oi_eff(liq_side, 0u128);
        set_pending_reset(ctx, opp);
        set_pending_reset(ctx, liq_side);

        Ok(())
    }
    }

    // ========================================================================
    // begin_full_drain_reset / finalize_side_reset (spec §2.5, §2.7)
    // ========================================================================

    test_visible! {
    fn begin_full_drain_reset(&mut self, side: Side) -> Result<()> {
        // Require OI_eff_side == 0
        if self.get_oi_eff(side) != 0 { return Err(RiskError::CorruptState); }

        // K_epoch_start_side = K_side
        let k = self.get_k_side(side);
        match side {
            Side::Long => self.adl_epoch_start_k_long = k,
            Side::Short => self.adl_epoch_start_k_short = k,
        }

        // F_epoch_start_side = F_side (v12.15)
        match side {
            Side::Long => self.f_epoch_start_long_num = self.f_long_num,
            Side::Short => self.f_epoch_start_short_num = self.f_short_num,
        }

        // Increment epoch
        match side {
            Side::Long => self.adl_epoch_long = self.adl_epoch_long.checked_add(1)
                .ok_or(RiskError::Overflow)?,
            Side::Short => self.adl_epoch_short = self.adl_epoch_short.checked_add(1)
                .ok_or(RiskError::Overflow)?,
        }

        // A_side = ADL_ONE
        self.set_a_side(side, ADL_ONE);

        // stale_account_count_side = stored_pos_count_side
        let spc = self.get_stored_pos_count(side);
        self.set_stale_count(side, spc);

        // phantom_dust_bound_side_q = 0 (spec §2.5 step 6)
        match side {
            Side::Long => self.phantom_dust_bound_long_q = 0u128,
            Side::Short => self.phantom_dust_bound_short_q = 0u128,
        }

        // mode = ResetPending
        self.set_side_mode(side, SideMode::ResetPending);
        Ok(())
    }
    }

    test_visible! {
    fn finalize_side_reset(&mut self, side: Side) -> Result<()> {
        if self.get_side_mode(side) != SideMode::ResetPending {
            return Err(RiskError::CorruptState);
        }
        if self.get_oi_eff(side) != 0 {
            return Err(RiskError::CorruptState);
        }
        if self.get_stale_count(side) != 0 {
            return Err(RiskError::CorruptState);
        }
        if self.get_stored_pos_count(side) != 0 {
            return Err(RiskError::CorruptState);
        }
        self.set_side_mode(side, SideMode::Normal);
        Ok(())
    }
    }

    // ========================================================================
    // schedule_end_of_instruction_resets / finalize (spec §5.7-5.8)
    // ========================================================================

    test_visible! {
    fn schedule_end_of_instruction_resets(&mut self, ctx: &mut InstructionContext) -> Result<()> {
        // §5.7.A: Bilateral-empty dust clearance
        if self.stored_pos_count_long == 0 && self.stored_pos_count_short == 0 {
            let clear_bound_q = self.phantom_dust_bound_long_q
                .checked_add(self.phantom_dust_bound_short_q)
                .ok_or(RiskError::CorruptState)?;
            let has_residual = self.oi_eff_long_q != 0
                || self.oi_eff_short_q != 0
                || self.phantom_dust_bound_long_q != 0
                || self.phantom_dust_bound_short_q != 0;
            if has_residual {
                if self.oi_eff_long_q != self.oi_eff_short_q {
                    return Err(RiskError::CorruptState);
                }
                if self.oi_eff_long_q <= clear_bound_q && self.oi_eff_short_q <= clear_bound_q {
                    self.oi_eff_long_q = 0u128;
                    self.oi_eff_short_q = 0u128;
                    ctx.pending_reset_long = true;
                    ctx.pending_reset_short = true;
                } else {
                    return Err(RiskError::CorruptState);
                }
            }
        }
        // §5.7.B: Unilateral-empty long (long empty, short has positions)
        else if self.stored_pos_count_long == 0 && self.stored_pos_count_short > 0 {
            let has_residual = self.oi_eff_long_q != 0
                || self.oi_eff_short_q != 0
                || self.phantom_dust_bound_long_q != 0;
            if has_residual {
                if self.oi_eff_long_q != self.oi_eff_short_q {
                    return Err(RiskError::CorruptState);
                }
                if self.oi_eff_long_q <= self.phantom_dust_bound_long_q {
                    self.oi_eff_long_q = 0u128;
                    self.oi_eff_short_q = 0u128;
                    ctx.pending_reset_long = true;
                    ctx.pending_reset_short = true;
                } else {
                    return Err(RiskError::CorruptState);
                }
            }
        }
        // §5.7.C: Unilateral-empty short (short empty, long has positions)
        else if self.stored_pos_count_short == 0 && self.stored_pos_count_long > 0 {
            let has_residual = self.oi_eff_long_q != 0
                || self.oi_eff_short_q != 0
                || self.phantom_dust_bound_short_q != 0;
            if has_residual {
                if self.oi_eff_long_q != self.oi_eff_short_q {
                    return Err(RiskError::CorruptState);
                }
                if self.oi_eff_short_q <= self.phantom_dust_bound_short_q {
                    self.oi_eff_long_q = 0u128;
                    self.oi_eff_short_q = 0u128;
                    ctx.pending_reset_long = true;
                    ctx.pending_reset_short = true;
                } else {
                    return Err(RiskError::CorruptState);
                }
            }
        }

        // §5.7.D: DrainOnly sides with zero OI
        if self.side_mode_long == SideMode::DrainOnly && self.oi_eff_long_q == 0 {
            ctx.pending_reset_long = true;
        }
        if self.side_mode_short == SideMode::DrainOnly && self.oi_eff_short_q == 0 {
            ctx.pending_reset_short = true;
        }

        Ok(())
    }
    }

    test_visible! {
    fn finalize_end_of_instruction_resets(&mut self, ctx: &InstructionContext) -> Result<()> {
        if ctx.pending_reset_long && self.side_mode_long != SideMode::ResetPending {
            self.begin_full_drain_reset(Side::Long)?;
        }
        if ctx.pending_reset_short && self.side_mode_short != SideMode::ResetPending {
            self.begin_full_drain_reset(Side::Short)?;
        }
        self.maybe_finalize_ready_reset_sides();
        Ok(())
    }
    }

    /// Preflight finalize: if a side is ResetPending with OI=0, stale=0, pos_count=0,
    /// transition it back to Normal so fresh OI can be added.
    /// Called before OI-increase gating and at end-of-instruction.
    fn maybe_finalize_ready_reset_sides(&mut self) {
        if self.side_mode_long == SideMode::ResetPending
            && self.get_oi_eff(Side::Long) == 0
            && self.get_stale_count(Side::Long) == 0
            && self.get_stored_pos_count(Side::Long) == 0
        {
            self.set_side_mode(Side::Long, SideMode::Normal);
        }
        if self.side_mode_short == SideMode::ResetPending
            && self.get_oi_eff(Side::Short) == 0
            && self.get_stale_count(Side::Short) == 0
            && self.get_stored_pos_count(Side::Short) == 0
        {
            self.set_side_mode(Side::Short, SideMode::Normal);
        }
    }

    // ========================================================================
    // Haircut and Equity (spec §3)
    // ========================================================================

    /// Compute haircut ratio (h_num, h_den) as u128 pair (spec §3.3)
    /// Uses pnl_matured_pos_tot as denominator per v12.14.0.
    pub fn haircut_ratio(&self) -> (u128, u128) {
        if self.pnl_matured_pos_tot == 0 {
            return (1u128, 1u128);
        }
        let senior_sum = self.c_tot.get().checked_add(self.insurance_fund.balance.get());
        let residual: u128 = match senior_sum {
            Some(ss) => {
                if self.vault.get() >= ss {
                    self.vault.get() - ss
                } else {
                    0u128
                }
            }
            None => 0u128, // overflow in senior_sum → deficit
        };
        let h_num = if residual < self.pnl_matured_pos_tot { residual } else { self.pnl_matured_pos_tot };
        (h_num, self.pnl_matured_pos_tot)
    }

    /// PNL_eff_matured_i (spec §3.3): haircutted matured released positive PnL
    pub fn effective_matured_pnl(&self, idx: usize) -> u128 {
        let released = self.released_pos(idx);
        if released == 0 {
            return 0u128;
        }
        let (h_num, h_den) = self.haircut_ratio();
        if h_den == 0 {
            return released;
        }
        wide_mul_div_floor_u128(released, h_num, h_den)
    }

    /// Eq_maint_raw_i (spec §3.4): C_i + PNL_i - FeeDebt_i in exact widened signed domain.
    /// For maintenance margin and one-sided health checks. Uses full local PNL_i.
    /// Returns i128. Negative overflow is projected to i128::MIN + 1 per §3.4
    /// (safe for one-sided checks against nonneg thresholds). For strict
    /// before/after buffer comparisons, use account_equity_maint_raw_wide.
    pub fn account_equity_maint_raw(&self, account: &Account) -> i128 {
        let wide = self.account_equity_maint_raw_wide(account);
        match wide.try_into_i128() {
            Some(v) => v,
            None => {
                // Overflow in either direction: fail conservative (spec §3.4).
                // i128::MIN + 1 fails every > 0 and > MM_req gate.
                i128::MIN + 1
            }
        }
    }

    /// Eq_maint_raw_i in exact I256 (spec §3.4 "transient widened signed type").
    /// MUST be used for strict before/after raw maintenance-buffer comparisons
    /// (§10.5 step 29). No saturation or clamping.
    pub fn account_equity_maint_raw_wide(&self, account: &Account) -> I256 {
        let cap = I256::from_u128(account.capital.get());
        let pnl = I256::from_i128(account.pnl);
        let fee_debt = I256::from_u128(fee_debt_u128_checked(account.fee_credits.get()));

        // C + PNL - FeeDebt in exact I256 — cannot overflow 256 bits
        let sum = cap.checked_add(pnl).expect("I256 add overflow");
        sum.checked_sub(fee_debt).expect("I256 sub overflow")
    }

    /// Eq_net_i (spec §3.4): max(0, Eq_maint_raw_i). For maintenance margin checks.
    pub fn account_equity_net(&self, account: &Account, _oracle_price: u64) -> i128 {
        let raw = self.account_equity_maint_raw(account);
        if raw < 0 { 0i128 } else { raw }
    }

    /// Eq_init_raw_i (spec §3.4): C_i + min(PNL_i, 0) + PNL_eff_matured_i - FeeDebt_i
    /// For initial margin and withdrawal checks. Uses haircutted matured PnL only.
    /// Returns i128. Negative overflow projected to i128::MIN + 1 per §3.4.
    pub fn account_equity_init_raw(&self, account: &Account, idx: usize) -> i128 {
        let cap = I256::from_u128(account.capital.get());
        let neg_pnl = I256::from_i128(if account.pnl < 0 { account.pnl } else { 0i128 });
        let eff_matured = I256::from_u128(self.effective_matured_pnl(idx));
        let fee_debt = I256::from_u128(fee_debt_u128_checked(account.fee_credits.get()));

        let sum = cap.checked_add(neg_pnl).expect("I256 add overflow")
            .checked_add(eff_matured).expect("I256 add overflow")
            .checked_sub(fee_debt).expect("I256 sub overflow");

        match sum.try_into_i128() {
            Some(v) => v,
            None => {
                // Overflow in either direction: fail conservative.
                i128::MIN + 1
            }
        }
    }

    /// Eq_init_net_i (spec §3.4): max(0, Eq_init_raw_i). For IM checks (trades).
    pub fn account_equity_init_net(&self, account: &Account, idx: usize) -> i128 {
        let raw = self.account_equity_init_raw(account, idx);
        if raw < 0 { 0i128 } else { raw }
    }

    /// Eq_withdraw_raw_i (spec §3.5): C + min(PNL, 0) + PNL_eff_matured - FeeDebt.
    /// Uses exact I256 arithmetic. Includes haircutted matured released PnL.
    pub fn account_equity_withdraw_raw(&self, account: &Account, idx: usize) -> i128 {
        let cap = I256::from_u128(account.capital.get());
        let neg_pnl = I256::from_i128(if account.pnl < 0 { account.pnl } else { 0i128 });
        let eff_matured = I256::from_u128(self.effective_matured_pnl(idx));
        let fee_debt = I256::from_u128(fee_debt_u128_checked(account.fee_credits.get()));
        let sum = cap.checked_add(neg_pnl).expect("I256 add")
            .checked_add(eff_matured).expect("I256 add")
            .checked_sub(fee_debt).expect("I256 sub");
        match sum.try_into_i128() {
            Some(v) => v,
            None => i128::MIN + 1, // fail conservative on any overflow
        }
    }

    /// max_safe_flat_conversion_released (spec §4.12).
    /// Returns largest x_safe <= x_cap such that converting x_safe released profit
    /// on a live flat account cannot make Eq_maint_raw_i negative post-conversion.
    /// Uses 256-bit exact intermediates per spec §1.6 item 29.
    pub fn max_safe_flat_conversion_released(&self, idx: usize, x_cap: u128, h_num: u128, h_den: u128) -> u128 {
        if x_cap == 0 { return 0; }
        let e_before = self.account_equity_maint_raw(&self.accounts[idx]);
        if e_before <= 0 { return 0; }
        if h_den == 0 || h_num == h_den { return x_cap; }
        let haircut_loss_num = h_den - h_num;
        // min(x_cap, floor(E_before * h_den / haircut_loss_num))
        let safe = wide_mul_div_floor_u128(e_before as u128, h_den, haircut_loss_num);
        core::cmp::min(x_cap, safe)
    }

    /// notional (spec §9.1): floor(|effective_pos_q| * oracle_price / POS_SCALE)
    pub fn notional(&self, idx: usize, oracle_price: u64) -> u128 {
        let eff = self.effective_pos_q(idx);
        if eff == 0 {
            return 0;
        }
        let abs_eff = eff.unsigned_abs();
        mul_div_floor_u128(abs_eff, oracle_price as u128, POS_SCALE)
    }

    /// is_above_maintenance_margin (spec §9.1): Eq_net_i > MM_req_i
    /// Per spec §9.1: if eff == 0 then MM_req = 0; else MM_req = max(proportional, MIN_NONZERO_MM_REQ)
    pub fn is_above_maintenance_margin(&self, account: &Account, idx: usize, oracle_price: u64) -> bool {
        let eq_net = self.account_equity_net(account, oracle_price);
        let eff = self.effective_pos_q(idx);
        if eff == 0 {
            return eq_net > 0;
        }
        let not = self.notional(idx, oracle_price);
        let proportional = mul_div_floor_u128(not, self.params.maintenance_margin_bps as u128, 10_000);
        let mm_req = core::cmp::max(proportional, self.params.min_nonzero_mm_req);
        let mm_req_i128 = if mm_req > i128::MAX as u128 { i128::MAX } else { mm_req as i128 };
        eq_net > mm_req_i128
    }

    /// is_above_initial_margin (spec §9.1): exact Eq_init_raw_i >= IM_req_i
    /// Per spec §9.1: if eff == 0 then IM_req = 0; else IM_req = max(proportional, MIN_NONZERO_IM_REQ)
    /// Per spec §3.4: MUST use exact raw equity, not clamped Eq_init_net_i,
    /// so negative raw equity is distinguishable from zero.
    pub fn is_above_initial_margin(&self, account: &Account, idx: usize, oracle_price: u64) -> bool {
        let eq_init_raw = self.account_equity_init_raw(account, idx);
        let eff = self.effective_pos_q(idx);
        if eff == 0 {
            return eq_init_raw >= 0;
        }
        let not = self.notional(idx, oracle_price);
        let proportional = mul_div_floor_u128(not, self.params.initial_margin_bps as u128, 10_000);
        let im_req = core::cmp::max(proportional, self.params.min_nonzero_im_req);
        let im_req_i128 = if im_req > i128::MAX as u128 { i128::MAX } else { im_req as i128 };
        eq_init_raw >= im_req_i128
    }

    /// Eq_trade_open_raw_i (spec §3.5, v12.14.0): counterfactual trade approval
    /// metric with the candidate trade's own positive slippage removed.
    /// `candidate_trade_pnl` is the signed execution-slippage PnL for this account
    /// from the candidate trade under evaluation.
    pub fn account_equity_trade_open_raw(
        &self, account: &Account, idx: usize, candidate_trade_pnl: i128
    ) -> i128 {
        let trade_gain = if candidate_trade_pnl > 0 { candidate_trade_pnl as u128 } else { 0u128 };

        // Trade lane uses FULL positive PnL via g (spec §3.5), not just released.
        // This allows unreleased reserved PnL to support the same account's
        // risk-increasing trades through the global haircut.
        // Only the candidate trade's own positive gain is neutralized.
        let pos_pnl = i128_clamp_pos(account.pnl);
        let pos_pnl_trade_open = pos_pnl.saturating_sub(trade_gain);

        // PNL_trade_open_i for loss component
        let pnl_trade_open = account.pnl.checked_sub(trade_gain as i128)
            .unwrap_or(i128::MIN + 1);

        // Counterfactual global positive aggregate (using pnl_pos_tot, not matured)
        // If aggregates are corrupt, return most restrictive equity (blocks trades)
        let pnl_pos_tot_trade_open = match self.pnl_pos_tot.checked_sub(pos_pnl) {
            Some(v) => match v.checked_add(pos_pnl_trade_open) {
                Some(v2) => v2,
                None => return i128::MIN + 1, // corrupt: blocks all trades
            },
            None => return i128::MIN + 1, // corrupt: blocks all trades
        };

        // Counterfactual trade haircut g
        let pnl_eff_trade_open = if pnl_pos_tot_trade_open == 0 {
            pos_pnl_trade_open
        } else {
            let senior_sum = self.c_tot.get().checked_add(
                self.insurance_fund.balance.get()).unwrap_or(u128::MAX);
            let residual = if self.vault.get() >= senior_sum {
                self.vault.get() - senior_sum
            } else { 0u128 };
            let g_num = core::cmp::min(residual, pnl_pos_tot_trade_open);
            mul_div_floor_u128(pos_pnl_trade_open, g_num, pnl_pos_tot_trade_open)
        };

        // Eq_trade_open = C_i + min(PNL_trade_open, 0) + g*PosPNL_trade_open - FeeDebt
        let cap = I256::from_u128(account.capital.get());
        let neg_pnl = I256::from_i128(if pnl_trade_open < 0 { pnl_trade_open } else { 0i128 });
        let eff = I256::from_u128(pnl_eff_trade_open);
        let fee_debt = I256::from_u128(fee_debt_u128_checked(account.fee_credits.get()));

        let result = cap.checked_add(neg_pnl).expect("I256 add")
            .checked_add(eff).expect("I256 add")
            .checked_sub(fee_debt).expect("I256 sub");

        match result.try_into_i128() {
            Some(v) => v,
            None => i128::MIN + 1, // fail conservative on any overflow
        }
    }

    /// is_above_initial_margin_trade_open (spec §9.1 + §3.5):
    /// Uses Eq_trade_open_raw_i for risk-increasing trade approval.
    pub fn is_above_initial_margin_trade_open(
        &self, account: &Account, idx: usize, oracle_price: u64,
        candidate_trade_pnl: i128,
    ) -> bool {
        let eq = self.account_equity_trade_open_raw(account, idx, candidate_trade_pnl);
        let eff = self.effective_pos_q(idx);
        if eff == 0 {
            return eq >= 0;
        }
        let not = self.notional(idx, oracle_price);
        let proportional = mul_div_floor_u128(not, self.params.initial_margin_bps as u128, 10_000);
        let im_req = core::cmp::max(proportional, self.params.min_nonzero_im_req);
        let im_req_i128 = if im_req > i128::MAX as u128 { i128::MAX } else { im_req as i128 };
        eq >= im_req_i128
    }

    // ========================================================================
    // Conservation check (spec §3.1)
    // ========================================================================

    pub fn check_conservation(&self) -> bool {
        let senior = self.c_tot.get().checked_add(self.insurance_fund.balance.get());
        match senior {
            Some(s) => self.vault.get() >= s,
            None => false,
        }
    }

    /// Assert global engine postconditions (spec §3.1 + §5.2).
    ///
    /// Called as the last step of every public `_not_atomic` entrypoint.
    /// Verifies:
    ///   1. Conservation: V >= C_tot + I (no underwater vault).
    ///   2. Bilateral OI: OI_eff_long_q == OI_eff_short_q (no side imbalance).
    ///
    /// Per-instruction arithmetic should preserve both, but these are the
    /// global spec-level invariants and the spec requires them checked at
    /// the public surface.
    fn assert_public_postconditions(&self) -> Result<()> {
        if !self.check_conservation() {
            return Err(RiskError::CorruptState);
        }
        if self.oi_eff_long_q != self.oi_eff_short_q {
            return Err(RiskError::CorruptState);
        }
        Ok(())
    }

    // ========================================================================
    // Warmup Helpers (spec §6)
    // ========================================================================

    /// released_pos (spec §2.1): ReleasedPos_i = max(PNL_i, 0) - R_i
    pub fn released_pos(&self, idx: usize) -> u128 {
        let pnl = self.accounts[idx].pnl;
        let pos_pnl = i128_clamp_pos(pnl);
        // Checked: reserved_pnl > pos_pnl would be CorruptState,
        // but this is a view fn (no Result). Saturating is safe here
        // because callers validate before mutation.
        pos_pnl.saturating_sub(self.accounts[idx].reserved_pnl)
    }

    // ========================================================================
    // Two-bucket warmup reserve helpers (spec §4.3)
    // ========================================================================

    /// append_or_route_new_reserve (spec §4.3)
    test_visible! {
    fn append_or_route_new_reserve(&mut self, idx: usize, reserve_add: u128, now_slot: u64, h_lock: u64) -> Result<()> {
        // Validate existing reserve shape before mutating on top of it.
        // Malformed bucket state must fail rather than be merged through.
        self.validate_reserve_shape(idx)?;

        let a = &mut self.accounts[idx];

        // Step 1: if sched absent and pending present → promote pending to scheduled
        if a.sched_present == 0 && a.pending_present != 0 {
            a.sched_present = 1;
            a.sched_remaining_q = a.pending_remaining_q;
            a.sched_anchor_q = a.pending_remaining_q;
            a.sched_start_slot = now_slot;
            a.sched_horizon = a.pending_horizon;
            a.sched_release_q = 0;
            a.pending_present = 0;
            a.pending_remaining_q = 0;
            a.pending_horizon = 0;
            a.pending_created_slot = 0;
        }

        if a.sched_present == 0 {
            // Step 2: sched absent → create scheduled bucket
            a.sched_present = 1;
            a.sched_remaining_q = reserve_add;
            a.sched_anchor_q = reserve_add;
            a.sched_start_slot = now_slot;
            a.sched_horizon = h_lock;
            a.sched_release_q = 0;
        } else if a.sched_present != 0 && a.pending_present == 0
            && a.sched_start_slot == now_slot && a.sched_horizon == h_lock && a.sched_release_q == 0
        {
            // Step 3: merge into scheduled (same slot, same horizon, not yet released)
            a.sched_remaining_q = a.sched_remaining_q.checked_add(reserve_add).ok_or(RiskError::Overflow)?;
            a.sched_anchor_q = a.sched_anchor_q.checked_add(reserve_add).ok_or(RiskError::Overflow)?;
        } else if a.pending_present == 0 {
            // Step 4: create pending bucket
            a.pending_present = 1;
            a.pending_remaining_q = reserve_add;
            a.pending_horizon = h_lock;
            a.pending_created_slot = now_slot;
        } else {
            // Step 5: merge into pending (horizon = max)
            a.pending_remaining_q = a.pending_remaining_q.checked_add(reserve_add).ok_or(RiskError::Overflow)?;
            a.pending_horizon = core::cmp::max(a.pending_horizon, h_lock);
        }

        // Step 6: R_i += reserve_add
        a.reserved_pnl = a.reserved_pnl.checked_add(reserve_add).ok_or(RiskError::Overflow)?;
        Ok(())
    }

    }

    /// apply_reserve_loss_newest_first (spec §4.4) — consume from pending first, then scheduled.
    test_visible! {
    fn apply_reserve_loss_newest_first(&mut self, idx: usize, reserve_loss: u128) -> Result<()> {
        // Validate reserve integrity first — a malformed bucket (e.g., sums
        // not matching reserved_pnl, horizons out of bounds, reserved_pnl
        // exceeding positive PnL) must fail rather than be partially
        // consumed and transformed into a different malformed state.
        self.validate_reserve_shape(idx)?;

        // Phase 1: compute per-bucket takes WITHOUT mutating. Validates
        // feasibility (reserve_loss <= total available, reserve_loss <=
        // reserved_pnl). Previously mutated step-by-step and only checked
        // "remaining != 0" at the end, which left partial consumption on
        // Err paths.
        let a = &self.accounts[idx];
        let pend_avail = if a.pending_present != 0 { a.pending_remaining_q } else { 0 };
        let sched_avail = if a.sched_present != 0 { a.sched_remaining_q } else { 0 };
        let total_avail = pend_avail
            .checked_add(sched_avail)
            .ok_or(RiskError::CorruptState)?;
        if reserve_loss > total_avail { return Err(RiskError::CorruptState); }
        // Pre-validate R_i decrement.
        let new_reserved_pnl = a.reserved_pnl
            .checked_sub(reserve_loss)
            .ok_or(RiskError::CorruptState)?;

        // Newest-first order: pending → scheduled.
        let take_pend = core::cmp::min(reserve_loss, pend_avail);
        // Safe: take_pend <= reserve_loss.
        let take_sched = reserve_loss - take_pend;
        // Safe: take_sched = reserve_loss - take_pend <= total_avail - pend_avail = sched_avail.

        // Phase 2: commit.
        let a = &mut self.accounts[idx];
        if take_pend > 0 {
            a.pending_remaining_q -= take_pend;
            if a.pending_remaining_q == 0 {
                a.pending_present = 0;
                a.pending_horizon = 0;
                a.pending_created_slot = 0;
            }
        }
        if take_sched > 0 {
            a.sched_remaining_q -= take_sched;
            if a.sched_remaining_q == 0 {
                a.sched_present = 0;
                a.sched_anchor_q = 0;
                a.sched_start_slot = 0;
                a.sched_horizon = 0;
                a.sched_release_q = 0;
            }
        }
        a.reserved_pnl = new_reserved_pnl;
        Ok(())
    }

    }

    /// prepare_account_for_resolved_touch (spec §4.4.3)
    test_visible! {
    fn prepare_account_for_resolved_touch(&mut self, idx: usize) {
        let a = &mut self.accounts[idx];
        // Always clear bucket metadata even if reserved_pnl == 0.
        a.sched_present = 0;
        a.sched_remaining_q = 0;
        a.sched_anchor_q = 0;
        a.sched_start_slot = 0;
        a.sched_horizon = 0;
        a.sched_release_q = 0;
        a.pending_present = 0;
        a.pending_remaining_q = 0;
        a.pending_horizon = 0;
        a.pending_created_slot = 0;
        a.reserved_pnl = 0;
        // Do NOT mutate PNL_matured_pos_tot (already set globally at resolve time)
    }
    }


    /// Validate reserve-bucket shape consistency.
    /// Absent bucket => all fields zero. Present scheduled => horizon > 0,
    /// release <= anchor, remaining <= anchor - release.
    /// Total: sched_remaining + pending_remaining == reserved_pnl.
    fn validate_reserve_shape(&self, idx: usize) -> Result<()> {
        let a = &self.accounts[idx];
        if a.sched_present == 0 {
            if a.sched_remaining_q != 0 || a.sched_anchor_q != 0
                || a.sched_start_slot != 0 || a.sched_horizon != 0
                || a.sched_release_q != 0
            {
                return Err(RiskError::CorruptState);
            }
        } else {
            // Spec §4.4/§1.4: sched_horizon in [cfg_h_min, cfg_h_max] when present.
            // Matches pending_horizon validation below — previously only pending
            // was bounded-checked; malformed sched state could otherwise be
            // accelerated or merged before detection.
            if a.sched_horizon == 0 { return Err(RiskError::CorruptState); }
            if a.sched_horizon < self.params.h_min { return Err(RiskError::CorruptState); }
            if a.sched_horizon > self.params.h_max { return Err(RiskError::CorruptState); }
            if a.sched_release_q > a.sched_anchor_q { return Err(RiskError::CorruptState); }
            let used = a.sched_remaining_q.checked_add(a.sched_release_q)
                .ok_or(RiskError::CorruptState)?;
            if used > a.sched_anchor_q { return Err(RiskError::CorruptState); }
        }
        if a.sched_present != 0 && a.sched_remaining_q == 0 {
            return Err(RiskError::CorruptState);
        }
        if a.pending_present == 0 {
            if a.pending_remaining_q != 0 || a.pending_horizon != 0
                || a.pending_created_slot != 0
            {
                return Err(RiskError::CorruptState);
            }
        } else {
            // Spec §4.4/§1.4: pending_horizon in [cfg_h_min, cfg_h_max]
            if a.pending_horizon == 0 { return Err(RiskError::CorruptState); }
            if a.pending_horizon < self.params.h_min { return Err(RiskError::CorruptState); }
            if a.pending_horizon > self.params.h_max { return Err(RiskError::CorruptState); }
            if a.pending_remaining_q == 0 { return Err(RiskError::CorruptState); }
        }
        let sched_r = if a.sched_present != 0 { a.sched_remaining_q } else { 0 };
        let pend_r = if a.pending_present != 0 { a.pending_remaining_q } else { 0 };
        let total = sched_r.checked_add(pend_r).ok_or(RiskError::CorruptState)?;
        if total != a.reserved_pnl { return Err(RiskError::CorruptState); }

        // Spec §2.1: R_i <= max(PNL_i, 0). Without this, a corrupt account
        // with reserved_pnl > max(pnl, 0) would pass shape validation and
        // subsequent helpers (apply_reserve_loss, admit_outstanding) would
        // mutate on top of an invalid state.
        let pos_pnl: u128 = if a.pnl > 0 { a.pnl as u128 } else { 0 };
        if a.reserved_pnl > pos_pnl { return Err(RiskError::CorruptState); }

        Ok(())
    }

    /// advance_profit_warmup (spec §4.8, two-bucket)
    /// Releases reserve from the scheduled bucket per linear maturity.
    test_visible! {
    fn advance_profit_warmup(&mut self, idx: usize) -> Result<()> {
        // Validate reserve integrity BEFORE the pending→scheduled promotion.
        // Previously validation ran after promotion, so malformed pending
        // fields (e.g., pending_horizon == 0, or pending_remaining_q
        // mismatching reserved_pnl) would be copied into the scheduled
        // bucket before being caught.
        self.validate_reserve_shape(idx)?;

        let r = self.accounts[idx].reserved_pnl;
        if r == 0 {
            return Ok(());
        }

        // Step 2: if sched absent and pending present → promote
        if self.accounts[idx].sched_present == 0 && self.accounts[idx].pending_present != 0 {
            let a = &mut self.accounts[idx];
            a.sched_present = 1;
            a.sched_remaining_q = a.pending_remaining_q;
            a.sched_anchor_q = a.pending_remaining_q;
            a.sched_start_slot = self.current_slot;
            a.sched_horizon = a.pending_horizon;
            a.sched_release_q = 0;
            a.pending_present = 0;
            a.pending_remaining_q = 0;
            a.pending_horizon = 0;
            a.pending_created_slot = 0;
        }

        // If sched absent but R > 0 with no pending either -> corrupt
        if self.accounts[idx].sched_present == 0 {
            return Err(RiskError::CorruptState);
        }

        // Step 4: elapsed = current_slot - sched_start_slot
        if self.current_slot < self.accounts[idx].sched_start_slot {
            return Err(RiskError::CorruptState);
        }
        let elapsed = (self.current_slot - self.accounts[idx].sched_start_slot) as u128;

        // Step 5: sched_total = min(anchor, floor(anchor * elapsed / horizon))
        let a = &mut self.accounts[idx];
        if a.sched_horizon == 0 {
            return Err(RiskError::CorruptState);
        }
        let sched_total = if elapsed >= a.sched_horizon as u128 {
            a.sched_anchor_q
        } else {
            mul_div_floor_u128(a.sched_anchor_q, elapsed, a.sched_horizon as u128)
        };

        // Step 6: require sched_total >= sched_release_q
        if sched_total < a.sched_release_q { return Err(RiskError::CorruptState); }

        // Step 7: sched_increment
        let sched_increment = sched_total - a.sched_release_q;

        // Step 8: release = min(remaining, increment)
        let release = core::cmp::min(a.sched_remaining_q, sched_increment);

        // Step 9: if release > 0
        if release > 0 {
            a.sched_remaining_q = a.sched_remaining_q.checked_sub(release).ok_or(RiskError::CorruptState)?;
            a.reserved_pnl = a.reserved_pnl.checked_sub(release).ok_or(RiskError::CorruptState)?;
            self.pnl_matured_pos_tot = self.pnl_matured_pos_tot.checked_add(release)
                .ok_or(RiskError::Overflow)?;
        }

        // Step 10: sched_release_q = sched_total
        self.accounts[idx].sched_release_q = sched_total;

        // Step 11: if scheduled empty → clear, promote pending if present
        if self.accounts[idx].sched_remaining_q == 0 {
            self.accounts[idx].sched_present = 0;
            self.accounts[idx].sched_anchor_q = 0;
            self.accounts[idx].sched_start_slot = 0;
            self.accounts[idx].sched_horizon = 0;
            self.accounts[idx].sched_release_q = 0;

            // Promote pending if present
            if self.accounts[idx].pending_present != 0 {
                let a = &mut self.accounts[idx];
                a.sched_present = 1;
                a.sched_remaining_q = a.pending_remaining_q;
                a.sched_anchor_q = a.pending_remaining_q;
                a.sched_start_slot = self.current_slot;
                a.sched_horizon = a.pending_horizon;
                a.sched_release_q = 0;
                a.pending_present = 0;
                a.pending_remaining_q = 0;
                a.pending_horizon = 0;
                a.pending_created_slot = 0;
            }
        }

        // Step 12: if R_i == 0 → require both absent
        if self.accounts[idx].reserved_pnl == 0 {
            if self.accounts[idx].sched_present != 0 || self.accounts[idx].pending_present != 0 {
                return Err(RiskError::CorruptState);
            }
        }

        if self.pnl_matured_pos_tot > self.pnl_pos_tot {
            return Err(RiskError::CorruptState);
        }
        Ok(())
    }
    }

    // ========================================================================
    // Loss settlement and profit conversion (spec §7)
    // ========================================================================

    /// settle_losses (spec §7.1): settle negative PnL from principal
    fn settle_losses(&mut self, idx: usize) -> Result<()> {
        let pnl = self.accounts[idx].pnl;
        if pnl >= 0 {
            return Ok(());
        }
        if pnl == i128::MIN { return Err(RiskError::CorruptState); }
        let need = pnl.unsigned_abs();
        let cap = self.accounts[idx].capital.get();
        let pay = core::cmp::min(need, cap);
        if pay > 0 {
            self.set_capital(idx, cap - pay)?;
            let pay_i128 = pay as i128; // pay <= need = |pnl| <= i128::MAX, safe
            let new_pnl = pnl.checked_add(pay_i128)
                .ok_or(RiskError::CorruptState)?;
            if new_pnl == i128::MIN { return Err(RiskError::CorruptState); }
            self.set_pnl(idx, new_pnl)?;
        }
        Ok(())
    }

    /// resolve_flat_negative (spec §7.3): for flat accounts with negative PnL
    fn resolve_flat_negative(&mut self, idx: usize) -> Result<()> {
        let eff = self.effective_pos_q(idx);
        if eff != 0 {
            return Ok(()); // Not flat
        }
        let pnl = self.accounts[idx].pnl;
        if pnl < 0 {
            if pnl == i128::MIN { return Err(RiskError::CorruptState); }
            let loss = pnl.unsigned_abs();
            self.absorb_protocol_loss(loss);
            self.set_pnl(idx, 0i128)?;
        }
        Ok(())
    }

    /// fee_debt_sweep (spec §7.5): after any capital increase, sweep fee debt
    test_visible! {
    fn fee_debt_sweep(&mut self, idx: usize) -> Result<()> {
        let fc = self.accounts[idx].fee_credits.get();
        let debt = fee_debt_u128_checked(fc);
        if debt == 0 {
            return Ok(());
        }
        let cap = self.accounts[idx].capital.get();
        let pay = core::cmp::min(debt, cap);
        if pay > 0 {
            self.set_capital(idx, cap - pay)?;
            // pay <= debt = |fee_credits|, so fee_credits + pay <= 0: no overflow
            let pay_i128 = core::cmp::min(pay, i128::MAX as u128) as i128;
            self.accounts[idx].fee_credits = I128::new(self.accounts[idx].fee_credits.get()
                .checked_add(pay_i128).ok_or(RiskError::CorruptState)?);
            self.insurance_fund.balance = U128::new(
                self.insurance_fund.balance.get().checked_add(pay)
                    .ok_or(RiskError::Overflow)?);
        }
        // Per spec §7.5: unpaid fee debt remains as local fee_credits until
        // physical capital becomes available or manual profit conversion occurs.
        // MUST NOT consume junior PnL claims to mint senior insurance capital.
        Ok(())
    }
    }

    // ========================================================================
    // touch_account_live_local (spec §7.7, v12.14.0)
    // ========================================================================

    /// Live local touch: advance warmup, settle side effects, settle losses.
    /// Does NOT auto-convert, does NOT fee-sweep. Those happen in finalize.
    test_visible! {
    fn touch_account_live_local(&mut self, idx: usize, ctx: &mut InstructionContext) -> Result<()> {
        if self.market_mode != MarketMode::Live { return Err(RiskError::Unauthorized); }
        if idx >= MAX_ACCOUNTS || !self.is_used(idx) {
            return Err(RiskError::AccountNotFound);
        }
        if !ctx.add_touched(idx as u16) {
            return Err(RiskError::Overflow); // touched-set capacity exceeded
        }

        // Fail-conservative: validate reserve shape BEFORE any acceleration or
        // merge can act on it. Malformed sched_horizon (e.g., out of [h_min,
        // h_max]) or bucket metadata inconsistency must cause the instruction
        // to fail rather than be "healed" by downstream mutations.
        self.validate_reserve_shape(idx)?;

        // Step 4: accelerate outstanding reserve if h=1 admits (spec §4.9)
        self.admit_outstanding_reserve_on_touch(idx)?;

        // Step 5: advance cohort-based warmup
        self.advance_profit_warmup(idx)?;

        // Step 5: settle side effects with H_lock for reserve routing
        self.settle_side_effects_live(idx, ctx)?;

        // Step 6: settle losses from principal
        self.settle_losses(idx)?;

        // Step 7: resolve flat negative
        if self.effective_pos_q(idx) == 0 && self.accounts[idx].pnl < 0 {
            self.resolve_flat_negative(idx)?;
        }

        // Steps 8-9: MUST NOT auto-convert, MUST NOT fee-sweep
        Ok(())
    }

    }

    /// finalize_touched_accounts_post_live (spec §7.8, v12.14.0)
    /// Whole-only conversion + fee sweep with shared snapshot.
    test_visible! {
    fn finalize_touched_accounts_post_live(&mut self, ctx: &InstructionContext) -> Result<()> {
        // Step 1: compute shared snapshot
        let senior_sum = self.c_tot.get().checked_add(
            self.insurance_fund.balance.get()).unwrap_or(u128::MAX);
        let residual = if self.vault.get() >= senior_sum {
            self.vault.get() - senior_sum
        } else { 0u128 };
        let h_snapshot_den = self.pnl_matured_pos_tot;
        let h_snapshot_num = if h_snapshot_den == 0 { 0 } else {
            core::cmp::min(residual, h_snapshot_den)
        };
        let is_whole = h_snapshot_den > 0 && h_snapshot_num == h_snapshot_den;

        // Step 2: iterate touched accounts in ascending order
        // Sort touched_accounts (simple insertion sort, max 4 elements)
        let count = ctx.touched_count as usize;
        let mut sorted = ctx.touched_accounts;
        for i in 1..count {
            let mut j = i;
            while j > 0 && sorted[j - 1] > sorted[j] {
                sorted.swap(j - 1, j);
                j -= 1;
            }
        }

        for ti in 0..count {
            let idx = sorted[ti] as usize;

            // Whole-only flat auto-conversion
            if is_whole
                && self.accounts[idx].position_basis_q == 0
                && self.accounts[idx].pnl > 0
            {
                let released = self.released_pos(idx);
                if released > 0 {
                    self.consume_released_pnl(idx, released)?;
                    let new_cap = self.accounts[idx].capital.get()
                        .checked_add(released).ok_or(RiskError::Overflow)?;
                    self.set_capital(idx, new_cap)?;
                }
            }

            // Fee-debt sweep
            self.fee_debt_sweep(idx)?;
        }
        Ok(())
    }

    }

    // ========================================================================
    // Account Management
    // ========================================================================

    pub fn set_owner(&mut self, idx: u16, owner: [u8; 32]) -> Result<()> {
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::Unauthorized);
        }
        // Preserve the "owner is claimed iff nonzero" convention.
        // Rejecting zero here means set_owner cannot silently un-claim an
        // account and callers cannot land the slot in an ambiguous state.
        if owner == [0u8; 32] {
            return Err(RiskError::Unauthorized);
        }
        // Defense-in-depth: reject if owner is already claimed (non-zero).
        // Authorization is the wrapper layer's job, but the engine should
        // not silently overwrite an existing owner.
        if self.accounts[idx as usize].owner != [0u8; 32] {
            return Err(RiskError::Unauthorized);
        }
        self.accounts[idx as usize].owner = owner;
        Ok(())
    }

    // ========================================================================
    // deposit (spec §10.2)
    // ========================================================================

    pub fn deposit_not_atomic(&mut self, idx: u16, amount: u128, _oracle_price: u64, now_slot: u64) -> Result<()> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        // Time monotonicity (spec §10.3 step 1)
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        if now_slot < self.last_market_slot {
            return Err(RiskError::Overflow);
        }

        // Pre-validate vault capacity before any mutations (prevents ghost account)
        let v_candidate = self.vault.get().checked_add(amount).ok_or(RiskError::Overflow)?;
        if v_candidate > MAX_VAULT_TVL {
            return Err(RiskError::Overflow);
        }

        // Step 2: spec §10.2 — deposit is the canonical materialization path.
        // Missing account materializes when amount >= cfg_min_initial_deposit.
        // No engine-native opening fee (v12.18.1).
        let capital_amount = amount;
        if !self.is_used(idx as usize) {
            if amount < self.params.min_initial_deposit.get() {
                return Err(RiskError::InsufficientBalance);
            }
            self.materialize_at(idx, now_slot)?;
        }

        // Pre-validate: settle_losses can only fail on i128::MIN PNL (corruption).
        // Check before any mutation to maintain validate-then-mutate contract.
        if self.is_used(idx as usize) && self.accounts[idx as usize].pnl == i128::MIN {
            return Err(RiskError::CorruptState);
        }

        // Step 3: current_slot = now_slot
        self.current_slot = now_slot;
        self.vault = U128::new(v_candidate);

        // Step 6: set_capital(i, C_i + capital_amount)
        let new_cap = self.accounts[idx as usize].capital.get()
            .checked_add(capital_amount).ok_or(RiskError::Overflow)?;
        self.set_capital(idx as usize, new_cap)?;

        // Step 7: settle_losses_from_principal
        self.settle_losses(idx as usize)?;

        // Step 8: deposit MUST NOT invoke resolve_flat_negative (spec §7.3).
        // A pure deposit path that does not call accrue_market_to MUST NOT
        // invoke this path — surviving flat negative PNL waits for a later
        // accrued touch.

        // Step 9: if flat and PNL >= 0, sweep fee debt (spec §7.5)
        // Per spec §10.3: deposit into account with basis != 0 MUST defer.
        // Per spec §7.5: only a surviving negative PNL_i blocks the sweep.
        if self.accounts[idx as usize].position_basis_q == 0
            && self.accounts[idx as usize].pnl >= 0
        {
            self.fee_debt_sweep(idx as usize)?;
        }

        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // withdraw_not_atomic (spec §10.3)
    // ========================================================================

    pub fn withdraw_not_atomic(
        &mut self,
        idx: u16,
        amount: u128,
        oracle_price: u64,
        now_slot: u64,
        funding_rate_e9: i128,
        admit_h_min: u64,
        admit_h_max: u64,
    ) -> Result<()> {
                Self::validate_admission_pair(admit_h_min, admit_h_max, &self.params)?;

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        // No require_fresh_crank: spec §10.4 does not gate withdraw_not_atomic on keeper
        // liveness. touch_account_live_local calls accrue_market_to with the caller's
        // oracle and slot, satisfying spec §0 goal 6 (liveness without external action).

        if !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }

        let mut ctx = InstructionContext::new_with_admission(admit_h_min, admit_h_max);

        // Step 2: accrue market
        self.accrue_market_to(now_slot, oracle_price, funding_rate_e9)?;
        self.current_slot = now_slot;

        // Step 3: live local touch
        self.touch_account_live_local(idx as usize, &mut ctx)?;

        // Finalize touched (whole-only conversion + fee sweep)
        self.finalize_touched_accounts_post_live(&ctx)?;

        // Step 4: require amount <= C_i
        if self.accounts[idx as usize].capital.get() < amount {
            return Err(RiskError::InsufficientBalance);
        }

        // Step 5: universal dust guard — post-withdraw_not_atomic capital must be 0 or >= MIN_INITIAL_DEPOSIT
        let post_cap = self.accounts[idx as usize].capital.get() - amount;
        if post_cap != 0 && post_cap < self.params.min_initial_deposit.get() {
            return Err(RiskError::InsufficientBalance);
        }

        // Step 6: if position exists, require post-withdrawal margin using
        // withdrawal equity (capital minus losses minus fees — does NOT include
        // matured released PnL, preventing approval against claims that may not
        // survive other accounts' conversions).
        let eff = self.effective_pos_q(idx as usize);
        if eff != 0 {
            // Post-withdrawal equity: current withdraw equity minus withdrawal amount
            let eq_withdraw = self.account_equity_withdraw_raw(&self.accounts[idx as usize], idx as usize);
            let eq_post = eq_withdraw.saturating_sub(amount as i128);
            let notional = self.notional(idx as usize, oracle_price);
            // eff != 0 here, so always enforce min_nonzero_im_req even if
            // notional floors to 0 for microscopic positions.
            let im_req = core::cmp::max(
                mul_div_floor_u128(notional, self.params.initial_margin_bps as u128, 10_000),
                self.params.min_nonzero_im_req,
            );
            if eq_post < im_req as i128 {
                return Err(RiskError::Undercollateralized);
            }
        }

        // Step 7: commit withdrawal
        self.set_capital(idx as usize, self.accounts[idx as usize].capital.get() - amount)?;
        self.vault = U128::new(self.vault.get().checked_sub(amount).ok_or(RiskError::CorruptState)?);

        // Steps 8-9: end-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx)?;

        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // settle_account_not_atomic (spec §10.7)
    // ========================================================================

    /// Top-level settle wrapper per spec §10.7.
    /// If settlement is exposed as a standalone instruction, this wrapper MUST be used.
    pub fn settle_account_not_atomic(
        &mut self,
        idx: u16,
        oracle_price: u64,
        now_slot: u64,
        funding_rate_e9: i128,
        admit_h_min: u64,
        admit_h_max: u64,
    ) -> Result<()> {
                Self::validate_admission_pair(admit_h_min, admit_h_max, &self.params)?;

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }
        if !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }

        let mut ctx = InstructionContext::new_with_admission(admit_h_min, admit_h_max);

        // Step 2: accrue market
        self.accrue_market_to(now_slot, oracle_price, funding_rate_e9)?;
        self.current_slot = now_slot;

        // Step 3: live local touch (no auto-convert, no fee-sweep)
        self.touch_account_live_local(idx as usize, &mut ctx)?;

        // Step 4: finalize (shared snapshot, whole-only conversion, fee-sweep)
        self.finalize_touched_accounts_post_live(&ctx)?;

        // Steps 5-6: end-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx)?;

        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // execute_trade_not_atomic (spec §10.4)
    // ========================================================================

    pub fn execute_trade_not_atomic(
        &mut self,
        a: u16,
        b: u16,
        oracle_price: u64,
        now_slot: u64,
        size_q: i128,
        exec_price: u64,
        funding_rate_e9: i128,
        admit_h_min: u64,
        admit_h_max: u64,
    ) -> Result<()> {
                Self::validate_admission_pair(admit_h_min, admit_h_max, &self.params)?;

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }
        if exec_price == 0 || exec_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }
        // Spec §10.5 step 7: require 0 < size_q <= MAX_TRADE_SIZE_Q
        if size_q <= 0 {
            return Err(RiskError::Overflow);
        }
        if size_q as u128 > MAX_TRADE_SIZE_Q {
            return Err(RiskError::Overflow);
        }

        // trade_notional check (spec §10.4 step 6)
        let trade_notional_check = mul_div_floor_u128(size_q as u128, exec_price as u128, POS_SCALE);
        if trade_notional_check > MAX_ACCOUNT_NOTIONAL {
            return Err(RiskError::Overflow);
        }

        // No require_fresh_crank: spec §10.5 does not gate execute_trade_not_atomic on
        // keeper liveness. touch_account_live_local calls accrue_market_to with the
        // caller's oracle and slot, satisfying spec §0 goal 6.

        if !self.is_used(a as usize) || !self.is_used(b as usize) {
            return Err(RiskError::AccountNotFound);
        }
        if a == b {
            return Err(RiskError::Overflow);
        }

        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }

        let mut ctx = InstructionContext::new_with_admission(admit_h_min, admit_h_max);

        // Step 10: accrue market once
        self.accrue_market_to(now_slot, oracle_price, funding_rate_e9)?;
        self.current_slot = now_slot;

        // Steps 11-12: live local touch both (no auto-convert, no fee-sweep)
        self.touch_account_live_local(a as usize, &mut ctx)?;
        self.touch_account_live_local(b as usize, &mut ctx)?;

        // Step 13: capture old effective positions
        let old_eff_a = self.effective_pos_q(a as usize);
        let old_eff_b = self.effective_pos_q(b as usize);

        // Steps 14-16: capture pre-trade MM requirements and raw maintenance buffers
        // Spec §9.1: if effective_pos_q(i) == 0, MM_req_i = 0
        let mm_req_pre_a = if old_eff_a == 0 { 0u128 } else {
            let not = self.notional(a as usize, oracle_price);
            core::cmp::max(
                    mul_div_floor_u128(not, self.params.maintenance_margin_bps as u128, 10_000),
                    self.params.min_nonzero_mm_req
                )
        };
        let mm_req_pre_b = if old_eff_b == 0 { 0u128 } else {
            let not = self.notional(b as usize, oracle_price);
            core::cmp::max(
                    mul_div_floor_u128(not, self.params.maintenance_margin_bps as u128, 10_000),
                    self.params.min_nonzero_mm_req
                )
        };
        let maint_raw_wide_pre_a = self.account_equity_maint_raw_wide(&self.accounts[a as usize]);
        let maint_raw_wide_pre_b = self.account_equity_maint_raw_wide(&self.accounts[b as usize]);
        let buffer_pre_a = maint_raw_wide_pre_a.checked_sub(I256::from_u128(mm_req_pre_a)).expect("I256 sub");
        let buffer_pre_b = maint_raw_wide_pre_b.checked_sub(I256::from_u128(mm_req_pre_b)).expect("I256 sub");

        // Step 6: compute new effective positions
        let new_eff_a = old_eff_a.checked_add(size_q).ok_or(RiskError::Overflow)?;
        let neg_size_q = size_q.checked_neg().ok_or(RiskError::Overflow)?;
        let new_eff_b = old_eff_b.checked_add(neg_size_q).ok_or(RiskError::Overflow)?;

        // Validate position bounds
        if new_eff_a != 0 && new_eff_a.unsigned_abs() > MAX_POSITION_ABS_Q {
            return Err(RiskError::Overflow);
        }
        if new_eff_b != 0 && new_eff_b.unsigned_abs() > MAX_POSITION_ABS_Q {
            return Err(RiskError::Overflow);
        }

        // Validate notional bounds
        {
            let notional_a = mul_div_floor_u128(new_eff_a.unsigned_abs(), oracle_price as u128, POS_SCALE);
            if notional_a > MAX_ACCOUNT_NOTIONAL {
                return Err(RiskError::Overflow);
            }
            let notional_b = mul_div_floor_u128(new_eff_b.unsigned_abs(), oracle_price as u128, POS_SCALE);
            if notional_b > MAX_ACCOUNT_NOTIONAL {
                return Err(RiskError::Overflow);
            }
        }

        // Preflight: finalize any ResetPending sides that are fully ready,
        // so OI-increase gating doesn't block trades on reopenable sides.
        self.maybe_finalize_ready_reset_sides();

        // Step 5: compute bilateral OI once (spec §5.2.2) and use for both
        // mode gating and later writeback. Avoids redundant checked arithmetic.
        let (oi_long_after, oi_short_after) = self.bilateral_oi_after(
            &old_eff_a, &new_eff_a, &old_eff_b, &new_eff_b)?;

        // Validate OI bounds
        if oi_long_after > MAX_OI_SIDE_Q || oi_short_after > MAX_OI_SIDE_Q {
            return Err(RiskError::Overflow);
        }

        // Reject if trade would increase OI on a blocked side
        if (self.side_mode_long == SideMode::DrainOnly || self.side_mode_long == SideMode::ResetPending)
            && oi_long_after > self.oi_eff_long_q {
            return Err(RiskError::SideBlocked);
        }
        if (self.side_mode_short == SideMode::DrainOnly || self.side_mode_short == SideMode::ResetPending)
            && oi_short_after > self.oi_eff_short_q {
            return Err(RiskError::SideBlocked);
        }

        // Step 21: trade PnL alignment (spec §10.5)
        let price_diff = (oracle_price as i128) - (exec_price as i128);
        let trade_pnl_a = compute_trade_pnl(size_q, price_diff)?;
        let trade_pnl_b = trade_pnl_a.checked_neg().ok_or(RiskError::Overflow)?;

        let pnl_a = self.accounts[a as usize].pnl.checked_add(trade_pnl_a).ok_or(RiskError::Overflow)?;
        if pnl_a == i128::MIN { return Err(RiskError::Overflow); }
        self.set_pnl_with_reserve(a as usize, pnl_a, ReserveMode::UseAdmissionPair(ctx.admit_h_min_shared, ctx.admit_h_max_shared), Some(&mut ctx))?;

        let pnl_b = self.accounts[b as usize].pnl.checked_add(trade_pnl_b).ok_or(RiskError::Overflow)?;
        if pnl_b == i128::MIN { return Err(RiskError::Overflow); }
        self.set_pnl_with_reserve(b as usize, pnl_b, ReserveMode::UseAdmissionPair(ctx.admit_h_min_shared, ctx.admit_h_max_shared), Some(&mut ctx))?;

        // Step 8: attach effective positions
        self.attach_effective_position(a as usize, new_eff_a)?;
        self.attach_effective_position(b as usize, new_eff_b)?;

        // Step 9: write pre-computed OI (same values from step 5, spec §5.2.2)
        self.oi_eff_long_q = oi_long_after;
        self.oi_eff_short_q = oi_short_after;

        // Step 10: settle post-trade losses from principal for both accounts (spec §10.4 step 18)
        // Loss seniority: losses MUST be settled before explicit fees (spec §0 item 14)
        self.settle_losses(a as usize)?;
        self.settle_losses(b as usize)?;

        // Step 11: charge trading fees (spec §10.4 step 19, §8.1)
        let trade_notional = mul_div_floor_u128(size_q.unsigned_abs(), exec_price as u128, POS_SCALE);
        let fee = if trade_notional > 0 && self.params.trading_fee_bps > 0 {
            mul_div_ceil_u128(trade_notional, self.params.trading_fee_bps as u128, 10_000)
        } else {
            0
        };

        // Charge fee from both accounts (spec §10.5 step 28)
        // (cash_to_insurance, total_equity_impact) for each side
        let mut fee_cash_a = 0u128;
        let mut fee_cash_b = 0u128;
        let mut fee_impact_a = 0u128;
        let mut fee_impact_b = 0u128;
        if fee > 0 {
            if fee > MAX_PROTOCOL_FEE_ABS {
                return Err(RiskError::Overflow);
            }
            let (cash_a, impact_a, _dropped_a) = self.charge_fee_to_insurance(a as usize, fee)?;
            let (cash_b, impact_b, _dropped_b) = self.charge_fee_to_insurance(b as usize, fee)?;
            fee_cash_a = cash_a;
            fee_cash_b = cash_b;
            fee_impact_a = impact_a;
            fee_impact_b = impact_b;
        }

        // Steps 25-26: flat-close PNL guard (spec §10.5)
        if new_eff_a == 0 && self.accounts[a as usize].pnl < 0 {
            return Err(RiskError::Undercollateralized);
        }
        if new_eff_b == 0 && self.accounts[b as usize].pnl < 0 {
            return Err(RiskError::Undercollateralized);
        }

        // Step 29: post-trade margin enforcement (spec §10.5)
        // The spec says "(Eq_maint_raw_i + fee)" using the nominal fee.
        // We use fee_impact (capital_paid + collectible_debt) instead because:
        // - charge_fee_to_insurance can drop excess beyond collectible headroom
        // - Eq_maint_raw only decreased by impact, not the full nominal fee
        // - Adding back impact correctly reverses the actual state change
        // - Using nominal fee would over-compensate and admit invalid trades
        self.enforce_post_trade_margin(
            a as usize, b as usize, oracle_price,
            &old_eff_a, &new_eff_a, &old_eff_b, &new_eff_b,
            buffer_pre_a, buffer_pre_b, fee_impact_a, fee_impact_b,
            trade_pnl_a, trade_pnl_b,
        )?;

        // Finalize touched accounts (shared snapshot conversion + fee sweep)
        self.finalize_touched_accounts_post_live(&ctx)?;

        // Steps 16-17: end-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx)?;

        self.assert_public_postconditions()?;
        Ok(())
    }

    /// Charge fee per spec §8.1 — route shortfall through fee_credits instead of PNL.
    /// Returns (capital_paid_to_insurance, total_equity_impact).
    /// capital_paid is realized revenue; total includes collectible debt.
    /// Any excess beyond collectible headroom is silently dropped.
    /// Returns (fee_paid_to_insurance, fee_equity_impact, fee_dropped) per spec §4.14.
    fn charge_fee_to_insurance(&mut self, idx: usize, fee: u128) -> Result<(u128, u128, u128)> {
        if fee > MAX_PROTOCOL_FEE_ABS {
            return Err(RiskError::Overflow);
        }
        let cap = self.accounts[idx].capital.get();
        let fee_paid = core::cmp::min(fee, cap);
        if fee_paid > 0 {
            self.set_capital(idx, cap - fee_paid)?;
            self.insurance_fund.balance = U128::new(
                self.insurance_fund.balance.get().checked_add(fee_paid)
                    .ok_or(RiskError::Overflow)?);
        }
        let fee_shortfall = fee - fee_paid;
        if fee_shortfall > 0 {
            // Route collectible shortfall through fee_credits (debit).
            // Cap at collectible headroom to avoid reverting (spec §8.2.2):
            // fee_credits must stay in [-(i128::MAX), 0]; any excess is dropped.
            let current_fc = self.accounts[idx].fee_credits.get();
            // Headroom = current_fc - (-(i128::MAX)) = current_fc + i128::MAX
            let headroom = match current_fc.checked_add(i128::MAX) {
                Some(h) if h > 0 => h as u128,
                _ => 0u128, // at or beyond limit — no room
            };
            let collectible = core::cmp::min(fee_shortfall, headroom);
            if collectible > 0 {
                // Safe: collectible <= headroom <= i128::MAX, and
                // current_fc - collectible >= -(i128::MAX)
                let new_fc = current_fc - (collectible as i128);
                self.accounts[idx].fee_credits = I128::new(new_fc);
            }
            // Any excess beyond collectible headroom is silently dropped
            let equity_impact = fee_paid + collectible;
            let dropped = fee - equity_impact;
            Ok((fee_paid, equity_impact, dropped))
        } else {
            Ok((fee_paid, fee_paid, 0))
        }
    }

    /// OI component helpers for exact bilateral decomposition (spec §5.2.2)
    fn oi_long_component(pos: i128) -> u128 {
        if pos > 0 { pos as u128 } else { 0u128 }
    }

    fn oi_short_component(pos: i128) -> u128 {
        if pos < 0 { pos.unsigned_abs() } else { 0u128 }
    }

    /// Compute exact bilateral candidate side-OI after-values (spec §5.2.2).
    /// Returns (OI_long_after, OI_short_after).
    fn bilateral_oi_after(
        &self,
        old_a: &i128, new_a: &i128,
        old_b: &i128, new_b: &i128,
    ) -> Result<(u128, u128)> {
        let oi_long_after = self.oi_eff_long_q
            .checked_sub(Self::oi_long_component(*old_a)).ok_or(RiskError::CorruptState)?
            .checked_sub(Self::oi_long_component(*old_b)).ok_or(RiskError::CorruptState)?
            .checked_add(Self::oi_long_component(*new_a)).ok_or(RiskError::Overflow)?
            .checked_add(Self::oi_long_component(*new_b)).ok_or(RiskError::Overflow)?;

        let oi_short_after = self.oi_eff_short_q
            .checked_sub(Self::oi_short_component(*old_a)).ok_or(RiskError::CorruptState)?
            .checked_sub(Self::oi_short_component(*old_b)).ok_or(RiskError::CorruptState)?
            .checked_add(Self::oi_short_component(*new_a)).ok_or(RiskError::Overflow)?
            .checked_add(Self::oi_short_component(*new_b)).ok_or(RiskError::Overflow)?;

        Ok((oi_long_after, oi_short_after))
    }

    /// Enforce post-trade margin per spec §10.5 step 29.
    /// Uses strict risk-reducing buffer comparison with exact I256 Eq_maint_raw.
    fn enforce_post_trade_margin(
        &self,
        a: usize,
        b: usize,
        oracle_price: u64,
        old_eff_a: &i128,
        new_eff_a: &i128,
        old_eff_b: &i128,
        new_eff_b: &i128,
        buffer_pre_a: I256,
        buffer_pre_b: I256,
        fee_a: u128,
        fee_b: u128,
        trade_pnl_a: i128,
        trade_pnl_b: i128,
    ) -> Result<()> {
        self.enforce_one_side_margin(a, oracle_price, old_eff_a, new_eff_a, buffer_pre_a, fee_a, trade_pnl_a)?;
        self.enforce_one_side_margin(b, oracle_price, old_eff_b, new_eff_b, buffer_pre_b, fee_b, trade_pnl_b)?;
        Ok(())
    }

    fn enforce_one_side_margin(
        &self,
        idx: usize,
        oracle_price: u64,
        old_eff: &i128,
        new_eff: &i128,
        buffer_pre: I256,
        fee: u128,
        candidate_trade_pnl: i128,
    ) -> Result<()> {
        if *new_eff == 0 {
            // Spec v12.17 §9.4 step 25: fee-neutral shortfall comparison for flat closes.
            // min(Eq_maint_raw_post + fee_equity_impact, 0) >= min(Eq_maint_raw_pre, 0)
            // Uses the actual applied fee impact (fee parameter), not nominal requested fee.
            // buffer_pre = Eq_maint_raw_pre - MM_req_pre; add MM_req_pre back.
            // Use old_eff (pre-trade) to compute MM_req_pre — NOT current state (post-trade).
            let mm_req_pre_wide = if *old_eff == 0 { I256::ZERO } else {
                let abs_old = old_eff.unsigned_abs();
                let not_pre = mul_div_floor_u128(abs_old, oracle_price as u128, POS_SCALE);
                I256::from_u128(core::cmp::max(
                    mul_div_floor_u128(not_pre, self.params.maintenance_margin_bps as u128, 10_000),
                    self.params.min_nonzero_mm_req))
            };
            let eq_maint_raw_pre = buffer_pre.checked_add(mm_req_pre_wide).expect("I256 add");
            let shortfall_pre = if eq_maint_raw_pre.is_negative() { eq_maint_raw_pre } else { I256::ZERO };

            let eq_maint_raw_post = self.account_equity_maint_raw_wide(&self.accounts[idx]);
            let fee_wide = I256::from_u128(fee);
            let maint_raw_fee_neutral = eq_maint_raw_post.checked_add(fee_wide).expect("I256 add");
            let shortfall_post = if maint_raw_fee_neutral.is_negative() { maint_raw_fee_neutral } else { I256::ZERO };

            // shortfall_post >= shortfall_pre (both <= 0; "worsening" means more negative)
            if shortfall_post.checked_sub(shortfall_pre).map_or(true, |d| d.is_negative()) {
                return Err(RiskError::Undercollateralized);
            }
            return Ok(());
        }

        let abs_old: u128 = if *old_eff == 0 { 0u128 } else { old_eff.unsigned_abs() };
        let abs_new = new_eff.unsigned_abs();

        // Determine if risk-increasing (spec §9.2)
        let risk_increasing = abs_new > abs_old
            || (*old_eff > 0 && *new_eff < 0)
            || (*old_eff < 0 && *new_eff > 0)
            || *old_eff == 0;

        // Determine if strictly risk-reducing (spec §9.2)
        let strictly_reducing = *old_eff != 0
            && *new_eff != 0
            && ((*old_eff > 0 && *new_eff > 0) || (*old_eff < 0 && *new_eff < 0))
            && abs_new < abs_old;

        if risk_increasing {
            // Require Eq_trade_open_raw_i >= IM_req (spec §3.5 + §9.1)
            // Uses counterfactual equity with candidate trade's positive slippage removed
            if !self.is_above_initial_margin_trade_open(
                &self.accounts[idx], idx, oracle_price, candidate_trade_pnl) {
                return Err(RiskError::Undercollateralized);
            }
        } else if self.is_above_maintenance_margin(&self.accounts[idx], idx, oracle_price) {
            // Maintenance healthy: allow
        } else if strictly_reducing {
            // v12.14.0 §10.5 step 29: strict risk-reducing exemption (fee-neutral).
            // Both conditions must hold in exact widened I256:
            // 1. Fee-neutral buffer improves: (Eq_maint_raw_post + fee) - MM_req_post > buffer_pre
            // 2. Fee-neutral shortfall does not worsen: min(Eq_maint_raw_post + fee, 0) >= min(Eq_maint_raw_pre, 0)
            let maint_raw_wide_post = self.account_equity_maint_raw_wide(&self.accounts[idx]);
            let fee_wide = I256::from_u128(fee);

            // Fee-neutral post equity and buffer
            let maint_raw_fee_neutral = maint_raw_wide_post.checked_add(fee_wide).expect("I256 add");
            let mm_req_post = {
                let not = self.notional(idx, oracle_price);
                core::cmp::max(
                    mul_div_floor_u128(not, self.params.maintenance_margin_bps as u128, 10_000),
                    self.params.min_nonzero_mm_req
                )
            };
            let buffer_post_fee_neutral = maint_raw_fee_neutral.checked_sub(I256::from_u128(mm_req_post)).expect("I256 sub");

            // Recover pre-trade raw equity from buffer_pre + MM_req_pre
            let mm_req_pre = {
                let not_pre = if *old_eff == 0 { 0u128 } else {
                    mul_div_floor_u128(old_eff.unsigned_abs(), oracle_price as u128, POS_SCALE)
                };
                core::cmp::max(
                    mul_div_floor_u128(not_pre, self.params.maintenance_margin_bps as u128, 10_000),
                    self.params.min_nonzero_mm_req
                )
            };
            let maint_raw_pre = buffer_pre.checked_add(I256::from_u128(mm_req_pre)).expect("I256 add");

            // Condition 1: fee-neutral buffer strictly improves
            let cond1 = buffer_post_fee_neutral > buffer_pre;

            // Condition 2: fee-neutral shortfall below zero does not worsen
            // min(post + fee, 0) >= min(pre, 0)
            let zero = I256::from_i128(0);
            let shortfall_post = if maint_raw_fee_neutral < zero { maint_raw_fee_neutral } else { zero };
            let shortfall_pre = if maint_raw_pre < zero { maint_raw_pre } else { zero };
            let cond2 = shortfall_post >= shortfall_pre;

            if cond1 && cond2 {
                // Both conditions met: allow
            } else {
                return Err(RiskError::Undercollateralized);
            }
        } else {
            return Err(RiskError::Undercollateralized);
        }
        Ok(())
    }

    // ========================================================================
    // liquidate_at_oracle_not_atomic (spec §10.5 + §10.0)
    // ========================================================================

    /// Top-level liquidation: creates its own InstructionContext and finalizes resets.
    /// Accepts LiquidationPolicy per spec §10.6.
    pub fn liquidate_at_oracle_not_atomic(
        &mut self,
        idx: u16,
        now_slot: u64,
        oracle_price: u64,
        policy: LiquidationPolicy,
        funding_rate_e9: i128,
        admit_h_min: u64,
        admit_h_max: u64,
    ) -> Result<bool> {
                Self::validate_admission_pair(admit_h_min, admit_h_max, &self.params)?;

        // Spec §9.6 step 2: require account materialized (public entry point).
        if (idx as usize) >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        // Bounds and existence check BEFORE touch_account_live_local to prevent
        // market-state mutation (accrue_market_to) on missing accounts.
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Ok(false);
        }

        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }

        let mut ctx = InstructionContext::new_with_admission(admit_h_min, admit_h_max);

        // Step 2: accrue market
        self.accrue_market_to(now_slot, oracle_price, funding_rate_e9)?;
        self.current_slot = now_slot;

        // Step 3: live local touch
        self.touch_account_live_local(idx as usize, &mut ctx)?;

        // Step 4: liquidate (before finalize, so post-liquidation state gets finalized)
        let result = self.liquidate_at_oracle_internal(idx, now_slot, oracle_price, policy, &mut ctx)?;

        // Step 5: finalize AFTER liquidation — post-liquidation flat accounts
        // get whole-only conversion and fee sweep
        self.finalize_touched_accounts_post_live(&ctx)?;

        // End-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx)?;

        self.assert_public_postconditions()?;
        Ok(result)
    }

    /// Internal liquidation routine: takes caller's shared InstructionContext.
    /// Precondition (spec §9.4): caller has already called touch_account_live_local(i).
    /// Does NOT call schedule/finalize resets — caller is responsible.
    fn liquidate_at_oracle_internal(
        &mut self,
        idx: u16,
        _now_slot: u64,
        oracle_price: u64,
        policy: LiquidationPolicy,
        ctx: &mut InstructionContext,
    ) -> Result<bool> {
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Ok(false);
        }

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        // Check position exists
        let old_eff = self.effective_pos_q(idx as usize);
        if old_eff == 0 {
            return Ok(false);
        }

        // Step 4: check liquidation eligibility (spec §9.3)
        if self.is_above_maintenance_margin(&self.accounts[idx as usize], idx as usize, oracle_price) {
            return Ok(false);
        }

        let liq_side = side_of_i128(old_eff).unwrap();
        let abs_old_eff = old_eff.unsigned_abs();

        match policy {
            LiquidationPolicy::ExactPartial(q_close_q) => {
                // Spec §9.4: partial liquidation
                // Step 1-2: require 0 < q_close_q < abs(old_eff_pos_q_i)
                if q_close_q == 0 || q_close_q >= abs_old_eff {
                    return Err(RiskError::Overflow);
                }
                // Step 4: new_eff_abs_q = abs(old) - q_close_q
                let new_eff_abs_q = abs_old_eff.checked_sub(q_close_q)
                    .ok_or(RiskError::Overflow)?;
                // Step 5: require new_eff_abs_q > 0 (property 68)
                if new_eff_abs_q == 0 {
                    return Err(RiskError::Overflow);
                }
                // Step 6: new_eff_pos_q_i = sign(old) * new_eff_abs_q
                let sign = if old_eff > 0 { 1i128 } else { -1i128 };
                let new_eff = sign.checked_mul(new_eff_abs_q as i128)
                    .ok_or(RiskError::Overflow)?;

                // Step 7-8: close q_close_q at oracle, attach new position
                self.attach_effective_position(idx as usize, new_eff)?;

                // Step 9: settle realized losses from principal
                self.settle_losses(idx as usize)?;

                // Step 10-11: charge liquidation fee on quantity closed
                let liq_fee = {
                    let notional_val = mul_div_floor_u128(q_close_q, oracle_price as u128, POS_SCALE);
                    let liq_fee_raw = mul_div_ceil_u128(notional_val, self.params.liquidation_fee_bps as u128, 10_000);
                    core::cmp::min(
                        core::cmp::max(liq_fee_raw, self.params.min_liquidation_abs.get()),
                        self.params.liquidation_fee_cap.get(),
                    )
                };
                self.charge_fee_to_insurance(idx as usize, liq_fee)?;

                // Step 12: enqueue ADL with d=0 (partial, no bankruptcy)
                self.enqueue_adl(ctx, liq_side, q_close_q, 0)?;

                // Step 13: check if pending reset was scheduled
                // (If so, skip further live-OI-dependent work, but step 14 still runs)

                // Step 14: MANDATORY post-partial local maintenance health check
                // This MUST run even when step 13 has scheduled a pending reset (spec §9.4).
                if !self.is_above_maintenance_margin(&self.accounts[idx as usize], idx as usize, oracle_price) {
                    return Err(RiskError::Undercollateralized);
                }

                Ok(true)
            }
            LiquidationPolicy::FullClose => {
                // Spec §9.5: full-close liquidation (existing behavior)
                let q_close_q = abs_old_eff;

                // Close entire position at oracle
                self.attach_effective_position(idx as usize, 0i128)?;

                // Settle losses from principal
                self.settle_losses(idx as usize)?;

                // Charge liquidation fee (spec §8.3)
                let liq_fee = if q_close_q == 0 {
                    0u128
                } else {
                    let notional_val = mul_div_floor_u128(q_close_q, oracle_price as u128, POS_SCALE);
                    let liq_fee_raw = mul_div_ceil_u128(notional_val, self.params.liquidation_fee_bps as u128, 10_000);
                    core::cmp::min(
                        core::cmp::max(liq_fee_raw, self.params.min_liquidation_abs.get()),
                        self.params.liquidation_fee_cap.get(),
                    )
                };
                self.charge_fee_to_insurance(idx as usize, liq_fee)?;

                // Determine deficit D
                let eff_post = self.effective_pos_q(idx as usize);
                let d: u128 = if eff_post == 0 && self.accounts[idx as usize].pnl < 0 {
                    if self.accounts[idx as usize].pnl == i128::MIN { return Err(RiskError::CorruptState); }
                    self.accounts[idx as usize].pnl.unsigned_abs()
                } else {
                    0u128
                };

                // Enqueue ADL
                if q_close_q != 0 || d != 0 {
                    self.enqueue_adl(ctx, liq_side, q_close_q, d)?;
                }

                // If D > 0, set_pnl(i, 0)
                if d != 0 {
                    // Spec §8.5 step 8: NoPositiveIncreaseAllowed for defense-in-depth
                    self.set_pnl_with_reserve(idx as usize, 0i128,
                        ReserveMode::NoPositiveIncreaseAllowed, None)?;
                }

                Ok(true)
            }
        }
    }

    // ========================================================================
    // keeper_crank_not_atomic (spec §10.6)
    // ========================================================================

    /// keeper_crank_not_atomic (spec §10.8): Minimal on-chain permissionless shortlist processor.
    /// Candidate discovery is performed off-chain. ordered_candidates[] is untrusted.
    /// Each candidate is (account_idx, optional liquidation policy hint).
    pub fn keeper_crank_not_atomic(
        &mut self,
        now_slot: u64,
        oracle_price: u64,
        ordered_candidates: &[(u16, Option<LiquidationPolicy>)],
        max_revalidations: u16,
        funding_rate_e9: i128,
        admit_h_min: u64,
        admit_h_max: u64,
    ) -> Result<CrankOutcome> {
                Self::validate_admission_pair(admit_h_min, admit_h_max, &self.params)?;

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }

        // Reject requests exceeding MAX_TOUCHED_PER_INSTRUCTION instead of
        // silently truncating. finalize_touched_accounts_post_live cannot
        // process more than MAX_TOUCHED_PER_INSTRUCTION touched accounts, so
        // any caller requesting more is asking for work we cannot do — fail
        // explicitly rather than accept a reduced budget.
        if max_revalidations > MAX_TOUCHED_PER_INSTRUCTION as u16 {
            return Err(RiskError::Overflow);
        }

        // Step 1: initialize instruction context
        let mut ctx = InstructionContext::new_with_admission(admit_h_min, admit_h_max);

        // Steps 2-4: validate inputs
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        if now_slot < self.last_market_slot {
            return Err(RiskError::Overflow);
        }

        // Step 5: accrue_market_to exactly once
        self.accrue_market_to(now_slot, oracle_price, funding_rate_e9)?;

        // Step 6: current_slot = now_slot
        self.current_slot = now_slot;

        let advanced = now_slot > self.last_crank_slot;
        if advanced {
            self.last_crank_slot = now_slot;
        }

        // Step 7-8: process candidates in keeper-supplied order
        let mut attempts: u16 = 0;
        let mut num_liquidations: u32 = 0;

        for &(candidate_idx, ref hint) in ordered_candidates {
            // Budget check
            if attempts >= max_revalidations {
                break;
            }
            // Stop on pending reset
            if ctx.pending_reset_long || ctx.pending_reset_short {
                break;
            }
            // Skip missing accounts (doesn't count against budget)
            if (candidate_idx as usize) >= MAX_ACCOUNTS || !self.is_used(candidate_idx as usize) {
                continue;
            }

            // Count as an attempt
            attempts += 1;
            let cidx = candidate_idx as usize;

            // Touch candidate (adds to ctx.touched_accounts, up to 64 slots).
            self.touch_account_live_local(cidx, &mut ctx)?;
            // Fee sweep deferred to finalize_touched_accounts_post_live (spec §10 rule 7).

            // Check if liquidatable after exact current-state touch.
            // Apply hint if present and current-state-valid (spec §11.1 rule 3).
            if !ctx.pending_reset_long && !ctx.pending_reset_short {
                let eff = self.effective_pos_q(cidx);
                if eff != 0 {
                    if !self.is_above_maintenance_margin(&self.accounts[cidx], cidx, oracle_price) {
                        // Validate hint via stateless pre-flight (spec §11.1 rule 3).
                        // None hint → no action per spec §11.2.
                        // Invalid ExactPartial → None (no action) per spec §11.1 rule 3.
                        if let Some(policy) = self.validate_keeper_hint(candidate_idx, eff, hint, oracle_price) {
                            match self.liquidate_at_oracle_internal(candidate_idx, now_slot, oracle_price, policy, &mut ctx) {
                                Ok(true) => { num_liquidations += 1; }
                                Ok(false) => {}
                                Err(e) => return Err(e),
                            }
                        }
                    }
                }
            }
        }

        // Finalize: compute fresh snapshot from post-mutation state, apply
        // whole-only conversion + fee sweep to all tracked accounts.
        // MAX_TOUCHED_PER_INSTRUCTION = 64 matches LIQ_BUDGET_PER_CRANK.
        self.finalize_touched_accounts_post_live(&ctx)?;

        // Note: dust GC is NOT part of keeper_crank per spec §9.7.
        // Deployments should run reclaim_empty_account_not_atomic explicitly.
        let gc_closed = 0u32;

        // Steps 9-10: end-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx)?;

        self.assert_public_postconditions()?;
        Ok(CrankOutcome {
            advanced,
            num_liquidations,
            num_gc_closed: gc_closed,
        })
    }

    /// Validate a keeper-supplied liquidation-policy hint (spec §11.1 rule 3).
    /// Returns None if no liquidation action should be taken (absent hint per
    /// spec §11.2), or Some(policy) if the hint is valid. ExactPartial hints
    /// are validated via a stateless pre-flight check; invalid partials
    /// return None (no liquidation action) per spec §11.1 rule 3.
    ///
    /// Pre-flight correctness: settle_losses preserves C + PNL (spec §7.1),
    /// and the synthetic close at oracle generates zero additional PnL delta,
    /// so Eq_maint_raw after partial = Eq_maint_raw_before - liq_fee.
    test_visible! {
    fn validate_keeper_hint(
        &self,
        idx: u16,
        eff: i128,
        hint: &Option<LiquidationPolicy>,
        oracle_price: u64,
    ) -> Option<LiquidationPolicy> {
        match hint {
            // Spec §11.2: absent hint means no liquidation action for this candidate.
            None => None,
            Some(LiquidationPolicy::FullClose) => Some(LiquidationPolicy::FullClose),
            Some(LiquidationPolicy::ExactPartial(q_close_q)) => {
                let abs_eff = eff.unsigned_abs();
                // Bounds check: 0 < q_close_q < abs(eff)
                // Spec §11.1 rule 3: invalid hint → no liquidation action (None)
                if *q_close_q == 0 || *q_close_q >= abs_eff {
                    return None;
                }

                // Stateless pre-flight: predict post-partial maintenance health.
                let account = &self.accounts[idx as usize];

                // 1. Predict liquidation fee
                let notional_closed = mul_div_floor_u128(*q_close_q, oracle_price as u128, POS_SCALE);
                let liq_fee_raw = mul_div_ceil_u128(notional_closed, self.params.liquidation_fee_bps as u128, 10_000);
                let liq_fee = core::cmp::min(
                    core::cmp::max(liq_fee_raw, self.params.min_liquidation_abs.get()),
                    self.params.liquidation_fee_cap.get(),
                );

                // 2. Predict post-partial Eq_maint_raw (settle_losses preserves C + PNL sum).
                // Model the same capped fee application as charge_fee_to_insurance:
                // only capital + collectible fee-debt headroom is actually applied.
                let cap = account.capital.get();
                let fee_from_capital = core::cmp::min(liq_fee, cap);
                let fee_shortfall = liq_fee - fee_from_capital;
                let current_fc = account.fee_credits.get();
                let fc_headroom = match current_fc.checked_add(i128::MAX) {
                    Some(h) if h > 0 => h as u128,
                    _ => 0u128,
                };
                let fee_from_debt = core::cmp::min(fee_shortfall, fc_headroom);
                let fee_applied = fee_from_capital + fee_from_debt;

                let eq_raw_wide = self.account_equity_maint_raw_wide(account);
                let predicted_eq = match eq_raw_wide.checked_sub(I256::from_u128(fee_applied)) {
                    Some(v) => v,
                    None => return None,
                };

                // 3. Predict post-partial MM_req
                let rem_eff = abs_eff - *q_close_q;
                let rem_notional = mul_div_floor_u128(rem_eff, oracle_price as u128, POS_SCALE);
                let proportional_mm = mul_div_floor_u128(rem_notional, self.params.maintenance_margin_bps as u128, 10_000);
                let predicted_mm_req = if rem_eff == 0 {
                    0u128
                } else {
                    core::cmp::max(proportional_mm, self.params.min_nonzero_mm_req)
                };

                // 4. Health check: predicted_eq > predicted_mm_req
                // Spec §11.1 rule 3: failed pre-flight → no liquidation action (None)
                if predicted_eq <= I256::from_u128(predicted_mm_req) {
                    return None;
                }

                Some(LiquidationPolicy::ExactPartial(*q_close_q))
            }
        }
    }
    }

    // ========================================================================
    // convert_released_pnl_not_atomic (spec §10.4.1)
    // ========================================================================

    /// Explicit voluntary conversion of matured released positive PnL for open-position accounts.
    pub fn convert_released_pnl_not_atomic(
        &mut self,
        idx: u16,
        x_req: u128,
        oracle_price: u64,
        now_slot: u64,
        funding_rate_e9: i128,
        admit_h_min: u64,
        admit_h_max: u64,
    ) -> Result<()> {
                Self::validate_admission_pair(admit_h_min, admit_h_max, &self.params)?;

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }
        if !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }

        let mut ctx = InstructionContext::new_with_admission(admit_h_min, admit_h_max);

        // Step 2: accrue market
        self.accrue_market_to(now_slot, oracle_price, funding_rate_e9)?;
        self.current_slot = now_slot;

        // Step 3: live local touch (no auto-convert, no finalize yet)
        self.touch_account_live_local(idx as usize, &mut ctx)?;

        // Step 4: check bounds BEFORE finalize (spec v12.17 §9.3.1)
        // Finalize happens AFTER explicit conversion to avoid auto-convert
        // consuming the user's released PnL before they can request it.
        let released = self.released_pos(idx as usize);
        if x_req == 0 || x_req > released {
            return Err(RiskError::Overflow);
        }

        // Step 6: compute y using pre-conversion haircut (spec §7.4).
        let (h_num, h_den) = self.haircut_ratio();
        if h_den == 0 { return Err(RiskError::CorruptState); }

        // Step 9 (spec §9.3.1): flat-account safety cap (spec §4.12)
        if self.accounts[idx as usize].position_basis_q == 0 {
            let max_safe = self.max_safe_flat_conversion_released(
                idx as usize, x_req, h_num, h_den);
            if x_req > max_safe {
                return Err(RiskError::Undercollateralized);
            }
        }

        let y: u128 = wide_mul_div_floor_u128(x_req, h_num, h_den);

        // Step 7: consume_released_pnl(i, x_req)
        self.consume_released_pnl(idx as usize, x_req)?;

        // Step 8: set_capital(i, C_i + y)
        let new_cap = self.accounts[idx as usize].capital.get()
            .checked_add(y).ok_or(RiskError::Overflow)?;
        self.set_capital(idx as usize, new_cap)?;

        // Step 9: sweep fee debt
        self.fee_debt_sweep(idx as usize)?;

        // Step 10: post-conversion health check
        let eff = self.effective_pos_q(idx as usize);
        if eff != 0 {
            // Open position: require maintenance margin
            if !self.is_above_maintenance_margin(&self.accounts[idx as usize], idx as usize, oracle_price) {
                return Err(RiskError::Undercollateralized);
            }
        } else {
            // Flat account: require non-negative raw maintenance equity.
            // Without this, a haircutted conversion + fee sweep can leave
            // the account with Eq_maint_raw < 0 (more debt than capital).
            let eq = self.account_equity_maint_raw(&self.accounts[idx as usize]);
            if eq < 0 {
                return Err(RiskError::Undercollateralized);
            }
        }

        // Step 11: finalize AFTER explicit conversion (spec v12.17 §9.3.1)
        self.finalize_touched_accounts_post_live(&ctx)?;

        // Steps 12-13: end-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx)?;

        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // close_account_not_atomic
    // ========================================================================

    pub fn close_account_not_atomic(&mut self, idx: u16, now_slot: u64, oracle_price: u64, funding_rate_e9: i128, admit_h_min: u64, admit_h_max: u64) -> Result<u128> {
                Self::validate_admission_pair(admit_h_min, admit_h_max, &self.params)?;

        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }

        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        let mut ctx = InstructionContext::new_with_admission(admit_h_min, admit_h_max);

        // Accrue market + live local touch + finalize
        self.accrue_market_to(now_slot, oracle_price, funding_rate_e9)?;
        self.current_slot = now_slot;
        self.touch_account_live_local(idx as usize, &mut ctx)?;
        self.finalize_touched_accounts_post_live(&ctx)?;

        // Position must be zero
        let eff = self.effective_pos_q(idx as usize);
        if eff != 0 {
            return Err(RiskError::Undercollateralized);
        }

        // PnL must be zero (check BEFORE fee forgiveness to avoid
        // mutating fee_credits on a path that returns Err)
        if self.accounts[idx as usize].pnl > 0 {
            return Err(RiskError::PnlNotWarmedUp);
        }
        if self.accounts[idx as usize].pnl < 0 {
            return Err(RiskError::Undercollateralized);
        }

        // Spec §9.5 step 11: require FeeDebt_i == 0 (fee_credits >= 0).
        // Voluntary close must not forgive fee debt (unlike reclaim).
        if self.accounts[idx as usize].fee_credits.get() < 0 {
            return Err(RiskError::Undercollateralized);
        }

        // Spec §9.5 step 10: require R_i == 0 and both reserve buckets absent.
        if self.accounts[idx as usize].reserved_pnl != 0
            || self.accounts[idx as usize].sched_present != 0
            || self.accounts[idx as usize].pending_present != 0
        {
            return Err(RiskError::Undercollateralized);
        }

        let capital = self.accounts[idx as usize].capital;

        if capital > self.vault {
            return Err(RiskError::InsufficientBalance);
        }
        self.vault = self.vault - capital;
        self.set_capital(idx as usize, 0)?;

        // End-of-instruction resets before freeing
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx)?;

        self.free_slot(idx)?;

        self.assert_public_postconditions()?;
        Ok(capital.get())
    }

    // ========================================================================
    // force_close_resolved_not_atomic (resolved/frozen market path)
    // ========================================================================

    /// Force-close an account on a resolved market. Uses `self.resolved_slot`
    /// as the time anchor (no slot argument).
    ///
    /// Settles K-pair PnL, zeros position, settles losses, absorbs from
    /// insurance, converts profit (bypassing warmup), sweeps fee debt,
    /// forgives remainder, returns capital, frees slot.
    ///
    /// Skips accrue_market_to (market is frozen). Handles both same-epoch
    /// and epoch-mismatch accounts.
    // ========================================================================
    // resolve_market (spec §10.7, v12.14.0)
    // ========================================================================

    /// Transition market from Live to Resolved at a price-bounded settlement price.
    /// Per spec §9.7 (v12.16.4): requires market already accrued through resolution slot
    /// (slot_last == current_slot == now_slot), eliminating retroactive funding erasure.
    /// Self-synchronizing resolve_market (spec §9.7, v12.18.0).
    /// First accrues live state, then stores terminal K deltas separately.
    pub fn resolve_market_not_atomic(
        &mut self,
        resolve_mode: ResolveMode,
        resolved_price: u64,
        live_oracle_price: u64,
        now_slot: u64,
        funding_rate_e9: i128,
    ) -> Result<()> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        // Degenerate branch also skips accrue_market_to's last_market_slot
        // monotonicity check; enforce it here so the degenerate branch cannot
        // move last_market_slot backward under corrupt state.
        if now_slot < self.last_market_slot {
            return Err(RiskError::Overflow);
        }
        if resolved_price == 0 || resolved_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }
        if live_oracle_price == 0 || live_oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        // Explicit branch selection per spec §9.8 v12.18.5 / Goal 51.
        // Value-detected branch selection is forbidden: a flat live oracle
        // must NOT automatically enter the degenerate branch.
        let used_degenerate = match resolve_mode {
            ResolveMode::Degenerate => {
                // Degenerate branch requires these trusted equalities.
                if live_oracle_price != self.last_oracle_price {
                    return Err(RiskError::Overflow);
                }
                if funding_rate_e9 != 0 {
                    return Err(RiskError::Overflow);
                }
                self.current_slot = now_slot;
                self.last_market_slot = now_slot;
                true
            }
            ResolveMode::Ordinary => {
                // Ordinary branch: accrue to now_slot using live inputs.
                // Even when `live == P_last && rate == 0`, the ordinary
                // branch stays ordinary (spec test 85).
                self.accrue_market_to(now_slot, live_oracle_price, funding_rate_e9)?;
                false
            }
        };

        // Band check runs on the ordinary branch only. The degenerate branch
        // relies entirely on trusted wrapper inputs (spec §9.8 step 9).
        if !used_degenerate {
            let p_last = self.last_oracle_price;
            let p_last_i = p_last as i128;
            let p_res = resolved_price as i128;
            let dev_bps = self.params.resolve_price_deviation_bps as i128;
            let diff_abs = (p_res - p_last_i).unsigned_abs();
            let lhs = (diff_abs as u128).checked_mul(10_000).ok_or(RiskError::Overflow)?;
            let rhs = (dev_bps as u128).checked_mul(p_last as u128).ok_or(RiskError::Overflow)?;
            if lhs > rhs {
                return Err(RiskError::Overflow);
            }
        }

        // Step 8: compute resolved terminal mark deltas in exact signed arithmetic.
        // These deltas carry the settlement shift WITHOUT adding to persistent K_side,
        // so resolution can succeed even near K headroom (spec §9.7 step 8).
        let price_diff = resolved_price as i128 - live_oracle_price as i128;
        let resolved_k_long_td = if self.side_mode_long == SideMode::ResetPending {
            0i128
        } else {
            checked_u128_mul_i128(self.adl_mult_long, price_diff)?
        };
        let resolved_k_short_td = if self.side_mode_short == SideMode::ResetPending {
            0i128
        } else {
            // Short side: negative of price_diff
            let neg_price_diff = price_diff.checked_neg().ok_or(RiskError::Overflow)?;
            checked_u128_mul_i128(self.adl_mult_short, neg_price_diff)?
        };

        // Steps 8-13: set resolved state
        self.current_slot = now_slot;
        self.market_mode = MarketMode::Resolved;
        self.resolved_price = resolved_price;
        self.resolved_live_price = live_oracle_price;
        self.resolved_slot = now_slot;
        self.resolved_k_long_terminal_delta = resolved_k_long_td;
        self.resolved_k_short_terminal_delta = resolved_k_short_td;

        // Step 13: clear resolved payout snapshot state
        self.resolved_payout_h_num = 0;
        self.resolved_payout_h_den = 0;
        self.resolved_payout_ready = 0;

        // Step 14: all positive PnL is now matured
        self.pnl_matured_pos_tot = self.pnl_pos_tot;

        // Steps 15-16: zero OI
        self.oi_eff_long_q = 0;
        self.oi_eff_short_q = 0;

        // Steps 17-20: drain/finalize sides
        if self.side_mode_long != SideMode::ResetPending {
            self.begin_full_drain_reset(Side::Long)?;
        }
        if self.side_mode_short != SideMode::ResetPending {
            self.begin_full_drain_reset(Side::Short)?;
        }
        if self.side_mode_long == SideMode::ResetPending
            && self.stale_account_count_long == 0
            && self.stored_pos_count_long == 0
        {
            self.finalize_side_reset(Side::Long)?;
        }
        if self.side_mode_short == SideMode::ResetPending
            && self.stale_account_count_short == 0
            && self.stored_pos_count_short == 0
        {
            self.finalize_side_reset(Side::Short)?;
        }

        // Step 21: resolve additionally requires both sides == 0 (stronger
        // than bilateral balance).
        if self.oi_eff_long_q != 0 || self.oi_eff_short_q != 0 {
            return Err(RiskError::CorruptState);
        }

        self.assert_public_postconditions()?;
        Ok(())
    }

    /// Combined convenience: reconcile + terminal close if ready.
    /// For pnl <= 0 accounts or terminal-ready markets, completes in one call
    /// and returns `ResolvedCloseResult::Closed(capital)`.
    /// For positive-PnL on non-terminal markets, reconciliation persists and
    /// `ResolvedCloseResult::ProgressOnly` is returned (account stays open —
    /// re-call after terminal readiness is reached).
    pub fn force_close_resolved_not_atomic(&mut self, idx: u16) -> Result<ResolvedCloseResult> {
        // Phase 1: always reconcile (persists on success)
        self.reconcile_resolved_not_atomic(idx)?;

        let i = idx as usize;

        // Finalize any sides that are fully ready for reopening
        self.maybe_finalize_ready_reset_sides();

        // pnl <= 0: can close immediately (loser/zero — no payout gate)
        // pnl > 0: needs terminal readiness for payout
        if self.accounts[i].pnl > 0 && !self.is_terminal_ready() {
            // Reconciled but not yet payable. Progress persisted.
            return Ok(ResolvedCloseResult::ProgressOnly);
        }

        // Phase 2: terminal close
        let capital = self.close_resolved_terminal_not_atomic(idx)?;
        Ok(ResolvedCloseResult::Closed(capital))
    }

    /// Phase 1: Reconcile a resolved account. Materializes K-pair PnL,
    /// zeroes position, settles losses, absorbs insurance. Always persists
    /// on success. Idempotent on already-reconciled accounts.
    pub fn reconcile_resolved_not_atomic(&mut self, idx: u16) -> Result<()> {
        if self.market_mode != MarketMode::Resolved {
            return Err(RiskError::Unauthorized);
        }
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }
        // Resolved market is frozen at self.resolved_slot. No caller input
        // for the slot anchor — the engine uses the stored boundary. This
        // removes the earlier ratchet-past-resolved_slot footgun and
        // eliminates the wrapper-integration hazard of passing wall-clock
        // slots that the engine would reject.
        self.current_slot = self.resolved_slot;
        let i = idx as usize;

        // Always clear reserve metadata (even flat accounts may have ghost bucket flags)
        self.prepare_account_for_resolved_touch(i);

        if self.accounts[i].position_basis_q != 0 {
            let basis = self.accounts[i].position_basis_q;
            let abs_basis = basis.unsigned_abs();
            let a_basis = self.accounts[i].adl_a_basis;
            if a_basis == 0 { return Err(RiskError::CorruptState); }
            let k_snap = self.accounts[i].adl_k_snap;
            let f_snap_acct = self.accounts[i].f_snap;
            let side = side_of_i128(basis).unwrap();
            let epoch_snap = self.accounts[i].adl_epoch_snap;
            let epoch_side = self.get_epoch_side(side);

            // Resolved reconciliation uses K_epoch_start + resolved_k_terminal_delta
            // as the target K (spec §5.4 steps 6-7). F uses F_epoch_start.
            // All accounts are stale after resolution (epoch mismatch).
            let resolved_k_td = match side {
                Side::Long => self.resolved_k_long_terminal_delta,
                Side::Short => self.resolved_k_short_terminal_delta,
            };
            let den = a_basis.checked_mul(POS_SCALE).ok_or(RiskError::Overflow)?;
            let pnl_delta = if epoch_snap == epoch_side {
                // Same-epoch with nonzero basis in resolved mode is corrupt state.
                // After resolution, all nonzero-basis accounts must be stale.
                return Err(RiskError::CorruptState);
            } else {
                // Stale (normal resolved path): require one-epoch lag
                if epoch_snap.checked_add(1) != Some(epoch_side) {
                    return Err(RiskError::CorruptState);
                }
                if self.get_stale_count(side) == 0 {
                    return Err(RiskError::CorruptState);
                }
                // K_epoch_start + terminal delta in wide I256.
                // The terminal K sum may exceed i128; the wide helper handles this exactly.
                let k_terminal_wide = I256::from_i128(self.get_k_epoch_start(side))
                    .checked_add(I256::from_i128(resolved_k_td))
                    .ok_or(RiskError::Overflow)?;
                let f_end_wide = I256::from_i128(self.get_f_epoch_start(side));
                Self::compute_kf_pnl_delta_wide(
                    abs_basis, k_snap, k_terminal_wide, f_snap_acct, f_end_wide, den)?
            };
            let new_pnl = self.accounts[i].pnl.checked_add(pnl_delta)
                .ok_or(RiskError::Overflow)?;
            if new_pnl == i128::MIN { return Err(RiskError::Overflow); }

            // MUTATE (prepare already called above, epoch validated above)
            if pnl_delta != 0 {
                self.set_pnl(i, new_pnl)?;
                self.pnl_matured_pos_tot = self.pnl_pos_tot;
            }
            if epoch_snap != epoch_side {
                let old_stale = self.get_stale_count(side);
                self.set_stale_count(side, old_stale.checked_sub(1).ok_or(RiskError::CorruptState)?);
            }
            self.set_position_basis_q(i, 0)?;
            self.accounts[i].adl_a_basis = ADL_ONE;
            self.accounts[i].adl_k_snap = 0;
            self.accounts[i].f_snap = 0;
            self.accounts[i].adl_epoch_snap = 0;
        }

        self.settle_losses(i)?;
        self.resolve_flat_negative(i)?;

        self.assert_public_postconditions()?;
        Ok(())
    }

    /// Check if resolved market is terminal-ready for payouts.
    /// v12.16.4: uses O(1) neg_pnl_account_count instead of O(n) scan.
    pub fn is_terminal_ready(&self) -> bool {
        if self.resolved_payout_ready != 0 { return true; }
        // All positions zeroed
        if self.stored_pos_count_long != 0 || self.stored_pos_count_short != 0 {
            return false;
        }
        // All stale accounts reconciled
        if self.stale_account_count_long != 0 || self.stale_account_count_short != 0 {
            return false;
        }
        // No negative PnL accounts remaining (spec §4.7, v12.16.4)
        self.neg_pnl_account_count == 0
    }

    /// Phase 2: Terminal close. Requires terminal readiness.
    pub fn close_resolved_terminal_not_atomic(&mut self, idx: u16) -> Result<u128> {
        if self.market_mode != MarketMode::Resolved {
            return Err(RiskError::Unauthorized);
        }
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }
        let i = idx as usize;
        // Reject unreconciled accounts: position must be zeroed, PnL >= 0
        if self.accounts[i].position_basis_q != 0 {
            return Err(RiskError::Undercollateralized);
        }
        if self.accounts[i].pnl < 0 {
            // Negative PnL means losses not yet absorbed — must reconcile first
            return Err(RiskError::Undercollateralized);
        }
        // Bug 73 defense: unconditionally clear bucket metadata before free_slot
        // to defend against accounts that reached pnl == 0 post-reconcile but
        // retained stale bucket flags (shouldn't happen under normal flow, but
        // fails conservatively rather than bricking free_slot).
        self.prepare_account_for_resolved_touch(i);
        if self.accounts[i].pnl > 0 {
            if !self.is_terminal_ready() {
                return Err(RiskError::Unauthorized);
            }
            if self.resolved_payout_ready == 0 {
                self.pnl_matured_pos_tot = self.pnl_pos_tot;
                let senior = self.c_tot.get().checked_add(
                    self.insurance_fund.balance.get()).unwrap_or(u128::MAX);
                let residual = if self.vault.get() >= senior {
                    self.vault.get() - senior } else { 0u128 };
                let h_den = self.pnl_matured_pos_tot;
                let h_num = if h_den == 0 { 0 } else {
                    core::cmp::min(residual, h_den) };
                self.resolved_payout_h_num = h_num;
                self.resolved_payout_h_den = h_den;
                self.resolved_payout_ready = 1;
            }
            // prepare_account_for_resolved_touch already cleared reserve to 0;
            // assert the invariant explicitly as defense-in-depth before using
            // live-formula released_pos in Resolved mode.
            if self.accounts[i].reserved_pnl != 0 {
                return Err(RiskError::CorruptState);
            }
            let released = self.released_pos(i); // == pnl here since reserved == 0
            if released > 0 {
                // Spec forbids h_den==0 with positive released PnL when snapshot is ready.
                if self.resolved_payout_h_den == 0 {
                    return Err(RiskError::CorruptState);
                }
                let y = wide_mul_div_floor_u128(released,
                    self.resolved_payout_h_num, self.resolved_payout_h_den);
                // Canonical resolved-close path (spec): set_pnl_with_reserve to
                // zero the account's PnL with NoPositiveIncreaseAllowed, then
                // credit the haircutted payout y to capital. Unlike
                // consume_released_pnl (which is a Live-mode matured-drain
                // helper), this uses the same canonical PnL mutation primitive
                // as the rest of the engine.
                self.set_pnl_with_reserve(i, 0i128,
                    ReserveMode::NoPositiveIncreaseAllowed, None)?;
                let new_cap = self.accounts[i].capital.get()
                    .checked_add(y).ok_or(RiskError::Overflow)?;
                self.set_capital(i, new_cap)?;
            }
        }
        self.fee_debt_sweep(i)?;
        if self.accounts[i].fee_credits.get() < 0 {
            self.accounts[i].fee_credits = I128::ZERO;
        }
        let capital = self.accounts[i].capital;
        if capital > self.vault { return Err(RiskError::InsufficientBalance); }
        self.vault = self.vault - capital;
        self.set_capital(i, 0)?;
        self.free_slot(idx)?;

        self.assert_public_postconditions()?;
        Ok(capital.get())
    }

    // ========================================================================
    // Permissionless account reclamation (spec §10.7 + §2.6)
    // ========================================================================

    /// reclaim_empty_account_not_atomic(i, now_slot) — permissionless O(1) empty/dust-account recycling.
    /// Spec §10.7: MUST NOT call accrue_market_to, MUST NOT mutate side state,
    /// MUST NOT materialize any account.
    pub fn reclaim_empty_account_not_atomic(&mut self, idx: u16, now_slot: u64) -> Result<()> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }

        // Step 3: Pre-realization flat-clean preconditions (spec §10.7 / §2.6)
        let account = &self.accounts[idx as usize];
        if account.position_basis_q != 0 {
            return Err(RiskError::Undercollateralized);
        }
        if account.pnl != 0 {
            return Err(RiskError::Undercollateralized);
        }
        if account.reserved_pnl != 0 {
            return Err(RiskError::Undercollateralized);
        }
        // Require bucket metadata empty (not just reserved_pnl == 0)
        if account.sched_present != 0 || account.pending_present != 0 {
            return Err(RiskError::Undercollateralized);
        }
        if account.fee_credits.get() > 0 {
            return Err(RiskError::CorruptState);
        }

        // Step 4: anchor current_slot
        self.current_slot = now_slot;

        // No engine-native maintenance fee in v12.14.0 (spec §8).

        // Step 5: final reclaim-eligibility check (spec §2.6)
        // C_i must be 0 or dust (< MIN_INITIAL_DEPOSIT)
        if self.accounts[idx as usize].capital.get() >= self.params.min_initial_deposit.get()
            && !self.accounts[idx as usize].capital.is_zero()
        {
            return Err(RiskError::Undercollateralized);
        }

        // Step 7: reclamation effects (spec §2.6)
        // Validate-then-mutate: compute the new insurance balance with
        // checked_add BEFORE zeroing capital, so an overflow cannot leave
        // the account zeroed while the insurance add silently saturates.
        // U128's default + is saturating, which would break conservation
        // (C_tot + I invariant) if unchecked.
        let dust_cap = self.accounts[idx as usize].capital.get();
        if dust_cap > 0 {
            let new_insurance = self.insurance_fund.balance
                .checked_add(dust_cap)
                .ok_or(RiskError::Overflow)?;
            self.set_capital(idx as usize, 0)?;
            self.insurance_fund.balance = new_insurance;
        }

        // Forgive uncollectible fee debt (spec §2.6)
        if self.accounts[idx as usize].fee_credits.get() < 0 {
            self.accounts[idx as usize].fee_credits = I128::new(0);
        }

        // Free the slot
        self.free_slot(idx)?;

        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // Garbage collection
    // ========================================================================

    test_visible! {
    fn garbage_collect_dust(&mut self) -> Result<u32> {
        let mut to_free: [u16; GC_CLOSE_BUDGET as usize] = [0; GC_CLOSE_BUDGET as usize];
        let mut num_to_free = 0usize;

        let max_scan = (ACCOUNTS_PER_CRANK as usize).min(MAX_ACCOUNTS);
        let start = self.gc_cursor as usize;

        let mut scanned: usize = 0;
        for offset in 0..max_scan {
            if num_to_free >= GC_CLOSE_BUDGET as usize {
                break;
            }
            scanned = offset + 1;

            let idx = (start + offset) & ACCOUNT_IDX_MASK;
            let block = idx >> 6;
            let bit = idx & 63;
            if (self.used[block] & (1u64 << bit)) == 0 {
                continue;
            }

            // Dust predicate: check flat-clean preconditions BEFORE fee realization
            // (matching reclaim_empty_account_not_atomic pattern — spec §8.2.3).
            let account = &self.accounts[idx];
            if account.position_basis_q != 0 {
                continue;
            }
            if account.pnl != 0 {
                continue;
            }
            if account.reserved_pnl != 0 {
                continue;
            }
            if account.sched_present != 0 || account.pending_present != 0 {
                continue;
            }
            if account.fee_credits.get() > 0 {
                continue;
            }

            // Check capital for dust eligibility
            if self.accounts[idx].capital.get() >= self.params.min_initial_deposit.get()
                && !self.accounts[idx].capital.is_zero() {
                continue;
            }

            // Sweep dust capital into insurance (spec §2.6)
            let dust_cap = self.accounts[idx].capital.get();
            if dust_cap > 0 {
                self.set_capital(idx, 0)?;
                self.insurance_fund.balance = U128::new(
                    self.insurance_fund.balance.get().checked_add(dust_cap)
                        .ok_or(RiskError::Overflow)?);
            }

            // Forgive uncollectible fee debt (spec §2.6)
            if self.accounts[idx].fee_credits.get() < 0 {
                self.accounts[idx].fee_credits = I128::new(0);
            }

            to_free[num_to_free] = idx as u16;
            num_to_free += 1;
        }

        // Advance cursor by actual number of offsets scanned, not max_scan.
        // Prevents skipping unscanned accounts on early break.
        self.gc_cursor = ((start + scanned) & ACCOUNT_IDX_MASK) as u16;

        for i in 0..num_to_free {
            self.free_slot(to_free[i])?;
        }

        Ok(num_to_free as u32)
    }
    }


    // ========================================================================
    // Insurance fund operations
    // ========================================================================

    pub fn top_up_insurance_fund(&mut self, amount: u128, now_slot: u64) -> Result<bool> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        // Spec §10.3.2: time monotonicity
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        // Validate-then-mutate: all checks before any state change
        let new_vault = self.vault.get().checked_add(amount)
            .ok_or(RiskError::Overflow)?;
        if new_vault > MAX_VAULT_TVL {
            return Err(RiskError::Overflow);
        }
        let new_ins = self.insurance_fund.balance.get().checked_add(amount)
            .ok_or(RiskError::Overflow)?;
        // All checks passed — commit
        self.current_slot = now_slot;
        self.vault = U128::new(new_vault);
        self.insurance_fund.balance = U128::new(new_ins);
        Ok(self.insurance_fund.balance.get() > self.params.insurance_floor.get())
    }

    // set_insurance_floor removed — configuration immutability (spec §2.2.1).
    // Insurance floor is fixed at initialization and cannot be changed at runtime.

    // ========================================================================
    // Account fees (wrapper-owned)
    // ========================================================================

    /// charge_account_fee_not_atomic: public pure one-shot fee instruction.
    ///
    /// USE FOR: ad-hoc wrapper-owned charges (e.g., manual adjustments,
    /// one-time penalties). The engine does NOT track which interval this
    /// represents.
    ///
    /// DO NOT USE FOR recurring time-based fees. The canonical recurring
    /// path is `sync_account_fee_to_slot_not_atomic` which reads and
    /// advances `last_fee_slot` atomically. Mixing these two APIs for the
    /// same economic interval will double-charge — this method leaves
    /// `last_fee_slot` unchanged, so a subsequent sync call will re-charge
    /// the same dt.
    ///
    /// Only mutates: C_i, fee_credits_i, I, C_tot, current_slot.
    /// Never calls accrue_market_to or touches PNL, reserves, A/K, OI,
    /// side modes, stale counters, or dust bounds.
    ///
    /// Fee beyond collectible headroom is dropped (not socialized).
    pub fn charge_account_fee_not_atomic(
        &mut self,
        idx: u16,
        fee_abs: u128,
        now_slot: u64,
    ) -> Result<()> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        if fee_abs > MAX_PROTOCOL_FEE_ABS {
            return Err(RiskError::Overflow);
        }

        self.current_slot = now_slot;

        if fee_abs > 0 {
            self.charge_fee_to_insurance(idx as usize, fee_abs)?;
        }

        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // Fee credits
    // ========================================================================
    // settle_flat_negative_pnl (spec §10.8)
    // ========================================================================

    /// Lightweight permissionless instruction to resolve flat accounts with
    /// negative PnL. Does NOT call accrue_market_to. Only absorbs the
    /// negative PnL through insurance and zeroes it.
    ///
    /// Preconditions: account is flat (position_basis_q == 0) and pnl < 0.
    pub fn settle_flat_negative_pnl_not_atomic(&mut self, idx: u16, now_slot: u64) -> Result<()> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        let i = idx as usize;
        // Flat only, reserve state empty
        if self.accounts[i].position_basis_q != 0 {
            return Err(RiskError::Undercollateralized);
        }
        if self.accounts[i].reserved_pnl != 0
            || self.accounts[i].sched_present != 0
            || self.accounts[i].pending_present != 0 {
            return Err(RiskError::Undercollateralized);
        }
        // Noop if PnL >= 0 (per spec §9.2.4)
        if self.accounts[i].pnl >= 0 {
            return Ok(());
        }

        self.current_slot = now_slot;
        // Settle losses from principal first, then absorb remaining via insurance
        self.settle_losses(i)?;
        self.resolve_flat_negative(i)?;

        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // sync_account_fee_to_slot_not_atomic (spec §4.6.1, v12.18.4)
    // ========================================================================

    /// Public entrypoint for wrapper-owned recurring-fee realization.
    ///
    /// Wrappers that enable recurring maintenance fees MUST call this before
    /// any health-sensitive engine operation on the same Solana transaction
    /// (spec §9.0 step 5). Solana transaction atomicity guarantees the sync
    /// and the subsequent operation commit together or roll back together.
    ///
    /// The public entrypoint does NOT accept an arbitrary `fee_slot_anchor`.
    /// Reviewer v12.18.5 gap: allowing a stale caller-supplied anchor let a
    /// wrapper advance `current_slot` without booking recurring fees,
    /// leaving subsequent health-sensitive ops to run against stale fee
    /// debt. The engine now picks the anchor deterministically:
    ///
    /// - On Live:     `fee_slot_anchor = current_slot` (after advancing
    ///   `current_slot` to `now_slot`).
    /// - On Resolved: `fee_slot_anchor = resolved_slot` (Goal 49 — no
    ///   post-resolution fee accrual).
    ///
    /// Charges exactly once over `[last_fee_slot, fee_slot_anchor]`. A
    /// second call with `now_slot == current_slot` is a no-op. Newly
    /// materialized accounts start at their materialization slot and are
    /// never back-charged (Goal 47).
    ///
    /// The internal `sync_account_fee_to_slot` helper (which accepts an
    /// explicit anchor) remains available for tests and Kani proofs but
    /// is not part of the public engine surface.
    pub fn sync_account_fee_to_slot_not_atomic(
        &mut self,
        idx: u16,
        now_slot: u64,
        fee_rate_per_slot: u128,
    ) -> Result<()> {
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }
        if now_slot < self.current_slot { return Err(RiskError::Overflow); }
        let anchor = match self.market_mode {
            MarketMode::Live => {
                self.current_slot = now_slot;
                self.current_slot
            }
            MarketMode::Resolved => {
                // Respect Goal 49: anchor MUST NOT exceed resolved_slot.
                // The caller-supplied `now_slot` is validated for
                // monotonicity above but never used as the anchor on
                // Resolved.
                self.resolved_slot
            }
        };
        self.sync_account_fee_to_slot(idx as usize, anchor, fee_rate_per_slot)?;
        self.assert_public_postconditions()?;
        Ok(())
    }

    // ========================================================================
    // Public getters for wrapper use
    // ========================================================================

    /// Whether the market is in Resolved mode.
    pub fn is_resolved(&self) -> bool {
        self.market_mode == MarketMode::Resolved
    }

    /// Resolved market context (price, slot). Only meaningful when is_resolved().
    pub fn resolved_context(&self) -> (u64, u64) {
        (self.resolved_price, self.resolved_slot)
    }

    // ========================================================================
    // Fee credits
    // ========================================================================

    pub fn deposit_fee_credits(&mut self, idx: u16, amount: u128, now_slot: u64) -> Result<()> {
        if self.market_mode != MarketMode::Live {
            return Err(RiskError::Unauthorized);
        }
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }
        if now_slot < self.current_slot {
            return Err(RiskError::Overflow);
        }
        // Spec §2.1: fee_credits <= 0. The caller externally moves `amount`
        // tokens; the engine must book exactly that. Previously the method
        // silently capped at outstanding debt, which made the real-token ↔
        // engine.vault correspondence divergent (amount moved > debt booked).
        // Reject anything other than exact-or-smaller-than-debt payment.
        let debt = fee_debt_u128_checked(self.accounts[idx as usize].fee_credits.get());
        if amount > debt {
            return Err(RiskError::Overflow);
        }
        if amount == 0 {
            // Even zero: no debt, no mutation except current_slot.
            self.current_slot = now_slot;
            return Ok(());
        }
        if amount > i128::MAX as u128 {
            return Err(RiskError::Overflow);
        }
        let new_vault = self.vault.get().checked_add(amount)
            .ok_or(RiskError::Overflow)?;
        if new_vault > MAX_VAULT_TVL {
            return Err(RiskError::Overflow);
        }
        let new_ins = self.insurance_fund.balance.get().checked_add(amount)
            .ok_or(RiskError::Overflow)?;
        let new_credits = self.accounts[idx as usize].fee_credits
            .checked_add(amount as i128)
            .ok_or(RiskError::Overflow)?;
        // All checks passed — commit state.
        self.current_slot = now_slot;
        self.vault = U128::new(new_vault);
        self.insurance_fund.balance = U128::new(new_ins);
        self.accounts[idx as usize].fee_credits = new_credits;
        Ok(())
    }

    // ========================================================================
    // Recompute aggregates (test helper)
    // ========================================================================

    // ========================================================================
    // Utilities
    // ========================================================================

    test_visible! {
    fn advance_slot(&mut self, slots: u64) {
        self.current_slot = self.current_slot.saturating_add(slots);
    }
    }

    /// Count used accounts
    test_visible! {
    fn count_used(&self) -> u64 {
        let mut count = 0u64;
        self.for_each_used(|_, _| {
            count += 1;
        });
        count
    }
    }
}

// ============================================================================
// Free-standing helpers
// ============================================================================

/// Set pending reset on a side in the instruction context
fn set_pending_reset(ctx: &mut InstructionContext, side: Side) {
    match side {
        Side::Long => ctx.pending_reset_long = true,
        Side::Short => ctx.pending_reset_short = true,
    }
}

/// Multiply a u128 by an i128 returning i128 (checked).
/// Computes u128 * i128 → i128. Used for A_side * delta_p in accrue_market_to.
pub fn checked_u128_mul_i128(a: u128, b: i128) -> Result<i128> {
    if a == 0 || b == 0 {
        return Ok(0i128);
    }
    let negative = b < 0;
    let abs_b = if b == i128::MIN {
        return Err(RiskError::Overflow);
    } else {
        b.unsigned_abs()
    };
    // a * abs_b may overflow u128, use wide arithmetic
    let product = U256::from_u128(a).checked_mul(U256::from_u128(abs_b))
        .ok_or(RiskError::Overflow)?;
    // Bound to i128::MAX magnitude for both signs. Excludes i128::MIN (which is
    // forbidden throughout the engine) and avoids -(i128::MIN) negate panic.
    match product.try_into_u128() {
        Some(v) if v <= i128::MAX as u128 => {
            if negative {
                Ok(-(v as i128))
            } else {
                Ok(v as i128)
            }
        }
        _ => Err(RiskError::Overflow),
    }
}

/// Compute trade PnL: floor_div_signed_conservative(size_q * price_diff, POS_SCALE)
/// Uses native i128 arithmetic (spec §1.5.1 shows trade slippage fits in i128).
pub fn compute_trade_pnl(size_q: i128, price_diff: i128) -> Result<i128> {
    if size_q == 0 || price_diff == 0 {
        return Ok(0i128);
    }

    // Determine sign of result
    let neg_size = size_q < 0;
    let neg_price = price_diff < 0;
    let result_negative = neg_size != neg_price;

    let abs_size = size_q.unsigned_abs();
    let abs_price = price_diff.unsigned_abs();

    // Use wide_signed_mul_div_floor_from_k_pair style computation
    // abs_size * abs_price / POS_SCALE with signed floor rounding
    let abs_size_u256 = U256::from_u128(abs_size);
    let abs_price_u256 = U256::from_u128(abs_price);
    let ps_u256 = U256::from_u128(POS_SCALE);

    // div_rem using mul_div_floor_u256_with_rem (internally computes wide product)
    let (q, r) = mul_div_floor_u256_with_rem(abs_size_u256, abs_price_u256, ps_u256);

    if result_negative {
        // mag = q + 1 if r != 0, else q (floor toward -inf)
        let mag = if !r.is_zero() {
            q.checked_add(U256::ONE).ok_or(RiskError::Overflow)?
        } else {
            q
        };
        // Bound to i128::MAX magnitude to avoid -(i128::MIN) negate panic.
        // i128::MIN is forbidden throughout the engine.
        match mag.try_into_u128() {
            Some(v) if v <= i128::MAX as u128 => {
                Ok(-(v as i128))
            }
            _ => Err(RiskError::Overflow),
        }
    } else {
        match q.try_into_u128() {
            Some(v) if v <= i128::MAX as u128 => Ok(v as i128),
            _ => Err(RiskError::Overflow),
        }
    }
}


