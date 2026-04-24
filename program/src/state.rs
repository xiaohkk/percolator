//! On-chain account layouts.
//!
//! A slab account is laid out as:
//!
//! ```text
//! +----------------------+  offset 0
//! |     SlabHeader       |   wrapper-owned bookkeeping (mint, oracle, etc.)
//! +----------------------+  offset size_of::<SlabHeader>()
//! |     RiskEngine       |   raw bytes of the frozen engine struct
//! +----------------------+  offset header + size_of::<RiskEngine>()
//! ```
//!
//! The `RiskEngine` region is zero-initialized on `CreateSlab`. A later
//! `InitializeEngine` instruction will fill it in using the engine's
//! constructor (out of scope for this commit).

use core::mem::size_of;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::error::PercolatorError;

/// Wrapper-owned bookkeeping prepended to every slab account.
///
/// `#[repr(C)]` with explicit padding so the layout is byte-stable across
/// compilers and matches what the test asserts.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SlabHeader {
    /// SPL mint the slab tracks. Not consulted by the engine; the wrapper
    /// uses it to route deposits/withdrawals to the right token vault.
    pub mint: Pubkey,
    /// Oracle adapter account. Again, wrapper-only: the engine consumes a
    /// `u64` price, not an account.
    pub oracle: Pubkey,
    /// Who paid for this slab's rent and is authorized to run admin ops.
    pub creator: Pubkey,
    /// PDA bump if the slab account is a PDA. 0 when it's a plain keypair
    /// account.
    pub bump: u8,
    /// 0 = engine region is raw zeros (post-CreateSlab only),
    /// 1 = `InitializeEngine` has populated the engine region.
    /// Used as an idempotency flag to refuse double-init.
    pub initialized: u8,
    /// PDA bump for the slab's vault token account
    /// (seeds = `[b"vault", slab.pubkey]`). Stored so the on-chain
    /// `signer_seeds` path doesn't re-grind `find_program_address`.
    pub vault_bump: u8,
    /// Origin tag (task #23). 0 = seeded (admin CreateSlab), 1 = open /
    /// paid listing (CreateMarket). Non-normative for the engine; the
    /// frontend uses it to render origin badges on `/markets`.
    pub origin: u8,
    /// Explicit tail padding so `size_of::<SlabHeader>()` is a multiple of 8
    /// and we don't rely on compiler-inserted padding for account layout.
    pub _pad: [u8; 4],
}

impl SlabHeader {
    pub const LEN: usize = size_of::<Self>();

    pub fn new(
        mint: Pubkey,
        oracle: Pubkey,
        creator: Pubkey,
        bump: u8,
        vault_bump: u8,
    ) -> Self {
        Self {
            mint,
            oracle,
            creator,
            bump,
            initialized: 0,
            vault_bump,
            origin: ORIGIN_SEEDED,
            _pad: [0; 4],
        }
    }

