//! Formally Verified Risk Engine for Perpetual DEX — v10.5
//!
//! Implements the v10.5 spec: Combined Single-Document Revision.
//!
//! This module implements a formally verified risk engine that guarantees:
//! 1. Protected principal for flat accounts
//! 2. PNL warmup prevents instant withdrawal of manipulated profits
//! 3. ADL via lazy A/K side indices on the opposing OI side
//! 4. Conservation of funds across all operations (V >= C_tot + I)
//! 5. No hidden protocol MM — bankruptcy socialization through explicit A/K state only

#![no_std]
#![forbid(unsafe_code)]

#[cfg(kani)]
extern crate kani;

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

pub const GC_CLOSE_BUDGET: u32 = 32;
pub const ACCOUNTS_PER_CRANK: u16 = 256;
pub const LIQ_BUDGET_PER_CRANK: u16 = 120;

/// POS_SCALE = 2^64 (spec §1.2)
pub const POS_SCALE: u128 = 1u128 << 64;

/// ADL_ONE = 2^96 (spec §1.3)
pub const ADL_ONE: u128 = 1u128 << 96;

/// MIN_A_SIDE = 2^64 (spec §1.4)
pub const MIN_A_SIDE: u128 = 1u128 << 64;

/// MAX_ORACLE_PRICE = 2^56 - 1 (spec §1.4)
pub const MAX_ORACLE_PRICE: u64 = (1u64 << 56) - 1;

/// MAX_FUNDING_DT = 2^16 - 1 = 65535 (spec §1.4)
pub const MAX_FUNDING_DT: u64 = u16::MAX as u64;

/// MAX_ABS_FUNDING_BPS_PER_SLOT = 10000 (spec §1.4)
pub const MAX_ABS_FUNDING_BPS_PER_SLOT: i64 = 10_000;

// ============================================================================
// BPF-Safe 128-bit Types
// ============================================================================
pub mod i128;
pub use i128::{I128, U128};

// ============================================================================
// Wide 256-bit Arithmetic
// ============================================================================
pub mod wide_math;
use wide_math::{
    U256, I256,
    mul_div_floor_u256, mul_div_floor_u256_with_rem,
    mul_div_ceil_u256, checked_mul_div_ceil_u256,
    wide_signed_mul_div_floor,
    saturating_mul_u256_u64,
    fee_debt_u128_checked,
    ceil_div_positive_checked,
};

// ============================================================================
// Derived wide constants (computed at use site to keep const-friendly)
// ============================================================================

/// MAX_POSITION_ABS_Q = (2^40 - 1) * POS_SCALE as U256
fn max_position_abs_q() -> U256 {
    // (2^40 - 1) * 2^64 fits in u128
    let val: u128 = ((1u128 << 40) - 1).checked_mul(POS_SCALE).expect("MAX_POSITION_ABS_Q overflow");
    U256::from_u128(val)
}

/// MAX_OI_SIDE_Q = (2^40 - 1) * POS_SCALE as U256
fn max_oi_side_q() -> U256 {
    max_position_abs_q()
}

/// PHANTOM_DUST_MAX_Q = MAX_ACCOUNTS as U256
fn phantom_dust_max_q() -> U256 {
    U256::from_u128(MAX_ACCOUNTS as u128)
}

/// POS_SCALE as U256
fn pos_scale_u256() -> U256 {
    U256::from_u128(POS_SCALE)
}

/// ADL_ONE as U256
#[allow(dead_code)]
fn adl_one_u256() -> U256 {
    U256::from_u128(ADL_ONE)
}

// ============================================================================
// Core Data Structures
// ============================================================================

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountKind {
    User = 0,
    LP = 1,
}

/// Side mode for OI sides (spec §2.4)
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideMode {
    Normal = 0,
    DrainOnly = 1,
    ResetPending = 2,
}

/// Instruction context for deferred reset scheduling (spec §5.7-5.8)
pub struct InstructionContext {
    pub pending_reset_long: bool,
    pub pending_reset_short: bool,
}

impl InstructionContext {
    pub fn new() -> Self {
        Self {
            pending_reset_long: false,
            pending_reset_short: false,
        }
    }
}

/// Unified account (spec §2.1)
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Account {
    pub account_id: u64,
    pub capital: U128,
    pub kind: AccountKind,

    /// Realized PnL (i256, spec §2.1)
    pub pnl: I256,

    /// Reserved positive PnL (u256, spec §2.1)
    pub reserved_pnl: U256,

    /// Warmup start slot
    pub warmup_started_at_slot: u64,

    /// Linear warmup slope (u256, spec §2.1)
    pub warmup_slope_per_step: U256,

    /// Signed fixed-point base quantity basis (i256, spec §2.1)
    pub position_basis_q: I256,

    /// Side multiplier snapshot at last explicit position attachment (u128)
    pub adl_a_basis: u128,

    /// K coefficient snapshot (i256)
    pub adl_k_snap: I256,

    /// Side epoch snapshot
    pub adl_epoch_snap: u64,

    /// LP matching engine program ID
    pub matcher_program: [u8; 32],
    pub matcher_context: [u8; 32],

    /// Owner pubkey
    pub owner: [u8; 32],

    /// Fee credits
    pub fee_credits: I128,
    pub last_fee_slot: u64,

    /// Cumulative LP trading fees
    pub fees_earned_total: U128,
}

impl Account {
    pub fn is_lp(&self) -> bool {
        matches!(self.kind, AccountKind::LP)
    }

    pub fn is_user(&self) -> bool {
        matches!(self.kind, AccountKind::User)
    }
}

fn empty_account() -> Account {
    Account {
        account_id: 0,
        capital: U128::ZERO,
        kind: AccountKind::User,
        pnl: I256::ZERO,
        reserved_pnl: U256::ZERO,
        warmup_started_at_slot: 0,
        warmup_slope_per_step: U256::ZERO,
        position_basis_q: I256::ZERO,
        adl_a_basis: ADL_ONE,
        adl_k_snap: I256::ZERO,
        adl_epoch_snap: 0,
        matcher_program: [0; 32],
        matcher_context: [0; 32],
        owner: [0; 32],
        fee_credits: I128::ZERO,
        last_fee_slot: 0,
        fees_earned_total: U128::ZERO,
    }
}

/// Insurance fund state
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InsuranceFund {
    pub balance: U128,
    pub fee_revenue: U128,
}

/// Risk engine parameters
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskParams {
    pub warmup_period_slots: u64,
    pub maintenance_margin_bps: u64,
    pub initial_margin_bps: u64,
    pub trading_fee_bps: u64,
    pub max_accounts: u64,
    pub new_account_fee: U128,
    pub maintenance_fee_per_slot: U128,
    pub max_crank_staleness_slots: u64,
    pub liquidation_fee_bps: u64,
    pub liquidation_fee_cap: U128,
    pub liquidation_buffer_bps: u64,
    pub min_liquidation_abs: U128,
}

/// Main risk engine state (spec §2.2)
#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskEngine {
    pub vault: U128,
    pub insurance_fund: InsuranceFund,
    pub params: RiskParams,
    pub current_slot: u64,

    /// Stored funding rate for anti-retroactivity
    pub funding_rate_bps_per_slot_last: i64,

    // Keeper crank tracking
    pub last_crank_slot: u64,
    pub max_crank_staleness_slots: u64,

    // O(1) aggregates (spec §2.2)
    pub c_tot: U128,
    pub pnl_pos_tot: U256,

    // Crank cursors
    pub liq_cursor: u16,
    pub gc_cursor: u16,
    pub last_full_sweep_start_slot: u64,
    pub last_full_sweep_completed_slot: u64,
    pub crank_cursor: u16,
    pub sweep_start_idx: u16,

    // Lifetime counters
    pub lifetime_liquidations: u64,

    // ADL side state (spec §2.2)
    pub adl_mult_long: u128,
    pub adl_mult_short: u128,
    pub adl_coeff_long: I256,
    pub adl_coeff_short: I256,
    pub adl_epoch_long: u64,
    pub adl_epoch_short: u64,
    pub adl_epoch_start_k_long: I256,
    pub adl_epoch_start_k_short: I256,
    pub oi_eff_long_q: U256,
    pub oi_eff_short_q: U256,
    pub side_mode_long: SideMode,
    pub side_mode_short: SideMode,
    pub stored_pos_count_long: u64,
    pub stored_pos_count_short: u64,
    pub stale_account_count_long: u64,
    pub stale_account_count_short: u64,

    /// Dynamic phantom dust bounds (spec §4.6, §5.7)
    pub phantom_dust_bound_long_q: U256,
    pub phantom_dust_bound_short_q: U256,

    /// Last oracle price used in accrue_market_to
    pub last_oracle_price: u64,
    /// Last slot used in accrue_market_to
    pub last_market_slot: u64,
    /// Funding price sample (for anti-retroactivity)
    pub funding_price_sample_last: u64,

    /// Insurance floor (spec §4.7)
    pub insurance_floor: u128,

    // Slab management
    pub used: [u64; BITMAP_WORDS],
    pub num_used_accounts: u16,
    pub next_account_id: u64,
    pub free_head: u16,
    pub next_free: [u16; MAX_ACCOUNTS],
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
    InvalidMatchingEngine,
    PnlNotWarmedUp,
    Overflow,
    AccountNotFound,
    NotAnLPAccount,
    PositionSizeMismatch,
    AccountKindMismatch,
    SideBlocked,
    CorruptState,
}

pub type Result<T> = core::result::Result<T, RiskError>;

/// Outcome of a keeper crank operation
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrankOutcome {
    pub advanced: bool,
    pub slots_forgiven: u64,
    pub caller_settle_ok: bool,
    pub force_realize_needed: bool,
    pub panic_needed: bool,
    pub num_liquidations: u32,
    pub num_liq_errors: u16,
    pub num_gc_closed: u32,
    pub last_cursor: u16,
    pub sweep_complete: bool,
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

#[inline]
fn mul_u128(a: u128, b: u128) -> u128 {
    a.checked_mul(b).expect("mul_u128 overflow")
}

/// Determine which side a signed position is on. Positive = long, negative = short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Long,
    Short,
}

/// Try to negate a U256 magnitude to produce a negative (or zero) I256.
/// Returns Some(neg_val) if representable, None if the magnitude exceeds 2^255.
pub fn try_negate_u256_to_i256(v: U256) -> Option<I256> {
    if v.is_zero() {
        return Some(I256::ZERO);
    }
    // The maximum magnitude of a negative I256 is 2^255 (I256::MIN = -2^255).
    // v fits as positive I256 iff hi < 2^127 (sign bit clear).
    if v.hi() < (1u128 << 127) {
        // v in (0, 2^255-1]: from_raw_u256 gives positive I256, negate it
        let pos = I256::from_raw_u256(v);
        return pos.checked_neg(); // guaranteed Some for v <= I256::MAX
    }
    // v == 2^255 exactly → result is I256::MIN
    if v.lo() == 0 && v.hi() == (1u128 << 127) {
        return Some(I256::MIN);
    }
    // v > 2^255: not representable as negative I256
    None
}

