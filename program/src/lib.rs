//! Percolator Solana program.
//!
//! Thin BPF wrapper around the `percolator` crate (Toly's formally-verified
//! perpetual-DEX risk engine). The parent crate is `no_std` and knows nothing
//! about Solana; this crate owns the on-chain account layout, instruction
//! decoding, and dispatcher.
//!
//! Today, only `CreateSlab` is wired end-to-end. Every other instruction is
//! routed to `PercolatorError::NotImplemented` so the binary is honest about
//! what's live.

pub mod error;
pub mod instruction;
pub mod processor;
pub mod state;

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey,
};

// Only the macro is gated by `no-entrypoint`. Dependent crates (keeper,
// integration tests) that enable `no-entrypoint` still need the
// `process_instruction` symbol for `processor!()` in `solana-program-test`.
#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    processor::Processor::process(program_id, accounts, instruction_data)
}
