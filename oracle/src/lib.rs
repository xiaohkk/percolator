//! Percolator oracle adapter.
//!
//! One program + one feed account per (mint, source). The Percolator program
//! reads bytes 0..8 of a feed account as the mark price — this crate places
//! `price_lamports_per_token` at offset 0 so that remains backward compatible.

pub mod error;
pub mod instruction;
pub mod processor;
pub mod state;

pub use error::OracleError;
pub use state::{ring_median, Feed, SourceKind, RING_LEN, STALE_SLOTS};

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey,
};

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    processor::Processor::process(program_id, accounts, instruction_data)
}
