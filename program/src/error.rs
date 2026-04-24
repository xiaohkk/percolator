//! Program-level errors.
//!
//! These map to a `ProgramError::Custom(u32)` on the wire. Keep the
//! discriminants stable; clients will decode them by number.

use solana_program::program_error::ProgramError;
use thiserror::Error;

#[derive(Error, Debug, Copy, Clone, Eq, PartialEq)]
pub enum PercolatorError {
    #[error("Instruction not yet implemented")]
    NotImplemented = 0,

    #[error("Instruction data could not be decoded")]
    InvalidInstructionData = 1,

    #[error("Slab account is already initialized")]
    SlabAlreadyInitialized = 2,

    #[error("Slab account has unexpected size")]
    SlabSizeMismatch = 3,

    #[error("Missing required account")]
    MissingAccount = 4,

    #[error("Account not writable")]
    AccountNotWritable = 5,

    #[error("Signer required but not provided")]
    MissingSigner = 6,

    #[error("System program account mismatch")]
    InvalidSystemProgram = 7,

    #[error("Slab engine region already initialized")]
    AlreadyInitialized = 8,

    #[error("Slab header not yet written; run CreateSlab first")]
    SlabNotInitialized = 9,

    #[error("RiskParams failed validation")]
    InvalidRiskParams = 10,

    #[error("Signer is not the slab creator on record")]
    WrongSigner = 11,

    #[error("Mint does not match the slab's recorded mint")]
    WrongMint = 12,

    #[error("Vault token account does not match the derived PDA")]
    VaultPdaMismatch = 13,

    #[error("Token account owner does not match expected")]
    TokenAccountWrongOwner = 14,

    #[error("Token account mint does not match expected")]
    TokenAccountWrongMint = 15,

    #[error("Amount must be non-zero")]
    ZeroAmount = 16,

    #[error("Token program account mismatch")]
    InvalidTokenProgram = 17,

    #[error("Slab is at capacity; no free account slot")]
    SlabFull = 18,

    #[error("Engine rejected the operation")]
    EngineError = 19,

    #[error("Oracle price is stale or zero")]
    StaleOracle = 20,

    #[error("Oracle price is outside the slippage bounds")]
    SlippageExceeded = 21,

    #[error("Side is in DrainOnly or ResetPending mode; new OI not allowed")]
    DrainOnly = 22,

    #[error("Invalid side discriminator; expected 0 (long) or 1 (short)")]
    InvalidSide = 23,

    #[error("Protocol LP (slot 0) has not been bootstrapped yet")]
    LpNotBootstrapped = 24,

    #[error("Protocol LP is already bootstrapped")]
    LpAlreadyBootstrapped = 25,

    #[error("Zero-size order")]
    ZeroSize = 26,

    #[error("Account is above maintenance margin and cannot be liquidated")]
    AccountHealthy = 27,

    #[error("Victim slot index is out of range or unused")]
    InvalidVictimSlot = 28,

    #[error("Victim slot targets the protocol LP; not liquidatable")]
    ProtocolLpNotLiquidatable = 29,

    #[error("Invalid Crank kind discriminator")]
    InvalidCrankKind = 30,

    #[error("Crank had no work to do at this slot")]
    NothingToDo = 31,

    #[error("Treasury account pubkey does not match TREASURY_PUBKEY")]
    WrongTreasury = 32,

    #[error("CreateMarket fee below MIN_MARKET_CREATION_FEE_LAMPORTS floor")]
    ListingFeeTooLow = 33,
}

impl From<PercolatorError> for ProgramError {
    fn from(e: PercolatorError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