fn side_of_i256(v: &I256) -> Option<Side> {
    if v.is_zero() {
        None
    } else if v.is_positive() {
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

/// Clamp i256 max(v, 0) as U256
fn i256_clamp_pos(v: &I256) -> U256 {
    if v.is_positive() {
        v.abs_u256()
    } else {
        U256::ZERO
    }
}

/// Convert u128 to i256 safely
fn u128_to_i256(v: u128) -> I256 {
    I256::from_u128(v)
}

/// Convert U256 to u128 with saturation (clamp to u128::MAX)
fn u256_to_u128_sat(v: &U256) -> u128 {
    match v.try_into_u128() {
        Some(x) => x,
        None => u128::MAX,
    }
}

/// Try to convert I256 to i128. Returns None if it doesn't fit.
#[allow(dead_code)]
fn i256_to_i128(v: &I256) -> Option<i128> {
    v.try_into_i128()
}

// ============================================================================
// Core Implementation
// ============================================================================

impl RiskEngine {
    /// Create a new risk engine
    pub fn new(params: RiskParams) -> Self {
        assert!(
            params.maintenance_margin_bps < params.initial_margin_bps,
            "maintenance_margin_bps must be strictly less than initial_margin_bps"
        );
        let mut engine = Self {
            vault: U128::ZERO,
            insurance_fund: InsuranceFund {
                balance: U128::ZERO,
                fee_revenue: U128::ZERO,
            },
            params,
            current_slot: 0,
            funding_rate_bps_per_slot_last: 0,
            last_crank_slot: 0,
            max_crank_staleness_slots: params.max_crank_staleness_slots,
            c_tot: U128::ZERO,
            pnl_pos_tot: U256::ZERO,
            liq_cursor: 0,
            gc_cursor: 0,
            last_full_sweep_start_slot: 0,
            last_full_sweep_completed_slot: 0,
            crank_cursor: 0,
            sweep_start_idx: 0,
            lifetime_liquidations: 0,
            adl_mult_long: ADL_ONE,
            adl_mult_short: ADL_ONE,
            adl_coeff_long: I256::ZERO,
            adl_coeff_short: I256::ZERO,
            adl_epoch_long: 0,
            adl_epoch_short: 0,
            adl_epoch_start_k_long: I256::ZERO,
            adl_epoch_start_k_short: I256::ZERO,
            oi_eff_long_q: U256::ZERO,
            oi_eff_short_q: U256::ZERO,
            side_mode_long: SideMode::Normal,
            side_mode_short: SideMode::Normal,
            stored_pos_count_long: 0,
            stored_pos_count_short: 0,
            stale_account_count_long: 0,
            stale_account_count_short: 0,
            phantom_dust_bound_long_q: U256::ZERO,
            phantom_dust_bound_short_q: U256::ZERO,
            last_oracle_price: 0,
            last_market_slot: 0,
            funding_price_sample_last: 0,
            insurance_floor: 0,
            used: [0; BITMAP_WORDS],
            num_used_accounts: 0,
            next_account_id: 0,
            free_head: 0,
            next_free: [0; MAX_ACCOUNTS],
            accounts: [empty_account(); MAX_ACCOUNTS],
        };

        for i in 0..MAX_ACCOUNTS - 1 {
            engine.next_free[i] = (i + 1) as u16;
        }
        engine.next_free[MAX_ACCOUNTS - 1] = u16::MAX;

        engine
    }

    /// Initialize in place (for Solana BPF zero-copy)
    pub fn init_in_place(&mut self, params: RiskParams) {
        assert!(
            params.maintenance_margin_bps < params.initial_margin_bps,
            "maintenance_margin_bps must be strictly less than initial_margin_bps"
        );
        self.params = params;
        self.max_crank_staleness_slots = params.max_crank_staleness_slots;
        self.adl_mult_long = ADL_ONE;
        self.adl_mult_short = ADL_ONE;
        for i in 0..MAX_ACCOUNTS - 1 {
            self.next_free[i] = (i + 1) as u16;
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

    #[allow(dead_code)]
    fn for_each_used_mut<F: FnMut(usize, &mut Account)>(&mut self, mut f: F) {
        for (block, word) in self.used.iter().copied().enumerate() {
            let mut w = word;
            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                let idx = block * 64 + bit;
                w &= w - 1;
                if idx >= MAX_ACCOUNTS {
                    continue;
                }
                f(idx, &mut self.accounts[idx]);
            }
        }
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

    fn alloc_slot(&mut self) -> Result<u16> {
        if self.free_head == u16::MAX {
            return Err(RiskError::Overflow);
        }
        let idx = self.free_head;
        self.free_head = self.next_free[idx as usize];
        self.set_used(idx as usize);
        self.num_used_accounts = self.num_used_accounts.saturating_add(1);
        Ok(idx)
    }

    fn free_slot(&mut self, idx: u16) {
        self.accounts[idx as usize] = empty_account();
        self.clear_used(idx as usize);
        self.next_free[idx as usize] = self.free_head;
        self.free_head = idx;
        self.num_used_accounts = self.num_used_accounts.saturating_sub(1);
    }

    // ========================================================================
    // O(1) Aggregate Helpers (spec §4)
    // ========================================================================

    /// set_pnl (spec §4.3): Update PNL and maintain pnl_pos_tot with signed-delta branching.
    /// Forbids I256::MIN. Clamps reserved_pnl.
    pub fn set_pnl(&mut self, idx: usize, new_pnl: I256) {
        // Forbid I256::MIN (spec §1.5 item 15)
        assert!(new_pnl != I256::MIN, "set_pnl: I256::MIN forbidden");

        let old = self.accounts[idx].pnl;
        let old_pos = i256_clamp_pos(&old);
        let new_pos = i256_clamp_pos(&new_pnl);

        // Signed-delta branching (spec §4.3 steps 4-5)
        if new_pos > old_pos {
            let delta = new_pos.checked_sub(old_pos).expect("set_pnl: delta sub");
            self.pnl_pos_tot = self.pnl_pos_tot.checked_add(delta)
                .expect("set_pnl: pnl_pos_tot add overflow");
        } else if old_pos > new_pos {
            let delta = old_pos.checked_sub(new_pos).expect("set_pnl: delta sub");
            self.pnl_pos_tot = self.pnl_pos_tot.checked_sub(delta)
                .expect("set_pnl: pnl_pos_tot sub underflow");
        }

        self.accounts[idx].pnl = new_pnl;

        // Clamp reserved_pnl (spec §4.3 step 7)
        if self.accounts[idx].reserved_pnl > new_pos {
            self.accounts[idx].reserved_pnl = new_pos;
        }
    }

    /// set_capital (spec §4.2): checked signed-delta update of C_tot
    pub fn set_capital(&mut self, idx: usize, new_capital: u128) {
        let old = self.accounts[idx].capital.get();
        if new_capital >= old {
            let delta = new_capital - old;
            self.c_tot = U128::new(self.c_tot.get().checked_add(delta)
                .expect("set_capital: c_tot overflow"));
        } else {
            let delta = old - new_capital;
            self.c_tot = U128::new(self.c_tot.get().checked_sub(delta)
                .expect("set_capital: c_tot underflow"));
        }
        self.accounts[idx].capital = U128::new(new_capital);
    }

    /// set_position_basis_q (spec §4.4): update stored pos counts based on sign changes
    pub fn set_position_basis_q(&mut self, idx: usize, new_basis: I256) {
        let old = self.accounts[idx].position_basis_q;
        let old_side = side_of_i256(&old);
        let new_side = side_of_i256(&new_basis);

        // Decrement old side count
        if let Some(s) = old_side {
            match s {
                Side::Long => {
                    self.stored_pos_count_long = self.stored_pos_count_long
                        .checked_sub(1).expect("set_position_basis_q: long count underflow");
                }
                Side::Short => {
                    self.stored_pos_count_short = self.stored_pos_count_short
                        .checked_sub(1).expect("set_position_basis_q: short count underflow");
                }
            }
        }

        // Increment new side count
        if let Some(s) = new_side {
            match s {
                Side::Long => {
                    self.stored_pos_count_long = self.stored_pos_count_long
                        .checked_add(1).expect("set_position_basis_q: long count overflow");
                }
                Side::Short => {
                    self.stored_pos_count_short = self.stored_pos_count_short
                        .checked_add(1).expect("set_position_basis_q: short count overflow");
                }
            }
        }

        self.accounts[idx].position_basis_q = new_basis;
    }

    /// attach_effective_position (spec §4.5)
    pub fn attach_effective_position(&mut self, idx: usize, new_eff_pos_q: I256) {
        // Before replacing a nonzero same-epoch basis, account for the fractional
        // remainder that will be orphaned (dynamic dust accounting).
        let old_basis = self.accounts[idx].position_basis_q;
        if !old_basis.is_zero() {
            if let Some(old_side) = side_of_i256(&old_basis) {
                let epoch_snap = self.accounts[idx].adl_epoch_snap;
                let epoch_side = self.get_epoch_side(old_side);
                if epoch_snap == epoch_side {
                    let a_basis = self.accounts[idx].adl_a_basis;
                    if a_basis != 0 {
                        let a_side = self.get_a_side(old_side);
                        let abs_basis = old_basis.abs_u256();
                        if let Some(product) = abs_basis.checked_mul(U256::from_u128(a_side)) {
                            if let Some(rem) = product.checked_rem(U256::from_u128(a_basis)) {
                                if !rem.is_zero() {
                                    self.inc_phantom_dust_bound(old_side);
                                }
                            }
                        }
                    }
                }
            }
        }

        if new_eff_pos_q.is_zero() {
            self.set_position_basis_q(idx, I256::ZERO);
            // Reset snapshots to canonical zero-position defaults in current epoch (spec §4.5)
            self.accounts[idx].adl_a_basis = ADL_ONE;
            if old_basis.is_positive() {
                self.accounts[idx].adl_k_snap = self.adl_coeff_long;
                self.accounts[idx].adl_epoch_snap = self.adl_epoch_long;
            } else if old_basis.is_negative() {
                self.accounts[idx].adl_k_snap = self.adl_coeff_short;
                self.accounts[idx].adl_epoch_snap = self.adl_epoch_short;
            } else {
                // Was already flat — use long side defaults
                self.accounts[idx].adl_k_snap = self.adl_coeff_long;
                self.accounts[idx].adl_epoch_snap = self.adl_epoch_long;
            }
        } else {
            let side = side_of_i256(&new_eff_pos_q).expect("attach: nonzero must have side");
            self.set_position_basis_q(idx, new_eff_pos_q);

            match side {
                Side::Long => {
                    self.accounts[idx].adl_a_basis = self.adl_mult_long;
                    self.accounts[idx].adl_k_snap = self.adl_coeff_long;
                    self.accounts[idx].adl_epoch_snap = self.adl_epoch_long;
                }
                Side::Short => {
                    self.accounts[idx].adl_a_basis = self.adl_mult_short;
                    self.accounts[idx].adl_k_snap = self.adl_coeff_short;
                    self.accounts[idx].adl_epoch_snap = self.adl_epoch_short;
                }
            }
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

    fn get_k_side(&self, s: Side) -> I256 {
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

    fn get_k_epoch_start(&self, s: Side) -> I256 {
        match s {
            Side::Long => self.adl_epoch_start_k_long,
            Side::Short => self.adl_epoch_start_k_short,
        }
    }

    fn get_side_mode(&self, s: Side) -> SideMode {
        match s {
            Side::Long => self.side_mode_long,
            Side::Short => self.side_mode_short,
        }
    }

    fn get_oi_eff(&self, s: Side) -> U256 {
        match s {
            Side::Long => self.oi_eff_long_q,
            Side::Short => self.oi_eff_short_q,
        }
    }

    fn set_oi_eff(&mut self, s: Side, v: U256) {
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

    fn set_k_side(&mut self, s: Side, v: I256) {
        match s {
            Side::Long => self.adl_coeff_long = v,
            Side::Short => self.adl_coeff_short = v,
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
    pub fn inc_phantom_dust_bound(&mut self, s: Side) {
        match s {
            Side::Long => {
                self.phantom_dust_bound_long_q = self.phantom_dust_bound_long_q
                    .checked_add(U256::from_u128(1))
                    .expect("phantom_dust_bound_long_q overflow");
            }
            Side::Short => {
                self.phantom_dust_bound_short_q = self.phantom_dust_bound_short_q
                    .checked_add(U256::from_u128(1))
                    .expect("phantom_dust_bound_short_q overflow");
            }
        }
    }

    /// Spec §4.6.1: increment phantom dust bound by amount_q (checked).
    pub fn inc_phantom_dust_bound_by(&mut self, s: Side, amount_q: U256) {
        match s {
            Side::Long => {
                self.phantom_dust_bound_long_q = self.phantom_dust_bound_long_q
                    .checked_add(amount_q)
                    .expect("phantom_dust_bound_long_q overflow");
            }
            Side::Short => {
                self.phantom_dust_bound_short_q = self.phantom_dust_bound_short_q
                    .checked_add(amount_q)
                    .expect("phantom_dust_bound_short_q overflow");
            }
        }
    }

    // ========================================================================
    // effective_pos_q (spec §5.2)
    // ========================================================================

    /// Compute effective position quantity for account idx.
    pub fn effective_pos_q(&self, idx: usize) -> I256 {
        let basis = self.accounts[idx].position_basis_q;
        if basis.is_zero() {
            return I256::ZERO;
        }

        let side = side_of_i256(&basis).unwrap();
        let epoch_snap = self.accounts[idx].adl_epoch_snap;
        let epoch_side = self.get_epoch_side(side);

        if epoch_snap != epoch_side {
            // Epoch mismatch → effective position is 0 for current-market risk
            return I256::ZERO;
        }

        let a_side = self.get_a_side(side);
        let a_basis = self.accounts[idx].adl_a_basis;

        if a_basis == 0 {
            return I256::ZERO;
        }

        let abs_basis = basis.abs_u256();
        // floor(|basis| * A_s / a_basis_i)
        let effective_abs = mul_div_floor_u256(abs_basis, U256::from_u128(a_side), U256::from_u128(a_basis));

        if basis.is_negative() {
            // Return negative
            match effective_abs.try_into_u128() {
                Some(0) => I256::ZERO,
                _ => {
                    let pos = I256::from_raw_u256_pub(effective_abs);
                    pos.checked_neg().unwrap_or(I256::ZERO)
                }
            }
        } else {
            I256::from_raw_u256_pub(effective_abs)
        }
    }

    // ========================================================================
    // settle_side_effects (spec §5.3)
    // ========================================================================

    pub fn settle_side_effects(&mut self, idx: usize) -> Result<()> {
        let basis = self.accounts[idx].position_basis_q;
        if basis.is_zero() {
            return Ok(());
        }

        let side = side_of_i256(&basis).unwrap();
        let epoch_snap = self.accounts[idx].adl_epoch_snap;
        let epoch_side = self.get_epoch_side(side);
        let a_basis = self.accounts[idx].adl_a_basis;

        if a_basis == 0 {
            return Err(RiskError::CorruptState);
        }

        let abs_basis = basis.abs_u256();

        if epoch_snap == epoch_side {
            // Same epoch (spec §5.3 step 3)
            let a_side = self.get_a_side(side);
            let k_side = self.get_k_side(side);
            let k_snap = self.accounts[idx].adl_k_snap;

            // q_eff_new = floor(|basis| * A_s / a_basis)
            let q_eff_new = mul_div_floor_u256(
                abs_basis,
                U256::from_u128(a_side),
                U256::from_u128(a_basis),
            );

            // pnl_delta = floor_div_signed_conservative(|basis| * (K_s - k_snap), a_basis * POS_SCALE)
            let k_diff = k_side.checked_sub(k_snap).ok_or(RiskError::Overflow)?;
            let den = U256::from_u128(a_basis).checked_mul(pos_scale_u256())
                .ok_or(RiskError::Overflow)?;
            let pnl_delta = wide_signed_mul_div_floor(abs_basis, k_diff, den);

            let old_pnl = self.accounts[idx].pnl;
            let new_pnl = old_pnl.checked_add(pnl_delta).ok_or(RiskError::Overflow)?;
            if new_pnl == I256::MIN {
                return Err(RiskError::Overflow);
            }
            self.set_pnl(idx, new_pnl);

            if q_eff_new.is_zero() {
                // Position effectively zeroed (spec §5.3 step 3)
                self.inc_phantom_dust_bound(side);
                self.set_position_basis_q(idx, I256::ZERO);
                // Reset snapshots in current epoch
                self.accounts[idx].adl_a_basis = ADL_ONE;
                self.accounts[idx].adl_k_snap = k_side;
                self.accounts[idx].adl_epoch_snap = epoch_side;
            } else {
                // Update k_snap only; do NOT change basis or a_basis (non-compounding)
                self.accounts[idx].adl_k_snap = k_side;
                self.accounts[idx].adl_epoch_snap = epoch_side;
            }
        } else {
            // Epoch mismatch (spec §5.3 step 4)
            let side_mode = self.get_side_mode(side);
            if side_mode != SideMode::ResetPending {
                return Err(RiskError::CorruptState);
            }
            if epoch_snap.checked_add(1) != Some(epoch_side) {
                return Err(RiskError::CorruptState);
            }

            let k_epoch_start = self.get_k_epoch_start(side);
            let k_snap = self.accounts[idx].adl_k_snap;

            let k_diff = k_epoch_start.checked_sub(k_snap).ok_or(RiskError::Overflow)?;
            let den = U256::from_u128(a_basis).checked_mul(pos_scale_u256())
                .ok_or(RiskError::Overflow)?;
            let pnl_delta = wide_signed_mul_div_floor(abs_basis, k_diff, den);

            let old_pnl = self.accounts[idx].pnl;
            let new_pnl = old_pnl.checked_add(pnl_delta).ok_or(RiskError::Overflow)?;
            if new_pnl == I256::MIN {
                return Err(RiskError::Overflow);
            }
            self.set_pnl(idx, new_pnl);

            self.set_position_basis_q(idx, I256::ZERO);

            // Decrement stale count
            let old_stale = self.get_stale_count(side);
            let new_stale = old_stale.checked_sub(1).ok_or(RiskError::CorruptState)?;
            self.set_stale_count(side, new_stale);

            // Reset snapshots in current epoch
            let k_side = self.get_k_side(side);
            self.accounts[idx].adl_a_basis = ADL_ONE;
            self.accounts[idx].adl_k_snap = k_side;
            self.accounts[idx].adl_epoch_snap = epoch_side;
        }

        Ok(())
    }

    // ========================================================================
    // accrue_market_to (spec §5.4)
    // ========================================================================

    pub fn accrue_market_to(&mut self, now_slot: u64, oracle_price: u64) -> Result<()> {
        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        let total_dt = now_slot.saturating_sub(self.last_market_slot);
        if total_dt == 0 && self.last_oracle_price == oracle_price {
            // No time elapsed and price unchanged — skip
            self.funding_price_sample_last = oracle_price;
            return Ok(());
        }

        // Read OI at start (fixed for all sub-steps per spec)
        let oi_long = self.oi_eff_long_q;
        let oi_short = self.oi_eff_short_q;
        let long_live = !oi_long.is_zero();
        let short_live = !oi_short.is_zero();

        // Same-slot price change: apply mark-only ΔP with no funding.
        // Without this, a price move within the same slot is silently dropped.
        if total_dt == 0 {
            let delta_p = (oracle_price as i128).checked_sub(self.last_oracle_price as i128)
                .ok_or(RiskError::Overflow)?;
            if delta_p != 0 {
                if long_live {
                    let a_long_256 = U256::from_u128(self.adl_mult_long);
                    let delta_p_i256 = I256::from_i128(delta_p);
                    let delta_k = checked_u256_mul_i256(a_long_256, delta_p_i256)?;
                    self.adl_coeff_long = self.adl_coeff_long.checked_add(delta_k)
                        .ok_or(RiskError::Overflow)?;
                }
                if short_live {
                    let a_short_256 = U256::from_u128(self.adl_mult_short);
                    let delta_p_i256 = I256::from_i128(delta_p);
                    let delta_k = checked_u256_mul_i256(a_short_256, delta_p_i256)?;
                    self.adl_coeff_short = self.adl_coeff_short.checked_sub(delta_k)
                        .ok_or(RiskError::Overflow)?;
                }
            }
            self.last_oracle_price = oracle_price;
            self.funding_price_sample_last = oracle_price;
            return Ok(());
        }

        let funding_rate = self.funding_rate_bps_per_slot_last;
        if funding_rate.abs() > MAX_ABS_FUNDING_BPS_PER_SLOT {
            return Err(RiskError::Overflow);
        }

        let fund_px = if self.funding_price_sample_last == 0 {
            oracle_price
        } else {
            self.funding_price_sample_last
        };

        // Process in bounded sub-steps (dt <= MAX_FUNDING_DT each)
        let mut remaining_dt = total_dt;
        let mut current_price = self.last_oracle_price;
        if current_price == 0 {
            current_price = oracle_price;
        }

        while remaining_dt > 0 {
            let dt = core::cmp::min(remaining_dt, MAX_FUNDING_DT);
            remaining_dt -= dt;

            // For intermediate sub-steps, price is linearly interpolated to oracle_price
            // at the final step. For simplicity (and spec compliance), we step price
            // to oracle_price at the last sub-step.
            let step_price = if remaining_dt == 0 { oracle_price } else { current_price };

            // Mark-to-market: ΔP = step_price - current_price
            let delta_p = (step_price as i128).checked_sub(current_price as i128)
                .ok_or(RiskError::Overflow)?;

            if delta_p != 0 {
                // K_long += A_long * ΔP (if long has OI)
                if long_live {
                    let a_long_256 = U256::from_u128(self.adl_mult_long);
                    let delta_p_i256 = I256::from_i128(delta_p);
                    // A_long * ΔP as signed: need checked signed multiply
                    let delta_k = checked_u256_mul_i256(a_long_256, delta_p_i256)?;
                    self.adl_coeff_long = self.adl_coeff_long.checked_add(delta_k)
                        .ok_or(RiskError::Overflow)?;
                }
                // K_short -= A_short * ΔP (if short has OI)
                if short_live {
                    let a_short_256 = U256::from_u128(self.adl_mult_short);
                    let delta_p_i256 = I256::from_i128(delta_p);
                    let delta_k = checked_u256_mul_i256(a_short_256, delta_p_i256)?;
                    self.adl_coeff_short = self.adl_coeff_short.checked_sub(delta_k)
                        .ok_or(RiskError::Overflow)?;
                }
            }

            // Funding: payer-driven rounding (spec v10.5 §5.4 step 5)
            if dt > 0 && funding_rate != 0 {
                // funding_term_raw = fund_px * |r_last| * dt (unsigned)
                let abs_rate = (funding_rate as i128).unsigned_abs();
                let funding_term_raw: u128 = (fund_px as u128)
                    .checked_mul(abs_rate)
                    .ok_or(RiskError::Overflow)?
                    .checked_mul(dt as u128)
                    .ok_or(RiskError::Overflow)?;

                if funding_term_raw > 0 {
                    // r_last > 0 → longs pay, shorts receive
                    // r_last < 0 → shorts pay, longs receive
                    let (payer_live, receiver_live) = if funding_rate > 0 {
                        (long_live, short_live)
                    } else {
                        (short_live, long_live)
                    };
                    let (a_payer, a_receiver) = if funding_rate > 0 {
                        (self.adl_mult_long, self.adl_mult_short)
                    } else {
                        (self.adl_mult_short, self.adl_mult_long)
                    };

                    if payer_live {
                        let a_p = U256::from_u128(a_payer);
                        let ft = U256::from_u128(funding_term_raw);
                        let ten_k = U256::from_u128(10_000);
                        // delta_K_payer_abs = ceil(A_p * funding_term_raw / 10_000)
                        let delta_k_payer_abs = mul_div_ceil_u256(a_p, ft, ten_k);
                        // Apply payer loss
                        let delta_k_payer_neg = try_negate_u256_to_i256(delta_k_payer_abs)
                            .ok_or(RiskError::Overflow)?;
                        if funding_rate > 0 {
                            self.adl_coeff_long = self.adl_coeff_long.checked_add(delta_k_payer_neg)
                                .ok_or(RiskError::Overflow)?;
                        } else {
                            self.adl_coeff_short = self.adl_coeff_short.checked_add(delta_k_payer_neg)
                                .ok_or(RiskError::Overflow)?;
                        }

                        // Derive receiver gain: floor(delta_K_payer_abs * A_r / A_p)
                        if receiver_live {
                            let a_r = U256::from_u128(a_receiver);
                            let delta_k_receiver_abs = mul_div_floor_u256(delta_k_payer_abs, a_r, a_p);
                            // Ensure fits as positive I256 (high bit clear)
                            if delta_k_receiver_abs.hi() >= (1u128 << 127) {
                                return Err(RiskError::Overflow);
                            }
                            let delta_k_receiver = I256::from_raw_u256_pub(delta_k_receiver_abs);
                            if funding_rate > 0 {
                                self.adl_coeff_short = self.adl_coeff_short.checked_add(delta_k_receiver)
                                    .ok_or(RiskError::Overflow)?;
                            } else {
                                self.adl_coeff_long = self.adl_coeff_long.checked_add(delta_k_receiver)
                                    .ok_or(RiskError::Overflow)?;
                            }
                        }
                    }
                }
            }

            current_price = step_price;
        }

        self.last_market_slot = now_slot;
        self.last_oracle_price = oracle_price;
        self.funding_price_sample_last = oracle_price;

        Ok(())
    }

    /// Set funding rate for next interval (spec §5.5 anti-retroactivity)
    pub fn set_funding_rate_for_next_interval(&mut self, new_rate: i64) {
        self.funding_rate_bps_per_slot_last = new_rate;
    }

    // ========================================================================
    // absorb_protocol_loss (spec §4.7)
    // ========================================================================

    pub fn absorb_protocol_loss(&mut self, loss: U256) {
        if loss.is_zero() {
            return;
        }
        let ins_bal = self.insurance_fund.balance.get();
        let available = ins_bal.saturating_sub(self.insurance_floor);
        let loss_u128 = u256_to_u128_sat(&loss);
        let pay = core::cmp::min(loss_u128, available);
        if pay > 0 {
            self.insurance_fund.balance = U128::new(ins_bal - pay);
        }
        // Remaining loss is implicit haircut through h
    }

    // ========================================================================
    // enqueue_adl (spec §5.6)
    // ========================================================================

    pub fn enqueue_adl(&mut self, ctx: &mut InstructionContext, liq_side: Side, q_close_q: U256, d: U256) -> Result<()> {
        let opp = opposite_side(liq_side);

        // Step 1: decrease liquidated side OI (checked — underflow is corrupt state)
        if !q_close_q.is_zero() {
            let old_oi = self.get_oi_eff(liq_side);
            let new_oi = old_oi.checked_sub(q_close_q).ok_or(RiskError::CorruptState)?;
            self.set_oi_eff(liq_side, new_oi);
        }

        // Step 2: read opposing OI
        let oi = self.get_oi_eff(opp);

        // Step 3: if OI == 0
        if oi.is_zero() {
            if !d.is_zero() {
                self.absorb_protocol_loss(d);
            }
            return Ok(());
        }

        // Step 4 (v10.5): if OI > 0 and stored_pos_count_opp == 0,
        // route deficit through absorb and do NOT modify K_opp.
        if self.get_stored_pos_count(opp) == 0 {
            if q_close_q > oi {
                return Err(RiskError::CorruptState);
            }
            let oi_post = oi.checked_sub(q_close_q).ok_or(RiskError::Overflow)?;
            if !d.is_zero() {
                self.absorb_protocol_loss(d);
            }
            self.set_oi_eff(opp, oi_post);
            if oi_post.is_zero() {
                set_pending_reset(ctx, opp);
            }
            return Ok(());
        }

        // Step 5: require q_close_q <= OI
        if q_close_q > oi {
            return Err(RiskError::CorruptState);
        }

        let a_old = self.get_a_side(opp);
        let a_old_u256 = U256::from_u128(a_old);
        let oi_post = oi.checked_sub(q_close_q).ok_or(RiskError::Overflow)?;

        // Step 6: handle D > 0 (quote deficit)
        // v10.5: fused delta_K_abs = ceil(D * A_old * POS_SCALE / OI)
        // Per §1.5 Rule 14: if the quotient doesn't fit in U256, route to
        // absorb_protocol_loss instead of panicking.
        if !d.is_zero() {
            let a_ps = a_old_u256.checked_mul(pos_scale_u256())
                .ok_or(RiskError::Overflow)?;
            match checked_mul_div_ceil_u256(d, a_ps, oi) {
                Some(delta_k_abs) => {
                    match try_negate_u256_to_i256(delta_k_abs) {
                        Some(delta_k) => {
                            let k_opp = self.get_k_side(opp);
                            match k_opp.checked_add(delta_k) {
                                Some(new_k) => {
                                    self.set_k_side(opp, new_k);
                                }
                                None => {
                                    self.absorb_protocol_loss(d);
                                }
                            }
                        }
                        None => {
                            self.absorb_protocol_loss(d);
                        }
                    }
                }
                None => {
                    // Quotient overflow: deficit too large to represent in K-space
                    self.absorb_protocol_loss(d);
                }
            }
        }

        // Step 7: if OI_post == 0
        if oi_post.is_zero() {
            self.set_oi_eff(opp, U256::ZERO);
            set_pending_reset(ctx, opp);
            return Ok(());
        }

        // Steps 8-9: compute A_candidate and A_trunc_rem
        let (a_candidate, a_trunc_rem) = mul_div_floor_u256_with_rem(
            a_old_u256,
            oi_post,
            oi,
        );

        // Step 10: A_candidate > 0
        if !a_candidate.is_zero() {
            let a_new = u256_to_u128_sat(&a_candidate);
            self.set_a_side(opp, a_new);
            self.set_oi_eff(opp, oi_post);
            // Only account for global A-truncation dust when actual truncation occurs
            if !a_trunc_rem.is_zero() {
                let n_opp = U256::from_u128(self.get_stored_pos_count(opp) as u128);
                // global_a_dust_bound = N_opp + ceil((OI + N_opp) / A_old)
                let oi_plus_n = oi.checked_add(n_opp).unwrap_or(U256::MAX);
                let ceil_term = ceil_div_positive_checked(oi_plus_n, a_old_u256);
                let global_a_dust_bound = n_opp.checked_add(ceil_term)
                    .unwrap_or(U256::MAX);
                self.inc_phantom_dust_bound_by(opp, global_a_dust_bound);
            }
            if a_new < MIN_A_SIDE {
                self.set_side_mode(opp, SideMode::DrainOnly);
            }
            return Ok(());
        }

        // Step 11: precision exhaustion terminal drain
        self.set_oi_eff(opp, U256::ZERO);
        self.set_oi_eff(liq_side, U256::ZERO);
        set_pending_reset(ctx, opp);
        set_pending_reset(ctx, liq_side);

        Ok(())
    }

    // ========================================================================
    // begin_full_drain_reset / finalize_side_reset (spec §2.5, §2.7)
    // ========================================================================

    pub fn begin_full_drain_reset(&mut self, side: Side) {
        // Require OI_eff_side == 0
        assert!(self.get_oi_eff(side).is_zero(), "begin_full_drain_reset: OI not zero");

        // K_epoch_start_side = K_side
        let k = self.get_k_side(side);
        match side {
            Side::Long => self.adl_epoch_start_k_long = k,
            Side::Short => self.adl_epoch_start_k_short = k,
        }

        // Increment epoch
        match side {
            Side::Long => self.adl_epoch_long = self.adl_epoch_long.checked_add(1)
                .expect("epoch overflow"),
            Side::Short => self.adl_epoch_short = self.adl_epoch_short.checked_add(1)
                .expect("epoch overflow"),
        }

        // A_side = ADL_ONE
        self.set_a_side(side, ADL_ONE);

        // stale_account_count_side = stored_pos_count_side
        let spc = self.get_stored_pos_count(side);
        self.set_stale_count(side, spc);

        // phantom_dust_bound_side_q = 0 (spec §2.5 step 6)
        match side {
            Side::Long => self.phantom_dust_bound_long_q = U256::ZERO,
            Side::Short => self.phantom_dust_bound_short_q = U256::ZERO,
        }

        // mode = ResetPending
        self.set_side_mode(side, SideMode::ResetPending);
    }

    pub fn finalize_side_reset(&mut self, side: Side) -> Result<()> {
        if self.get_side_mode(side) != SideMode::ResetPending {
            return Err(RiskError::CorruptState);
        }
        if !self.get_oi_eff(side).is_zero() {
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

    // ========================================================================
    // schedule_end_of_instruction_resets / finalize (spec §5.7-5.8)
    // ========================================================================

    pub fn schedule_end_of_instruction_resets(&mut self, ctx: &mut InstructionContext) -> Result<()> {
        // §5.7.A: Bilateral-empty dust clearance
        if self.stored_pos_count_long == 0 && self.stored_pos_count_short == 0 {
            let clear_bound_q = self.phantom_dust_bound_long_q
                .checked_add(self.phantom_dust_bound_short_q)
                .ok_or(RiskError::CorruptState)?;
            let has_residual = !self.oi_eff_long_q.is_zero()
                || !self.oi_eff_short_q.is_zero()
                || !self.phantom_dust_bound_long_q.is_zero()
                || !self.phantom_dust_bound_short_q.is_zero();
            if has_residual {
                if self.oi_eff_long_q != self.oi_eff_short_q {
                    return Err(RiskError::CorruptState);
                }
                if self.oi_eff_long_q <= clear_bound_q && self.oi_eff_short_q <= clear_bound_q {
                    self.oi_eff_long_q = U256::ZERO;
                    self.oi_eff_short_q = U256::ZERO;
                    ctx.pending_reset_long = true;
                    ctx.pending_reset_short = true;
                } else {
                    return Err(RiskError::CorruptState);
                }
            }
        }
        // §5.7.B: Unilateral-empty long (long empty, short has positions)
        else if self.stored_pos_count_long == 0 && self.stored_pos_count_short > 0 {
            let has_residual = !self.oi_eff_long_q.is_zero()
                || !self.oi_eff_short_q.is_zero()
                || !self.phantom_dust_bound_long_q.is_zero();
            if has_residual {
                if self.oi_eff_long_q != self.oi_eff_short_q {
                    return Err(RiskError::CorruptState);
                }
                if self.oi_eff_long_q <= self.phantom_dust_bound_long_q {
                    self.oi_eff_long_q = U256::ZERO;
                    self.oi_eff_short_q = U256::ZERO;
                    ctx.pending_reset_long = true;
                    ctx.pending_reset_short = true;
                } else {
                    return Err(RiskError::CorruptState);
                }
            }
        }
        // §5.7.C: Unilateral-empty short (short empty, long has positions)
        else if self.stored_pos_count_short == 0 && self.stored_pos_count_long > 0 {
            let has_residual = !self.oi_eff_long_q.is_zero()
                || !self.oi_eff_short_q.is_zero()
                || !self.phantom_dust_bound_short_q.is_zero();
            if has_residual {
                if self.oi_eff_long_q != self.oi_eff_short_q {
                    return Err(RiskError::CorruptState);
                }
                if self.oi_eff_short_q <= self.phantom_dust_bound_short_q {
                    self.oi_eff_long_q = U256::ZERO;
                    self.oi_eff_short_q = U256::ZERO;
                    ctx.pending_reset_long = true;
                    ctx.pending_reset_short = true;
                } else {
                    return Err(RiskError::CorruptState);
                }
            }
        }

        // §5.7.D: DrainOnly sides with zero OI
        if self.side_mode_long == SideMode::DrainOnly && self.oi_eff_long_q.is_zero() {
            ctx.pending_reset_long = true;
        }
        if self.side_mode_short == SideMode::DrainOnly && self.oi_eff_short_q.is_zero() {
            ctx.pending_reset_short = true;
        }

        Ok(())
    }

    pub fn finalize_end_of_instruction_resets(&mut self, ctx: &InstructionContext) {
        if ctx.pending_reset_long && self.side_mode_long != SideMode::ResetPending {
            self.begin_full_drain_reset(Side::Long);
        }
        if ctx.pending_reset_short && self.side_mode_short != SideMode::ResetPending {
            self.begin_full_drain_reset(Side::Short);
        }
        // Auto-finalize sides that are fully ready for reopening
        self.maybe_finalize_ready_reset_sides();
    }

    /// Preflight finalize: if a side is ResetPending with OI=0, stale=0, pos_count=0,
    /// transition it back to Normal so fresh OI can be added.
    /// Called before OI-increase gating and at end-of-instruction.
    pub fn maybe_finalize_ready_reset_sides(&mut self) {
        if self.side_mode_long == SideMode::ResetPending
            && self.get_oi_eff(Side::Long).is_zero()
            && self.get_stale_count(Side::Long) == 0
            && self.get_stored_pos_count(Side::Long) == 0
        {
            self.set_side_mode(Side::Long, SideMode::Normal);
        }
        if self.side_mode_short == SideMode::ResetPending
            && self.get_oi_eff(Side::Short).is_zero()
            && self.get_stale_count(Side::Short) == 0
            && self.get_stored_pos_count(Side::Short) == 0
        {
            self.set_side_mode(Side::Short, SideMode::Normal);
        }
    }

    // ========================================================================
    // Haircut and Equity (spec §3)
    // ========================================================================

    /// Compute haircut ratio (h_num, h_den) as U256 pair (spec §3.2)
    pub fn haircut_ratio(&self) -> (U256, U256) {
        if self.pnl_pos_tot.is_zero() {
            return (U256::ONE, U256::ONE);
        }
        let senior_sum = self.c_tot.get().checked_add(self.insurance_fund.balance.get());
        let residual = match senior_sum {
            Some(ss) => {
                if self.vault.get() >= ss {
                    U256::from_u128(self.vault.get() - ss)
                } else {
                    U256::ZERO
                }
            }
            None => U256::ZERO, // overflow in senior_sum → deficit
        };
        let h_num = if residual < self.pnl_pos_tot { residual } else { self.pnl_pos_tot };
        (h_num, self.pnl_pos_tot)
    }

    /// effective_pos_pnl (spec §3.3): floor(max(PNL_i, 0) * h_num / h_den) as U256
    pub fn effective_pos_pnl(&self, pnl: &I256) -> U256 {
        if !pnl.is_positive() {
            return U256::ZERO;
        }
        let pos_pnl = pnl.abs_u256();
        let (h_num, h_den) = self.haircut_ratio();
        if h_den.is_zero() {
            return pos_pnl;
        }
        mul_div_floor_u256(pos_pnl, h_num, h_den)
    }

    /// account_equity_net (spec §3.3): Eq_net_i = max(0, Eq_real_i - FeeDebt_i)
    /// Returns as I256 for margin comparison
    pub fn account_equity_net(&self, account: &Account, _oracle_price: u64) -> I256 {
        // Eq_real_i = max(0, C_i + min(PNL_i, 0) + PNL_eff_pos_i)
        let cap_i256 = u128_to_i256(account.capital.get());
        let neg_pnl = if account.pnl.is_negative() {
            account.pnl
        } else {
            I256::ZERO
        };
        let eff_pos = self.effective_pos_pnl(&account.pnl);
        let eff_pos_i = I256::from_raw_u256_pub(eff_pos);

        let eq_real = cap_i256.saturating_add(neg_pnl).saturating_add(eff_pos_i);

        let eq_real_clamped = if eq_real.is_negative() { I256::ZERO } else { eq_real };

        // Subtract fee debt
        let fee_debt = fee_debt_u128_checked(account.fee_credits.get());
        let fee_debt_i256 = u128_to_i256(fee_debt);

        let eq_net = eq_real_clamped.checked_sub(fee_debt_i256).unwrap_or(I256::ZERO);
        if eq_net.is_negative() { I256::ZERO } else { eq_net }
    }

    /// notional (spec §9.1): floor(|effective_pos_q| * oracle_price / POS_SCALE)
    pub fn notional(&self, idx: usize, oracle_price: u64) -> u128 {
        let eff = self.effective_pos_q(idx);
        if eff.is_zero() {
            return 0;
        }
        let abs_eff = eff.abs_u256();
        let result = mul_div_floor_u256(abs_eff, U256::from_u128(oracle_price as u128), pos_scale_u256());
        u256_to_u128_sat(&result)
    }

    /// is_above_maintenance_margin (spec §9.1)
    pub fn is_above_maintenance_margin(&self, account: &Account, idx: usize, oracle_price: u64) -> bool {
        let eq_net = self.account_equity_net(account, oracle_price);
        let not = self.notional(idx, oracle_price);
        let mm_req = mul_u128(not, self.params.maintenance_margin_bps as u128) / 10_000;
        let mm_req_i256 = u128_to_i256(mm_req);
        eq_net > mm_req_i256
    }

    /// is_above_initial_margin (spec §9.1)
    pub fn is_above_initial_margin(&self, account: &Account, idx: usize, oracle_price: u64) -> bool {
        let eq_net = self.account_equity_net(account, oracle_price);
        let not = self.notional(idx, oracle_price);
        let im_req = mul_u128(not, self.params.initial_margin_bps as u128) / 10_000;
        let im_req_i256 = u128_to_i256(im_req);
        eq_net >= im_req_i256
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

    // ========================================================================
    // Warmup Helpers (spec §6)
    // ========================================================================

    /// avail_gross (spec §6.2): max(PNL_i, 0) - R_i
    fn avail_gross(&self, idx: usize) -> U256 {
        let pnl = &self.accounts[idx].pnl;
        let pos_pnl = i256_clamp_pos(pnl);
        let reserved = self.accounts[idx].reserved_pnl;
        pos_pnl.saturating_sub(reserved)
    }

    /// warmable_gross (spec §6.3)
    pub fn warmable_gross(&self, idx: usize) -> U256 {
        let avail = self.avail_gross(idx);
        if avail.is_zero() {
            return U256::ZERO;
        }
        let t = self.params.warmup_period_slots;
        if t == 0 {
            return avail;
        }
        let elapsed = self.current_slot.saturating_sub(self.accounts[idx].warmup_started_at_slot);
        let cap = saturating_mul_u256_u64(self.accounts[idx].warmup_slope_per_step, elapsed);
        if avail < cap { avail } else { cap }
    }

    /// update_warmup_slope (spec §6.4)
    pub fn update_warmup_slope(&mut self, idx: usize) {
        let avail = self.avail_gross(idx);
        let t = self.params.warmup_period_slots;

        let slope = if avail.is_zero() {
            U256::ZERO
        } else if t == 0 {
            avail
        } else {
            let base = avail.checked_div(U256::from_u128(t as u128)).unwrap_or(U256::ZERO);
            if base.is_zero() { U256::ONE } else { base }
        };

        self.accounts[idx].warmup_slope_per_step = slope;
        self.accounts[idx].warmup_started_at_slot = self.current_slot;
    }

    /// restart_on_new_profit (spec §6.5)
    fn restart_on_new_profit(&mut self, idx: usize, old_warmable: U256) {
        // Step 1: convert old_warmable if > 0
        if !old_warmable.is_zero() {
            self.do_profit_conversion(idx, old_warmable);
        }
        // Step 2: update warmup slope with new remaining AvailGross
        self.update_warmup_slope(idx);
    }

    // ========================================================================
    // Loss settlement and profit conversion (spec §7)
    // ========================================================================

    /// settle_losses (spec §7.1): settle negative PnL from principal
    fn settle_losses(&mut self, idx: usize) {
        let pnl = self.accounts[idx].pnl;
        if !pnl.is_negative() {
            return;
        }
        assert!(pnl != I256::MIN, "settle_losses: I256::MIN");
        let need = pnl.abs_u256();
        let need_u128 = u256_to_u128_sat(&need);
        let cap = self.accounts[idx].capital.get();
        let pay = core::cmp::min(need_u128, cap);
        if pay > 0 {
            self.set_capital(idx, cap - pay);
            let pay_i256 = I256::from_u128(pay);
            let new_pnl = pnl.checked_add(pay_i256).unwrap_or(I256::ZERO);
            if new_pnl == I256::MIN {
                self.set_pnl(idx, I256::ZERO);
            } else {
                self.set_pnl(idx, new_pnl);
            }
        }
    }

    /// resolve_flat_negative (spec §7.3): for flat accounts with negative PnL
    fn resolve_flat_negative(&mut self, idx: usize) {
        let eff = self.effective_pos_q(idx);
        if !eff.is_zero() {
            return; // Not flat — must resolve through liquidation
        }
        let pnl = self.accounts[idx].pnl;
        if pnl.is_negative() {
            let loss = pnl.abs_u256();
            self.absorb_protocol_loss(loss);
            self.set_pnl(idx, I256::ZERO);
        }
    }

    /// do_profit_conversion (spec §7.4): convert warmable x to capital — checked.
    fn do_profit_conversion(&mut self, idx: usize, x: U256) {
        if x.is_zero() {
            return;
        }
        // Compute y using pre-conversion haircut
        let (h_num, h_den) = self.haircut_ratio();
        let y = if h_den.is_zero() {
            x
        } else {
            mul_div_floor_u256(x, h_num, h_den)
        };
        let y_u128 = y.try_into_u128().expect("do_profit_conversion: y exceeds u128");

        // set_pnl(i, PNL_i - x)
        let x_i256 = I256::from_raw_u256_pub(x);
        let old_pnl = self.accounts[idx].pnl;
        let new_pnl = old_pnl.checked_sub(x_i256)
            .expect("do_profit_conversion: PnL underflow");
        assert!(new_pnl != I256::MIN, "do_profit_conversion: PnL == I256::MIN");
        self.set_pnl(idx, new_pnl);

        // set_capital(i, C_i + y)
        let new_cap = add_u128(self.accounts[idx].capital.get(), y_u128);
        self.set_capital(idx, new_cap);

        // Handle warmup schedule per spec §7.4
        let t = self.params.warmup_period_slots;
        let new_avail = self.avail_gross(idx);
        if t == 0 {
            self.accounts[idx].warmup_started_at_slot = self.current_slot;
            self.accounts[idx].warmup_slope_per_step = if new_avail.is_zero() {
                U256::ZERO
            } else {
                new_avail
            };
        } else if new_avail.is_zero() {
            self.accounts[idx].warmup_slope_per_step = U256::ZERO;
            self.accounts[idx].warmup_started_at_slot = self.current_slot;
        } else {
            // Preserve existing slope, just reset start
            self.accounts[idx].warmup_started_at_slot = self.current_slot;
        }
    }

    /// settle_warmup_to_capital (spec §7.4): convert warmable profits
    fn settle_warmup_to_capital(&mut self, idx: usize) {
        let x = self.warmable_gross(idx);
        self.do_profit_conversion(idx, x);
    }

    /// fee_debt_sweep (spec §7.5): after any capital increase, sweep fee debt
    fn fee_debt_sweep(&mut self, idx: usize) {
        let fc = self.accounts[idx].fee_credits.get();
        let debt = fee_debt_u128_checked(fc);
        if debt == 0 {
            return;
        }
        let cap = self.accounts[idx].capital.get();
        let pay = core::cmp::min(debt, cap);
        if pay > 0 {
            self.set_capital(idx, cap - pay);
            // pay <= debt = |fee_credits|, so fee_credits + pay <= 0: no overflow
            let pay_i128 = core::cmp::min(pay, i128::MAX as u128) as i128;
            self.accounts[idx].fee_credits = self.accounts[idx].fee_credits
                .saturating_add(pay_i128);
            self.insurance_fund.balance = self.insurance_fund.balance + pay;
        }
    }

    // ========================================================================
    // touch_account_full (spec §10.1)
    // ========================================================================

    pub fn touch_account_full(&mut self, idx: usize, oracle_price: u64, now_slot: u64) -> Result<()> {
        // Step 1
        self.current_slot = now_slot;

        // Step 2
        self.accrue_market_to(now_slot, oracle_price)?;

        // Step 3-4: capture old_avail and old_warmable before settle
        let old_avail = self.avail_gross(idx);
        let old_warmable = self.warmable_gross(idx);

        // Step 5: settle_side_effects
        self.settle_side_effects(idx)?;

        // Step 6-7: check if avail increased → restart-on-new-profit
        let new_avail = self.avail_gross(idx);
        if new_avail > old_avail {
            let cap_before = self.accounts[idx].capital.get();
            self.restart_on_new_profit(idx, old_warmable);
            // Fee-debt seniority: if restart conversion increased capital,
            // sweep fee debt immediately before any later capital-consuming logic.
            let cap_after = self.accounts[idx].capital.get();
            if cap_after > cap_before {
                self.fee_debt_sweep(idx);
            }
        }

        // Step 8: maintenance fees
        self.settle_maintenance_fee_internal(idx, now_slot)?;

        // Step 9: settle losses from principal
        self.settle_losses(idx);

        // Step 10: resolve flat negative
        self.resolve_flat_negative(idx);

        // Step 11: convert warmable profits
        self.settle_warmup_to_capital(idx);

        // Step 12: fee debt sweep
        self.fee_debt_sweep(idx);

        Ok(())
    }

    /// Internal maintenance fee settle — checked arithmetic, no margin check.
    fn settle_maintenance_fee_internal(&mut self, idx: usize, now_slot: u64) -> Result<()> {
        let dt = now_slot.saturating_sub(self.accounts[idx].last_fee_slot);
        if dt == 0 {
            return Ok(());
        }
        let fee_per_slot = self.params.maintenance_fee_per_slot.get();
        let due = fee_per_slot.checked_mul(dt as u128)
            .ok_or(RiskError::Overflow)?;
        self.accounts[idx].last_fee_slot = now_slot;

        // Deduct from fee_credits — checked subtraction.
        let due_i128: i128 = due.try_into().map_err(|_| RiskError::Overflow)?;
        let new_fc = self.accounts[idx].fee_credits.get()
            .checked_sub(due_i128).ok_or(RiskError::Overflow)?;
        self.accounts[idx].fee_credits = I128::new(new_fc);

        // Pay from capital if negative
        if self.accounts[idx].fee_credits.is_negative() {
            let owed_i128 = self.accounts[idx].fee_credits.get();
            let owed = fee_debt_u128_checked(owed_i128);
            let cap = self.accounts[idx].capital.get();
            let pay = core::cmp::min(owed, cap);
            if pay > 0 {
                self.set_capital(idx, cap - pay);
                self.insurance_fund.balance = self.insurance_fund.balance + pay;
                self.insurance_fund.fee_revenue = self.insurance_fund.fee_revenue + pay;
                let pay_i128: i128 = pay.try_into().map_err(|_| RiskError::Overflow)?;
                let new_credits = self.accounts[idx].fee_credits.get()
                    .checked_add(pay_i128).ok_or(RiskError::Overflow)?;
                self.accounts[idx].fee_credits = I128::new(new_credits);
            }
        }
        Ok(())
    }

    // ========================================================================
    // Account Management
    // ========================================================================

    pub fn add_user(&mut self, fee_payment: u128) -> Result<u16> {
        let used_count = self.num_used_accounts as u64;
        if used_count >= self.params.max_accounts {
            return Err(RiskError::Overflow);
        }

        let required_fee = self.params.new_account_fee.get();
        if fee_payment < required_fee {
            return Err(RiskError::InsufficientBalance);
        }

        let excess = fee_payment.saturating_sub(required_fee);

        self.vault = self.vault + fee_payment;
        self.insurance_fund.balance = self.insurance_fund.balance + required_fee;
        self.insurance_fund.fee_revenue = self.insurance_fund.fee_revenue + required_fee;

        let idx = self.alloc_slot()?;
        let account_id = self.next_account_id;
        self.next_account_id = self.next_account_id.saturating_add(1);

        self.accounts[idx as usize] = Account {
            kind: AccountKind::User,
            account_id,
            capital: U128::new(excess),
            pnl: I256::ZERO,
            reserved_pnl: U256::ZERO,
            warmup_started_at_slot: self.current_slot,
            warmup_slope_per_step: U256::ZERO,
            position_basis_q: I256::ZERO,
            adl_a_basis: ADL_ONE,
            adl_k_snap: I256::ZERO,
            adl_epoch_snap: 0,
            matcher_program: [0; 32],
            matcher_context: [0; 32],
            owner: [0; 32],
            fee_credits: I128::ZERO,
            last_fee_slot: self.current_slot,
            fees_earned_total: U128::ZERO,
        };

        if excess > 0 {
            self.c_tot = U128::new(self.c_tot.get().checked_add(excess)
                .ok_or(RiskError::Overflow)?);
        }

        Ok(idx)
    }

    pub fn add_lp(
        &mut self,
        matching_engine_program: [u8; 32],
        matching_engine_context: [u8; 32],
        fee_payment: u128,
    ) -> Result<u16> {
        let used_count = self.num_used_accounts as u64;
        if used_count >= self.params.max_accounts {
            return Err(RiskError::Overflow);
        }

        let required_fee = self.params.new_account_fee.get();
        if fee_payment < required_fee {
            return Err(RiskError::InsufficientBalance);
        }

        let excess = fee_payment.saturating_sub(required_fee);

        self.vault = self.vault + fee_payment;
        self.insurance_fund.balance = self.insurance_fund.balance + required_fee;
        self.insurance_fund.fee_revenue = self.insurance_fund.fee_revenue + required_fee;

        let idx = self.alloc_slot()?;
        let account_id = self.next_account_id;
        self.next_account_id = self.next_account_id.saturating_add(1);

        self.accounts[idx as usize] = Account {
            kind: AccountKind::LP,
            account_id,
            capital: U128::new(excess),
            pnl: I256::ZERO,
            reserved_pnl: U256::ZERO,
            warmup_started_at_slot: self.current_slot,
            warmup_slope_per_step: U256::ZERO,
            position_basis_q: I256::ZERO,
            adl_a_basis: ADL_ONE,
            adl_k_snap: I256::ZERO,
            adl_epoch_snap: 0,
            matcher_program: matching_engine_program,
            matcher_context: matching_engine_context,
            owner: [0; 32],
            fee_credits: I128::ZERO,
            last_fee_slot: self.current_slot,
            fees_earned_total: U128::ZERO,
        };

        if excess > 0 {
            self.c_tot = U128::new(self.c_tot.get().checked_add(excess)
                .ok_or(RiskError::Overflow)?);
        }

        Ok(idx)
    }

    pub fn set_owner(&mut self, idx: u16, owner: [u8; 32]) -> Result<()> {
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::Unauthorized);
        }
        self.accounts[idx as usize].owner = owner;
        Ok(())
    }

    // ========================================================================
    // deposit (spec §10.2)
    // ========================================================================

    pub fn deposit(&mut self, idx: u16, amount: u128, oracle_price: u64, now_slot: u64) -> Result<()> {
        self.current_slot = now_slot;

        if !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        // V += amount
        self.vault = U128::new(add_u128(self.vault.get(), amount));

        // set_capital(i, C_i + amount)
        let new_cap = add_u128(self.accounts[idx as usize].capital.get(), amount);
        self.set_capital(idx as usize, new_cap);

        // Fee debt sweep (spec §10.2)
        self.fee_debt_sweep(idx as usize);

        Ok(())
    }

    // ========================================================================
    // withdraw (spec §10.3)
    // ========================================================================

    pub fn withdraw(
        &mut self,
        idx: u16,
        amount: u128,
        oracle_price: u64,
        now_slot: u64,
    ) -> Result<()> {
        self.current_slot = now_slot;

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        self.require_fresh_crank(now_slot)?;

        if !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        let mut ctx = InstructionContext::new();

        // touch_account_full
        self.touch_account_full(idx as usize, oracle_price, now_slot)?;

        // require amount <= C_i
        if self.accounts[idx as usize].capital.get() < amount {
            return Err(RiskError::InsufficientBalance);
        }

        // If position exists, require post-withdraw initial margin
        let eff = self.effective_pos_q(idx as usize);
        if !eff.is_zero() {
            // Simulate withdrawal
            let new_cap = self.accounts[idx as usize].capital.get() - amount;
            let old_cap = self.accounts[idx as usize].capital.get();
            self.set_capital(idx as usize, new_cap);
            let passes_im = self.is_above_initial_margin(&self.accounts[idx as usize], idx as usize, oracle_price);
            // Revert
            self.set_capital(idx as usize, old_cap);
            if !passes_im {
                return Err(RiskError::Undercollateralized);
            }
        }

        // Commit withdrawal
        self.set_capital(idx as usize, self.accounts[idx as usize].capital.get() - amount);
        self.vault = U128::new(sub_u128(self.vault.get(), amount));

        // End-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx);

        Ok(())
    }

    // ========================================================================
    // execute_trade (spec §10.4)
    // ========================================================================

    pub fn execute_trade(
        &mut self,
        a: u16,
        b: u16,
        oracle_price: u64,
        now_slot: u64,
        size_q: I256,
        exec_price: u64,
    ) -> Result<()> {
        self.current_slot = now_slot;

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }
        if exec_price == 0 || exec_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }
        if size_q.is_zero() || size_q == I256::MIN {
            return Err(RiskError::Overflow);
        }

        // Validate size bounds
        let abs_size = size_q.abs_u256();
        if abs_size > max_position_abs_q() {
            return Err(RiskError::Overflow);
        }

        self.require_fresh_crank(now_slot)?;

        if !self.is_used(a as usize) || !self.is_used(b as usize) {
            return Err(RiskError::AccountNotFound);
        }
        if a == b {
            return Err(RiskError::Overflow);
        }

        let mut ctx = InstructionContext::new();

        // Step 2-3: touch both
        self.touch_account_full(a as usize, oracle_price, now_slot)?;
        self.touch_account_full(b as usize, oracle_price, now_slot)?;

        // Capture post-touch, pre-trade AvailGross and warmable for restart logic
        let old_avail_a = self.avail_gross(a as usize);
        let old_warmable_a = self.warmable_gross(a as usize);
        let old_avail_b = self.avail_gross(b as usize);
        let old_warmable_b = self.warmable_gross(b as usize);

        // Step 4: capture old effective positions
        let old_eff_a = self.effective_pos_q(a as usize);
        let old_eff_b = self.effective_pos_q(b as usize);

        // Step 6: compute new effective positions
        let new_eff_a = old_eff_a.checked_add(size_q).ok_or(RiskError::Overflow)?;
        let neg_size_q = size_q.checked_neg().ok_or(RiskError::Overflow)?;
        let new_eff_b = old_eff_b.checked_add(neg_size_q).ok_or(RiskError::Overflow)?;

        // Validate position bounds
        if !new_eff_a.is_zero() && new_eff_a.abs_u256() > max_position_abs_q() {
            return Err(RiskError::Overflow);
        }
        if !new_eff_b.is_zero() && new_eff_b.abs_u256() > max_position_abs_q() {
            return Err(RiskError::Overflow);
        }

        // Preflight: finalize any ResetPending sides that are fully ready,
        // so OI-increase gating doesn't block trades on reopenable sides.
        self.maybe_finalize_ready_reset_sides();

        // Step 5: reject if trade would increase OI on a blocked side
        self.check_side_mode_for_trade(&old_eff_a, &new_eff_a, &old_eff_b, &new_eff_b)?;

        // Step 7: trade PnL alignment
        // trade_pnl_a = floor_div_signed_conservative(size_q * (oracle - exec), POS_SCALE)
        let price_diff = I256::from_i128((oracle_price as i128) - (exec_price as i128));
        let trade_pnl_a = compute_trade_pnl(size_q, price_diff)?;
        let trade_pnl_b = trade_pnl_a.checked_neg().ok_or(RiskError::Overflow)?;

        let pnl_a = self.accounts[a as usize].pnl.checked_add(trade_pnl_a).ok_or(RiskError::Overflow)?;
        if pnl_a == I256::MIN { return Err(RiskError::Overflow); }
        self.set_pnl(a as usize, pnl_a);

        let pnl_b = self.accounts[b as usize].pnl.checked_add(trade_pnl_b).ok_or(RiskError::Overflow)?;
        if pnl_b == I256::MIN { return Err(RiskError::Overflow); }
        self.set_pnl(b as usize, pnl_b);

        // Step 8: attach effective positions
        self.attach_effective_position(a as usize, new_eff_a);
        self.attach_effective_position(b as usize, new_eff_b);

        // Step 9: update OI
        self.update_oi_from_positions(&old_eff_a, &new_eff_a, &old_eff_b, &new_eff_b)?;

        // Step 10: charge trading fees (spec §8.1)
        let trade_notional = {
            let tn = mul_div_floor_u256(abs_size, U256::from_u128(exec_price as u128), pos_scale_u256());
            u256_to_u128_sat(&tn)
        };
        let fee = if trade_notional > 0 && self.params.trading_fee_bps > 0 {
            let raw = U256::from_u128(trade_notional)
                .checked_mul(U256::from_u128(self.params.trading_fee_bps as u128))
                .unwrap_or(U256::ZERO);
            let fee_u256 = ceil_div_positive_checked(raw, U256::from_u128(10_000));
            u256_to_u128_sat(&fee_u256)
        } else {
            0
        };

        // Charge fee from account a (payer)
        if fee > 0 {
            self.charge_fee_safe(a as usize, fee)?;
        }

        // Track LP fees
        if self.accounts[b as usize].is_lp() {
            self.accounts[b as usize].fees_earned_total = U128::new(
                add_u128(self.accounts[b as usize].fees_earned_total.get(), fee)
            );
        }

        // Step 11: settle post-trade losses from principal for both accounts (spec §10.4)
        self.settle_losses(a as usize);
        self.settle_losses(b as usize);

        // Step 12: restart-on-new-profit only for accounts whose AvailGross actually increased
        // Per §6.5 step 2: if restart conversion increases C_i, sweep fee debt immediately
        // before any subsequent margin assessment.
        {
            let new_avail_a = self.avail_gross(a as usize);
            if new_avail_a > old_avail_a {
                let cap_before_a = self.accounts[a as usize].capital.get();
                self.restart_on_new_profit(a as usize, old_warmable_a);
                if self.accounts[a as usize].capital.get() > cap_before_a {
                    self.fee_debt_sweep(a as usize);
                }
            }
            let new_avail_b = self.avail_gross(b as usize);
            if new_avail_b > old_avail_b {
                let cap_before_b = self.accounts[b as usize].capital.get();
                self.restart_on_new_profit(b as usize, old_warmable_b);
                if self.accounts[b as usize].capital.get() > cap_before_b {
                    self.fee_debt_sweep(b as usize);
                }
            }
        }

        // Step 13: if funding-rate inputs changed, recompute r_last (spec §10.4)
        // (No-op: execute_trade does not modify funding-rate inputs)

        // Step 14: post-trade margin
        // Account a
        if !new_eff_a.is_zero() {
            let abs_old_a = if old_eff_a.is_zero() { U256::ZERO } else { old_eff_a.abs_u256() };
            let abs_new_a = new_eff_a.abs_u256();
            let risk_increasing_a = abs_new_a > abs_old_a
                || (old_eff_a.is_positive() && new_eff_a.is_negative())
                || (old_eff_a.is_negative() && new_eff_a.is_positive())
                || old_eff_a.is_zero();

            // Always require maintenance
            if !self.is_above_maintenance_margin(&self.accounts[a as usize], a as usize, oracle_price) {
                return Err(RiskError::Undercollateralized);
            }
            // If risk-increasing, also require initial margin
            if risk_increasing_a {
                if !self.is_above_initial_margin(&self.accounts[a as usize], a as usize, oracle_price) {
                    return Err(RiskError::Undercollateralized);
                }
            }
        } else {
            // Flat: after settle_losses, PnL must be >= 0.
            // Do NOT call resolve_flat_negative — that would let the protocol
            // absorb losses that should force this trade to be rejected.
            if self.accounts[a as usize].pnl.is_negative() {
                return Err(RiskError::Undercollateralized);
            }
        }

        // Account b
        if !new_eff_b.is_zero() {
            let abs_old_b = if old_eff_b.is_zero() { U256::ZERO } else { old_eff_b.abs_u256() };
            let abs_new_b = new_eff_b.abs_u256();
            let risk_increasing_b = abs_new_b > abs_old_b
                || (old_eff_b.is_positive() && new_eff_b.is_negative())
                || (old_eff_b.is_negative() && new_eff_b.is_positive())
                || old_eff_b.is_zero();

            if !self.is_above_maintenance_margin(&self.accounts[b as usize], b as usize, oracle_price) {
                return Err(RiskError::Undercollateralized);
            }
            if risk_increasing_b {
                if !self.is_above_initial_margin(&self.accounts[b as usize], b as usize, oracle_price) {
                    return Err(RiskError::Undercollateralized);
                }
            }
        } else {
            // Flat: after settle_losses, PnL must be >= 0.
            if self.accounts[b as usize].pnl.is_negative() {
                return Err(RiskError::Undercollateralized);
            }
        }

        // Step 15: fee debt sweep
        self.fee_debt_sweep(a as usize);
        self.fee_debt_sweep(b as usize);

        // Steps 16-17: end-of-instruction resets
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx);

        // Step 18: assert OI balance (spec §10.4)
        assert!(self.oi_eff_long_q == self.oi_eff_short_q, "OI_eff_long != OI_eff_short after trade");

        Ok(())
    }

    /// Charge fee per spec §8.1 — checked arithmetic, returns error on overflow.
    fn charge_fee_safe(&mut self, idx: usize, fee: u128) -> Result<()> {
        let cap = self.accounts[idx].capital.get();
        let fee_paid = core::cmp::min(fee, cap);
        if fee_paid > 0 {
            self.set_capital(idx, cap - fee_paid);
            self.insurance_fund.balance = self.insurance_fund.balance + fee_paid;
            self.insurance_fund.fee_revenue = self.insurance_fund.fee_revenue + fee_paid;
        }
        let fee_shortfall = fee - fee_paid;
        if fee_shortfall > 0 {
            let shortfall_i256 = I256::from_u128(fee_shortfall);
            let old_pnl = self.accounts[idx].pnl;
            let new_pnl = old_pnl.checked_sub(shortfall_i256)
                .ok_or(RiskError::Overflow)?;
            if new_pnl == I256::MIN {
                return Err(RiskError::Overflow);
            }
            self.set_pnl(idx, new_pnl);
        }
        Ok(())
    }

    /// Check side-mode gating: reject trade if net OI increases on a blocked side (spec §9.6)
    fn check_side_mode_for_trade(
        &self,
        old_a: &I256, new_a: &I256,
        old_b: &I256, new_b: &I256,
    ) -> Result<()> {
        for &side in &[Side::Long, Side::Short] {
            let mode = self.get_side_mode(side);
            if mode != SideMode::DrainOnly && mode != SideMode::ResetPending {
                continue;
            }
            let oi_contrib = |pos: &I256| -> U256 {
                match side_of_i256(pos) {
                    Some(s) if s == side => pos.abs_u256(),
                    _ => U256::ZERO,
                }
            };
            let old_total = oi_contrib(old_a).checked_add(oi_contrib(old_b)).unwrap_or(U256::MAX);
            let new_total = oi_contrib(new_a).checked_add(oi_contrib(new_b)).unwrap_or(U256::MAX);
            if new_total > old_total {
                return Err(RiskError::SideBlocked);
            }
        }
        Ok(())
    }

    /// Update OI from before/after effective positions
    fn update_oi_from_positions(
        &mut self,
        old_a: &I256, new_a: &I256,
        old_b: &I256, new_b: &I256,
    ) -> Result<()> {
        // For each account, compute OI delta per side
        self.update_single_oi(old_a, new_a)?;
        self.update_single_oi(old_b, new_b)?;

        // Check bounds
        if self.oi_eff_long_q > max_oi_side_q() {
            return Err(RiskError::Overflow);
        }
        if self.oi_eff_short_q > max_oi_side_q() {
            return Err(RiskError::Overflow);
        }

        Ok(())
    }

    fn update_single_oi(&mut self, old_eff: &I256, new_eff: &I256) -> Result<()> {
        // Remove old from its side
        if let Some(old_side) = side_of_i256(old_eff) {
            let abs_old = old_eff.abs_u256();
            let oi = self.get_oi_eff(old_side);
            let new_oi = oi.checked_sub(abs_old).ok_or(RiskError::CorruptState)?;
            self.set_oi_eff(old_side, new_oi);
        }
        // Add new to its side
        if let Some(new_side) = side_of_i256(new_eff) {
            let abs_new = new_eff.abs_u256();
            let oi = self.get_oi_eff(new_side);
            let new_oi = oi.checked_add(abs_new).ok_or(RiskError::Overflow)?;
            self.set_oi_eff(new_side, new_oi);
        }
        Ok(())
    }

    // ========================================================================
    // liquidate_at_oracle (spec §10.5 + §10.0)
    // ========================================================================

    /// Top-level liquidation: creates its own InstructionContext and finalizes resets.
    pub fn liquidate_at_oracle(
        &mut self,
        idx: u16,
        now_slot: u64,
        oracle_price: u64,
    ) -> Result<bool> {
        let mut ctx = InstructionContext::new();
        let result = self.liquidate_at_oracle_internal(idx, now_slot, oracle_price, &mut ctx)?;
        if result {
            // End-of-instruction resets (spec §10.5 steps 6-7)
            self.schedule_end_of_instruction_resets(&mut ctx)?;
            self.finalize_end_of_instruction_resets(&ctx);

            // Assert OI balance (spec §10.5)
            assert!(self.oi_eff_long_q == self.oi_eff_short_q, "OI_eff_long != OI_eff_short after liquidation");
        }
        Ok(result)
    }

    /// Internal liquidation routine: takes caller's shared InstructionContext.
    /// Does NOT call schedule/finalize resets — caller is responsible.
    fn liquidate_at_oracle_internal(
        &mut self,
        idx: u16,
        now_slot: u64,
        oracle_price: u64,
        ctx: &mut InstructionContext,
    ) -> Result<bool> {
        self.current_slot = now_slot;

        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Ok(false);
        }

        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        // Step 2: touch
        self.touch_account_full(idx as usize, oracle_price, now_slot)?;

        // Check position exists
        let old_eff = self.effective_pos_q(idx as usize);
        if old_eff.is_zero() {
            return Ok(false);
        }

        // Step 3: check liquidation eligibility (spec §9.3)
        if self.is_above_maintenance_margin(&self.accounts[idx as usize], idx as usize, oracle_price) {
            return Ok(false);
        }

        let liq_side = side_of_i256(&old_eff).unwrap();
        let abs_old_eff = old_eff.abs_u256();

        // Close entire position at oracle (bankruptcy liquidation per §10.0)
        let q_close_q = abs_old_eff;

        // Step 4: new effective position = 0
        self.attach_effective_position(idx as usize, I256::ZERO);

        // Step 6: settle losses from principal
        self.settle_losses(idx as usize);

        // Step 7: charge liquidation fee
        let notional_val = {
            let n = mul_div_floor_u256(q_close_q, U256::from_u128(oracle_price as u128), pos_scale_u256());
            u256_to_u128_sat(&n)
        };
        let liq_fee_raw = if notional_val > 0 && self.params.liquidation_fee_bps > 0 {
            let raw = mul_u128(notional_val, self.params.liquidation_fee_bps as u128);
            (raw + 9999) / 10_000
        } else {
            0
        };
        let liq_fee = core::cmp::min(liq_fee_raw, self.params.liquidation_fee_cap.get());
        self.charge_fee_safe(idx as usize, liq_fee)?;

        // Step 8: determine deficit D
        let eff_post = self.effective_pos_q(idx as usize);
        let d = if eff_post.is_zero() && self.accounts[idx as usize].pnl.is_negative() {
            self.accounts[idx as usize].pnl.abs_u256()
        } else {
            U256::ZERO
        };

        // Step 9: enqueue ADL
        if !q_close_q.is_zero() || !d.is_zero() {
            self.enqueue_adl(ctx, liq_side, q_close_q, d)?;
        }

        // Step 10: if D > 0, set_pnl(i, 0)
        if !d.is_zero() {
            self.set_pnl(idx as usize, I256::ZERO);
        }

        self.lifetime_liquidations = self.lifetime_liquidations.saturating_add(1);

        Ok(true)
    }

    // ========================================================================
    // keeper_crank (spec §10.6)
    // ========================================================================

    pub fn keeper_crank(
        &mut self,
        caller_idx: u16,
        now_slot: u64,
        oracle_price: u64,
        funding_rate_bps_per_slot: i64,
    ) -> Result<CrankOutcome> {
        if oracle_price == 0 || oracle_price > MAX_ORACLE_PRICE {
            return Err(RiskError::Overflow);
        }

        self.current_slot = now_slot;

        // Step 1: initialize instruction context (spec §10.6)
        let mut ctx = InstructionContext::new();

        // Accrue market state using stored rate (anti-retroactivity)
        self.accrue_market_to(now_slot, oracle_price)?;

        // Set new rate for next interval
        self.set_funding_rate_for_next_interval(funding_rate_bps_per_slot);

        let advanced = now_slot > self.last_crank_slot;
        if advanced {
            self.last_crank_slot = now_slot;
        }

        // Caller maintenance settle with 50% discount
        let (slots_forgiven, caller_settle_ok) = if (caller_idx as usize) < MAX_ACCOUNTS
            && self.is_used(caller_idx as usize)
        {
            let last_fee = self.accounts[caller_idx as usize].last_fee_slot;
            let dt = now_slot.saturating_sub(last_fee);
            let forgive = dt / 2;
            if forgive > 0 && dt > 0 {
                self.accounts[caller_idx as usize].last_fee_slot = last_fee.saturating_add(forgive);
            }
            self.settle_maintenance_fee_internal(caller_idx as usize, now_slot)?;
            (forgive, true)
        } else {
            (0, true)
        };

        // Process up to ACCOUNTS_PER_CRANK accounts
        let mut num_liquidations: u32 = 0;
        let num_liq_errors: u16 = 0;
        let mut sweep_complete = false;
        let mut accounts_processed: u16 = 0;
        let mut liq_budget = LIQ_BUDGET_PER_CRANK;

        let mut idx = self.crank_cursor as usize;
        let mut slots_scanned: usize = 0;

        while accounts_processed < ACCOUNTS_PER_CRANK && slots_scanned < MAX_ACCOUNTS {
            // If a pending reset has been triggered, stop live-OI-dependent work
            // immediately — go straight to end-of-instruction reset handling.
            if ctx.pending_reset_long || ctx.pending_reset_short {
                break;
            }

            slots_scanned += 1;

            let block = idx >> 6;
            let bit = idx & 63;
            let is_occupied = (self.used[block] & (1u64 << bit)) != 0;

            if is_occupied {
                accounts_processed += 1;

                // Touch account — propagate errors to trigger transaction rollback
                // rather than committing half-mutated state.
                self.touch_account_full(idx, oracle_price, now_slot)?;

                // Liquidation — uses internal routine sharing crank's ctx.
                // Errors must propagate: liquidate_at_oracle_internal mutates
                // state before downstream calls, so swallowing an error would
                // commit corrupted state (broken OI invariant).
                if liq_budget > 0 && !ctx.pending_reset_long && !ctx.pending_reset_short {
                    let eff = self.effective_pos_q(idx);
                    if !eff.is_zero() {
                        if !self.is_above_maintenance_margin(&self.accounts[idx], idx, oracle_price) {
                            match self.liquidate_at_oracle_internal(idx as u16, now_slot, oracle_price, &mut ctx) {
                                Ok(true) => {
                                    num_liquidations += 1;
                                    liq_budget = liq_budget.saturating_sub(1);
                                }
                                Ok(false) => {}
                                Err(e) => {
                                    return Err(e);
                                }
                            }
                        }
                    }
                }
            }

            idx = (idx + 1) & ACCOUNT_IDX_MASK;

            if idx == self.sweep_start_idx as usize && slots_scanned > 0 {
                sweep_complete = true;
                break;
            }
        }

        self.crank_cursor = idx as u16;

        if sweep_complete {
            self.last_full_sweep_completed_slot = now_slot;
            self.last_full_sweep_start_slot = now_slot;
            self.sweep_start_idx = self.crank_cursor;
        }

        let num_gc_closed = self.garbage_collect_dust();

        // Steps 3-4: end-of-instruction resets (spec §10.6)
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx);

        Ok(CrankOutcome {
            advanced,
            slots_forgiven,
            caller_settle_ok,
            force_realize_needed: false,
            panic_needed: false,
            num_liquidations,
            num_liq_errors,
            num_gc_closed,
            last_cursor: self.crank_cursor,
            sweep_complete,
        })
    }

    // ========================================================================
    // close_account
    // ========================================================================

    pub fn close_account(&mut self, idx: u16, now_slot: u64, oracle_price: u64) -> Result<u128> {
        self.current_slot = now_slot;

        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::AccountNotFound);
        }

        let mut ctx = InstructionContext::new();

        self.touch_account_full(idx as usize, oracle_price, now_slot)?;

        // Position must be zero
        let eff = self.effective_pos_q(idx as usize);
        if !eff.is_zero() {
            return Err(RiskError::Undercollateralized);
        }

        // Forgive fee debt
        if self.accounts[idx as usize].fee_credits.is_negative() {
            self.accounts[idx as usize].fee_credits = I128::ZERO;
        }

        // PnL must be zero
        if self.accounts[idx as usize].pnl.is_positive() {
            return Err(RiskError::PnlNotWarmedUp);
        }
        if self.accounts[idx as usize].pnl.is_negative() {
            return Err(RiskError::Undercollateralized);
        }

        let capital = self.accounts[idx as usize].capital;

        if capital > self.vault {
            return Err(RiskError::InsufficientBalance);
        }
        self.vault = self.vault - capital;
        self.set_capital(idx as usize, 0);

        // End-of-instruction resets before freeing
        self.schedule_end_of_instruction_resets(&mut ctx)?;
        self.finalize_end_of_instruction_resets(&ctx);

        self.free_slot(idx);

        Ok(capital.get())
    }

    // ========================================================================
    // Garbage collection
    // ========================================================================

    pub fn garbage_collect_dust(&mut self) -> u32 {
        let mut to_free: [u16; GC_CLOSE_BUDGET as usize] = [0; GC_CLOSE_BUDGET as usize];
        let mut num_to_free = 0usize;

        let max_scan = (ACCOUNTS_PER_CRANK as usize).min(MAX_ACCOUNTS);
        let start = self.gc_cursor as usize;

        for offset in 0..max_scan {
            if num_to_free >= GC_CLOSE_BUDGET as usize {
                break;
            }

            let idx = (start + offset) & ACCOUNT_IDX_MASK;
            let block = idx >> 6;
            let bit = idx & 63;
            if (self.used[block] & (1u64 << bit)) == 0 {
                continue;
            }

            // Never GC LP accounts
            if self.accounts[idx].is_lp() {
                continue;
            }

            // Best-effort fee settle (GC is non-critical; skip on error)
            if self.settle_maintenance_fee_internal(idx, self.current_slot).is_err() {
                continue;
            }

            // Dust predicate: zero position basis, zero capital, zero reserved, non-positive pnl
            let account = &self.accounts[idx];
            if !account.position_basis_q.is_zero() {
                continue;
            }
            if !account.capital.is_zero() {
                continue;
            }
            if !account.reserved_pnl.is_zero() {
                continue;
            }
            if account.pnl.is_positive() {
                continue;
            }

            // Write off negative PnL
            if self.accounts[idx].pnl.is_negative() {
                let loss = self.accounts[idx].pnl.abs_u256();
                self.absorb_protocol_loss(loss);
                self.set_pnl(idx, I256::ZERO);
            }

            to_free[num_to_free] = idx as u16;
            num_to_free += 1;
        }

        self.gc_cursor = ((start + max_scan) & ACCOUNT_IDX_MASK) as u16;

        for i in 0..num_to_free {
            self.free_slot(to_free[i]);
        }

        num_to_free as u32
    }

    // ========================================================================
    // Crank freshness
    // ========================================================================

    pub fn require_fresh_crank(&self, now_slot: u64) -> Result<()> {
        if now_slot.saturating_sub(self.last_crank_slot) > self.max_crank_staleness_slots {
            return Err(RiskError::Unauthorized);
        }
        Ok(())
    }

    pub fn require_recent_full_sweep(&self, now_slot: u64) -> Result<()> {
        if now_slot.saturating_sub(self.last_full_sweep_start_slot) > self.max_crank_staleness_slots {
            return Err(RiskError::Unauthorized);
        }
        Ok(())
    }

    // ========================================================================
    // Insurance fund operations
    // ========================================================================

    pub fn top_up_insurance_fund(&mut self, amount: u128) -> Result<bool> {
        self.vault = U128::new(add_u128(self.vault.get(), amount));
        self.insurance_fund.balance = U128::new(add_u128(self.insurance_fund.balance.get(), amount));
        Ok(self.insurance_fund.balance.get() > self.insurance_floor)
    }

    pub fn set_insurance_floor(&mut self, floor: u128) {
        self.insurance_floor = floor;
    }

    // ========================================================================
    // Fee credits
    // ========================================================================

    pub fn deposit_fee_credits(&mut self, idx: u16, amount: u128, now_slot: u64) -> Result<()> {
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::Unauthorized);
        }
        if amount > i128::MAX as u128 {
            return Err(RiskError::Overflow);
        }
        self.current_slot = now_slot;
        self.vault = self.vault + amount;
        self.insurance_fund.balance = self.insurance_fund.balance + amount;
        self.insurance_fund.fee_revenue = self.insurance_fund.fee_revenue + amount;
        self.accounts[idx as usize].fee_credits = self.accounts[idx as usize]
            .fee_credits.saturating_add(amount as i128);
        Ok(())
    }

    #[cfg(any(test, feature = "test", kani))]
    pub fn add_fee_credits(&mut self, idx: u16, amount: u128) -> Result<()> {
        if idx as usize >= MAX_ACCOUNTS || !self.is_used(idx as usize) {
            return Err(RiskError::Unauthorized);
        }
        self.accounts[idx as usize].fee_credits = self.accounts[idx as usize]
            .fee_credits.saturating_add(amount as i128);
        Ok(())
    }

    // ========================================================================
    // Recompute aggregates (test helper)
    // ========================================================================

    pub fn recompute_aggregates(&mut self) {
        let mut c_tot = 0u128;
        let mut pnl_pos_tot = U256::ZERO;
        self.for_each_used(|_idx, account| {
            c_tot = c_tot.saturating_add(account.capital.get());
            if account.pnl.is_positive() {
                let pos = account.pnl.abs_u256();
                pnl_pos_tot = pnl_pos_tot.saturating_add(pos);
            }
        });
        self.c_tot = U128::new(c_tot);
        self.pnl_pos_tot = pnl_pos_tot;
    }

    // ========================================================================
    // Utilities
    // ========================================================================

    pub fn advance_slot(&mut self, slots: u64) {
        self.current_slot = self.current_slot.saturating_add(slots);
    }

    /// Count used accounts
    pub fn count_used(&self) -> u64 {
        let mut count = 0u64;
        self.for_each_used(|_, _| {
            count += 1;
        });
        count
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

/// Multiply a U256 by an I256 returning I256 (checked).
/// Computes U256 * I256 → I256 using the sign of the I256 operand.
fn checked_u256_mul_i256(a: U256, b: I256) -> Result<I256> {
    if a.is_zero() || b.is_zero() {
        return Ok(I256::ZERO);
    }

    let negative = b.is_negative();
    let abs_b = if negative {
        if b == I256::MIN {
            return Err(RiskError::Overflow);
        }
        b.abs_u256()
    } else {
        b.abs_u256()
    };

    let product = a.checked_mul(abs_b).ok_or(RiskError::Overflow)?;

    // Check if product fits in I256 (must be <= I256::MAX as U256)
    let max_pos = I256::MAX.abs_u256();
    if !negative {
        if product > max_pos {
            return Err(RiskError::Overflow);
        }
        Ok(I256::from_raw_u256_pub(product))
    } else {
        // For negative: product can be up to |I256::MIN| = 2^255.
        // Use try_negate_u256_to_i256 which correctly handles the 2^255 boundary
        // (from_raw_u256_pub would misinterpret 2^255 as I256::MIN, and
        // checked_neg on I256::MIN returns None — a false overflow).
        try_negate_u256_to_i256(product).ok_or(RiskError::Overflow)
    }
}

/// Compute trade PnL: floor_div_signed_conservative(size_q * price_diff, POS_SCALE)
/// Uses wide signed arithmetic.
fn compute_trade_pnl(size_q: I256, price_diff: I256) -> Result<I256> {
    if size_q.is_zero() || price_diff.is_zero() {
        return Ok(I256::ZERO);
    }

    // Determine sign of result
    let neg_size = size_q.is_negative();
    let neg_price = price_diff.is_negative();
    let result_negative = neg_size != neg_price;

    let abs_size = size_q.abs_u256();
    let abs_price = price_diff.abs_u256();

    // Compute |size_q * price_diff| / POS_SCALE using wide mul-div
    let ps = pos_scale_u256();

    if result_negative {
        // We want floor(size_q * price_diff / POS_SCALE) where the product is negative.
        // Use wide_signed_mul_div_floor with abs_basis and a negative k_diff.
        let neg_k = I256::from_raw_u256_pub(abs_price).checked_neg().ok_or(RiskError::Overflow)?;
        Ok(wide_signed_mul_div_floor(abs_size, neg_k, ps))
    } else {
        // Positive result
        let pos_k = I256::from_raw_u256_pub(abs_price);
        Ok(wide_signed_mul_div_floor(abs_size, pos_k, ps))
    }
}

// ============================================================================
// I256 extension for raw U256 conversion (public)
// ============================================================================

impl I256 {
    /// Create I256 from raw U256 bits (public wrapper).
    /// The caller must ensure the value is valid (high bit determines sign).
    pub fn from_raw_u256_pub(v: U256) -> Self {
        Self::from_raw_u256(v)
    }
}
