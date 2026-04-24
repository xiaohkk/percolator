//! Percolator keeper binary.
//!
//! Scaffold — the full RPC + signing loop lands with #19 once the frontend
//! client SDK stabilizes. For now we verify the decision path compiles and
//! print a usage hint.

use percolator_keeper::strategy;

#[tokio::main]
async fn main() {
    eprintln!("percolator-keeper — scaffold build");
    eprintln!();
    eprintln!("Decision fn: strategy::decide(&SlabSnapshot) -> Action");
    eprintln!(
        "Supported actions: {:?}",
        [
            strategy::Action::RefreshOracle,
            strategy::Action::CrankFunding,
            strategy::Action::CrankAdlReset,
            strategy::Action::CrankGc,
            strategy::Action::Skip,
        ]
    );
    eprintln!();
    eprintln!(
        "Full tick loop: keeper/src/tick.rs (lands with task #19, once the \
         TypeScript client SDK exposes a blessed signer flow)."
    );
}
