//! Percolator keeper library. The decision surface lives in
//! [`strategy::decide`]; the binary in `main.rs` glues RPC fetches +
//! tx execution around it.

pub mod strategy;

pub use strategy::{decide, Action, SlabSnapshot, LIQ_BOUNTY_BPS, MIN_BOUNTY_THRESHOLD};
