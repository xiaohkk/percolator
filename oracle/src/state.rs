//! Feed account layout.
//!
//! One feed per (mint, source). Stored in a dedicated account; the Percolator
//! program reads bytes 0..8 of the same account as the mark price (temporary
//! until the frontend parses the whole struct). The first field is therefore
//! `price_lamports_per_token` so that bytes 0..8 == price.

use core::mem::size_of;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::error::OracleError;

/// Ring buffer depth for PumpSwap + other AMM sources — median of the last
/// `RING_LEN` updates is the published price. 30 is the spec value.
pub const RING_LEN: usize = 30;

/// Staleness threshold consumers enforce: if `current_slot - last_update_slot
/// > STALE_SLOTS`, the feed is considered stale and must be refreshed.
pub const STALE_SLOTS: u64 = 150;

/// Source kinds. Encoded as a `u8` in `Feed.source_kind`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// pump.fun bonding curve. Byte offsets:
    ///   virtual_sol_reserves   @ 0x08 (u64 LE)
    ///   virtual_token_reserves @ 0x10 (u64 LE)
    PumpBonding = 0,
    /// PumpSwap AMM pool. Two u64 reserves; see source layout below.
    PumpSwap = 1,
    /// Raydium CP (constant-product) pool.
    RaydiumCpmm = 2,
    /// Meteora DLMM. `active_bin_id` (i32) + `bin_step` (u16) + price math.
    MeteoraDlmm = 3,
}

impl SourceKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::PumpBonding),
            1 => Some(Self::PumpSwap),
            2 => Some(Self::RaydiumCpmm),
            3 => Some(Self::MeteoraDlmm),
            _ => None,
        }
    }
}

/// The feed account.
///
/// `#[repr(C)]` + explicit padding so on-chain byte layout is stable.
/// Field order is tuned so `price_lamports_per_token` lives at offset 0 —
/// the Percolator program's legacy bytes-0..8 read continues to work.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Feed {
    /// Published mark price in lamports-per-token. MUST live at offset 0.
    pub price_lamports_per_token: u64,
    /// Slot at which `price_lamports_per_token` was last written.
    pub last_update_slot: u64,
    /// The token this feed quotes.
    pub mint: Pubkey,
    /// The source account the feed reads from. Identity-checked on every
    /// Update.
    pub source: Pubkey,
    /// `SourceKind` as u8.
    pub source_kind: u8,
    /// 0 = not graduated (bonding curve live), 1 = graduated (moved to AMM).
    /// Once 1, bonding-curve reads are rejected; ConvertSource repoints.
    pub graduated: u8,
    /// Has `InitializeFeed` run? Idempotency flag.
    pub initialized: u8,
    /// Next index in `ring_buffer` to overwrite.
    pub ring_idx: u8,
    /// Explicit padding so the struct is 8-byte aligned before `ring_buffer`.
    pub _pad: [u8; 4],
    /// Most recent `RING_LEN` observed instantaneous prices (lamports per
    /// token). `price_lamports_per_token` is the median of this buffer.
    pub ring_buffer: [u64; RING_LEN],
}

impl Feed {
    pub const LEN: usize = size_of::<Self>();

    pub fn write_into(&self, dst: &mut [u8]) -> Result<(), ProgramError> {
        if dst.len() < Self::LEN {
            return Err(OracleError::FeedSizeMismatch.into());
        }
        // SAFETY: `Feed` is `#[repr(C)]` with stable layout; this is a
        // byte-for-byte memcpy of a plain-old-data struct.
        let src = unsafe {
            core::slice::from_raw_parts(self as *const Self as *const u8, Self::LEN)
        };
        dst[..Self::LEN].copy_from_slice(src);
        Ok(())
    }

    pub fn read_from(src: &[u8]) -> Result<Self, ProgramError> {
        if src.len() < Self::LEN {
            return Err(OracleError::FeedSizeMismatch.into());
        }
        // SAFETY: same as above. `Feed` has no invalid bit patterns (all
        // fields are primitive u8/u64/i64 and `[u8; 32]` inside `Pubkey`).
        let feed_ptr = src.as_ptr() as *const Self;
        Ok(unsafe { core::ptr::read_unaligned(feed_ptr) })
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized != 0
    }

    pub fn is_graduated(&self) -> bool {
        self.graduated != 0
    }

    pub fn source_kind_enum(&self) -> Option<SourceKind> {
        SourceKind::from_u8(self.source_kind)
    }
}

/// Median of the ring buffer. Non-zero entries only — an uninitialized
/// slot (price == 0) is treated as absent. Returns `None` on an empty ring.
///
/// Simple insertion sort into a stack buffer; `RING_LEN` is bounded so the
/// N^2 cost is negligible.
pub fn ring_median(ring: &[u64; RING_LEN]) -> Option<u64> {
    let mut buf = [0u64; RING_LEN];
    let mut n = 0usize;
    for &v in ring.iter() {
        if v != 0 {
            buf[n] = v;
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    let slice = &mut buf[..n];
    // Insertion sort.
    for i in 1..n {
        let mut j = i;
        while j > 0 && slice[j - 1] > slice[j] {
            slice.swap(j - 1, j);
            j -= 1;
        }
    }
    Some(if n % 2 == 1 {
        slice[n / 2]
    } else {
        // Even length: lower median (floor). Avoid averaging to prevent
        // overflow on huge prices.
        slice[n / 2 - 1]
    })
}
