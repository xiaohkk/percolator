# percolator-program

Solana BPF wrapper around the frozen `percolator` crate (Toly's
formally-verified perpetual-DEX risk engine).

## Layout

```
program/
├── Cargo.toml            # crate-type = ["cdylib", "lib"]
├── src/
│   ├── lib.rs            # entrypoint!(process_instruction)
│   ├── instruction.rs    # PercolatorInstruction + borsh codecs
│   ├── processor.rs      # dispatcher + CreateSlab
│   ├── state.rs          # SlabHeader + slab size helpers
│   └── error.rs          # PercolatorError -> ProgramError::Custom
└── tests/
    └── create_slab.rs    # solana-program-test integration tests
```

## Slab sizing

This crate enables the parent crate's `compact` feature, which sets
`MAX_ACCOUNTS = 256` at compile time. This is a minimal, additive change
to the parent crate (one new `cfg` branch alongside the existing `test`
and `kani` gates) and leaves the proven math untouched.

| Feature | MAX_ACCOUNTS | `sizeof(RiskEngine)` | Slab total | Rent per slab |
| --- | --- | --- | --- | --- |
| default (library) | 4096 | 1,590,544 bytes (1.52 MiB) | 1,590,648 bytes | ~11.07 SOL |
| **`compact` (this crate)** | **256** | **100,144 bytes (97.8 KiB)** | **100,248 bytes** | **~0.70 SOL** |
| `test` (library tests) | 64 | n/a | n/a | n/a |
| `kani` (proofs) | 4 | n/a | n/a | n/a |

256 accounts per market is plenty for a memecoin perp (most markets will
not cross 50 active accounts). Rent dropped 15x makes creator-paid slab
allocation realistic.

If capacity ever needs to grow, add a new parent feature (e.g.
`medium = MAX_ACCOUNTS=1024`) and flip this crate's dependency.

## Allocation pattern

`RiskEngine` is too large to allocate from inside the program:
`MAX_PERMITTED_DATA_INCREASE` caps CPI-driven account growth at 10 KiB per
call. Clients MUST pre-allocate the slab account in the same transaction,
before invoking `CreateSlab`:

```rust
let alloc_ix = system_instruction::create_account(
    payer, slab, rent_exempt_lamports, slab_account_size() as u64, program_id,
);
let create_ix = /* PercolatorInstruction::CreateSlab */;
Transaction::new_with_payer(&[alloc_ix, create_ix], Some(payer))
```

The program validates on entry that the slab is already sized, owned, and
zeroed, then writes the header.

## What's implemented

| Instruction  | Status              |
| ------------ | ------------------- |
| CreateSlab   | live                |
| Deposit      | returns NotImpl (1) |
| Withdraw     | returns NotImpl (1) |
| PlaceOrder   | returns NotImpl (1) |
| Liquidate    | returns NotImpl (1) |
| Crank        | returns NotImpl (1) |

`CreateSlab` intentionally does *not* call any engine init function. The
engine region is left zeroed; a future `InitializeEngine` instruction will
populate it using the engine's own constructor.

## Build

```
# host check
cargo check -p percolator-program

# unit tests (instruction codec, header layout)
cargo test -p percolator-program --lib

# integration tests (BanksClient end-to-end)
cargo test -p percolator-program --test create_slab

# SBF artifact (~62 KiB at target/deploy/percolator_program.so)
cargo-build-sbf --manifest-path program/Cargo.toml
```
