//! Oracle program errors. Map to `ProgramError::Custom(u32)` on the wire.

use solana_program::program_error::ProgramError;
use thiserror::Error;

#[derive(Error, Debug, Copy, Clone, Eq, PartialEq)]
pub enum OracleError {
    #[error("Instruction data could not be decoded")]
    InvalidInstructionData = 0,

    #[error("Feed account has unexpected size")]
    FeedSizeMismatch = 1,

    #[error("Feed account is already initialized")]
    FeedAlreadyInitialized = 2,

    #[error("Feed account is not yet initialized")]
    FeedNotInitialized = 3,

    #[error("Missing required account")]
    MissingAccount = 4,

    #[error("Account not writable")]
    AccountNotWritable = 5,

    #[error("Source account mismatch (pubkey or owner)")]
    WrongSourceAccount = 6,

    #[error("Source reserves/data indicate a stale or empty pool")]
    StaleSource = 7,

    #[error("Unknown source_kind discriminator")]
    InvalidSourceKind = 8,

    #[error("Graduation requires the source's `complete` flag to be set")]
    NotGraduated = 9,

    #[error("Feed is already graduated")]
    AlreadyGraduated = 10,

    #[error("ConvertSource rejected: feed must be graduated first")]
    ConvertRequiresGraduation = 11,

    #[error("Arithmetic overflow in price computation")]
    Overflow = 12,

    #[error("Invalid risk parameters (e.g., bin_step out of range)")]
    InvalidParameters = 13,
}

impl From<OracleError> for ProgramError {
    fn from(e: OracleError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