    /// Serialize into the first `LEN` bytes of an account's data buffer.
    pub fn write_into(&self, dst: &mut [u8]) -> Result<(), ProgramError> {
        if dst.len() < Self::LEN {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        dst[0..32].copy_from_slice(self.mint.as_ref());
        dst[32..64].copy_from_slice(self.oracle.as_ref());
        dst[64..96].copy_from_slice(self.creator.as_ref());
        dst[96] = self.bump;
        dst[97] = self.initialized;
        dst[98] = self.vault_bump;
        dst[99] = self.origin;
        dst[100..104].copy_from_slice(&self._pad);
        Ok(())
    }

    /// Read from the first `LEN` bytes of an account's data buffer.
    pub fn read_from(src: &[u8]) -> Result<Self, ProgramError> {
        if src.len() < Self::LEN {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        let mut mint = [0u8; 32];
        mint.copy_from_slice(&src[0..32]);
        let mut oracle = [0u8; 32];
        oracle.copy_from_slice(&src[32..64]);
        let mut creator = [0u8; 32];
        creator.copy_from_slice(&src[64..96]);
        let bump = src[96];
        let initialized = src[97];
        let vault_bump = src[98];
        let origin = src[99];
        let mut pad = [0u8; 4];
        pad.copy_from_slice(&src[100..104]);
        Ok(Self {
            mint: Pubkey::new_from_array(mint),
            oracle: Pubkey::new_from_array(oracle),
            creator: Pubkey::new_from_array(creator),
            bump,
            initialized,
            vault_bump,
            origin,
            _pad: pad,
        })
    }

    /// Whether `InitializeEngine` has populated the engine region.
    pub fn is_initialized(&self) -> bool {
        self.initialized != 0
    }
}

/// PDA seed prefix for a slab's vault token account.
pub const VAULT_SEED: &[u8] = b"vault";

/// Origin codes stored in `SlabHeader.origin` (task #23).
/// 0 = created by admin `CreateSlab` (the Day-1 seeded top memes).
/// 1 = created by permissionless `CreateMarket` (the paid listing flow).
pub const ORIGIN_SEEDED: u8 = 0;
pub const ORIGIN_OPEN: u8 = 1;

/// Canonical PDA for a slab's vault token account. Convenience helper;
/// on-chain paths should prefer `create_program_address(&[VAULT_SEED,
/// slab.key.as_ref(), &[stored_bump]], program_id)` with the bump already
/// stored in `SlabHeader.vault_bump` to avoid re-grinding.
pub fn find_vault_pda(slab: &Pubkey, program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, slab.as_ref()], program_id)
}

/// Full on-chain byte size of a slab account: header + engine region.
pub const fn slab_account_size() -> usize {
    SlabHeader::LEN + size_of::<percolator::RiskEngine>()
}

/// Byte length of just the engine region (tail of the slab buffer).
pub const fn engine_region_size() -> usize {
    size_of::<percolator::RiskEngine>()
}

/// Offset at which the engine bytes begin inside a slab account.
pub const ENGINE_OFFSET: usize = SlabHeader::LEN;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_is_104_bytes() {
        // 3 * 32 pubkeys = 96, + 1 bump + 7 pad = 104.
        assert_eq!(SlabHeader::LEN, 104);
    }

    #[test]
    fn header_roundtrip() {
        let mut h = SlabHeader::new(
            Pubkey::new_from_array([1u8; 32]),
            Pubkey::new_from_array([2u8; 32]),
            Pubkey::new_from_array([3u8; 32]),
            254,
            253,
        );
        h.initialized = 1;
        h.origin = ORIGIN_OPEN;
        let mut buf = vec![0u8; SlabHeader::LEN];
        h.write_into(&mut buf).unwrap();
        let read = SlabHeader::read_from(&buf).unwrap();
        assert_eq!(h, read);
        assert!(read.is_initialized());
        assert_eq!(read.vault_bump, 253);
        assert_eq!(read.origin, ORIGIN_OPEN);
    }

    #[test]
    fn fresh_header_defaults_to_seeded_origin() {
        let h = SlabHeader::new(
            Pubkey::new_from_array([0u8; 32]),
            Pubkey::new_from_array([0u8; 32]),
            Pubkey::new_from_array([0u8; 32]),
            0,
            0,
        );
        assert_eq!(h.origin, ORIGIN_SEEDED);
    }

    #[test]
    fn fresh_header_is_not_initialized() {
        let h = SlabHeader::new(
            Pubkey::new_from_array([0u8; 32]),
            Pubkey::new_from_array([0u8; 32]),
            Pubkey::new_from_array([0u8; 32]),
            0,
            0,
        );
        assert!(!h.is_initialized());
    }

    #[test]
    fn slab_size_is_header_plus_engine() {
        assert_eq!(
            slab_account_size(),
            SlabHeader::LEN + core::mem::size_of::<percolator::RiskEngine>()
        );
    }
}
