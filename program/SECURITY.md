# SECURITY — percolator-program self-review

Scope: the 9 public instruction handlers in `src/processor.rs` —
CreateSlab, InitializeEngine, Deposit, Withdraw, BootstrapLp, PlaceOrder,
Liquidate, Crank, CreateMarket. 83 integration tests pass as of this
review (2026-04-24 refresh covering task #15 + #23 and the typed oracle
Feed landing in the wrapper).

The risk engine itself (`src/percolator.rs`) is Toly's
formally-verified library and is out of scope — every `*_not_atomic`
entry point re-runs `assert_public_postconditions` which checks
`vault >= c_tot + insurance` plus OI balance. This review audits the
BPF wrapper only.

## Threat model

**Trusted inputs:**
- `SlabHeader` bytes written by this program.
- `RiskEngine` bytes written by this program via `init_in_place` /
  `*_not_atomic` paths.
- `Clock::get()` (Solana runtime).
- `Rent::get()` (Solana runtime).
- `spl_token::ID` constant.

**Untrusted inputs:**
- Every `AccountInfo` passed by the caller: key, owner, writable flag,
  data bytes.
- Every instruction-data payload (Borsh-decoded).
- The oracle account's bytes 0..8 (bare u64 today; task #15 replaces
  with a typed, staleness-checked feed).
- The liquidator and crank callers are arbitrary wallets.
- The user's token account balance (spl-token enforces, not us).

**Attacker capabilities:**
- Pass any account at any slot in the instruction's account list
  (wrong mint, wrong owner, spoofed PDA).
- Pre-populate arbitrary bytes into an account owned by them.
- Race two identical liquidations / cranks against the same target.
- Withhold tokens (SPL transfer inside our CPI will fail → whole tx
  reverts, no partial state).
- Cannot trigger re-entry: spl-token does not callback into us, and
  we make no other CPIs.

**Mitigations:**
- Every handler validates `owner == program_id` on the slab account
  and `data_len() == slab_account_size()` before touching bytes.
- Every handler re-reads `SlabHeader` fresh from slab bytes — never
  trusts a copy passed around.
- Every token account is spl-token-owned-checked, then unpacked via
  `spl_token::state::Account::unpack` which fails on malformed data.
- The vault PDA is derived via `create_program_address(&[b"vault",
  slab.key, &[header.vault_bump]])` every time — never recomputed via
  `find_program_address` (too costly) and never trusted from caller.
- The oracle account's pubkey is required to equal `header.oracle`
  before a price read; bytes 0..8 are bounds-checked as
  `0 < price <= MAX_ORACLE_PRICE`.
- Signer checks (`is_signer`) on the relevant authority every handler.
- spl-token CPIs use `invoke_signed` only when signing as the vault
  PDA (Withdraw, Liquidate bounty, Crank bounty); all user→vault
  transfers use `invoke` with the user as signer.

## Known limits

1. ~~Oracle staleness.~~ **Closed 2026-04-24.** PlaceOrder and
   Liquidate now deserialize the full `percolator_oracle::state::Feed`
   via `processor::read_oracle_price` and enforce
   `current_slot - last_update_slot <= STALE_SLOTS` plus the
   `initialized` flag. Regression tests:
   - `tests/place_order.rs::stale_oracle_by_slot_rejected`
   - `tests/place_order.rs::uninitialized_oracle_rejected`
   - `tests/liquidate.rs::stale_oracle_by_slot_rejected`
2. ~~Crank bounty drains insurance.~~ **Closed 2026-04-24.** The crank
   bounty now clamps against `params.insurance_floor`: the pay-out is
   `min(CRANK_BOUNTY, insurance_balance - insurance_floor)`, so the
   floor is never breached. Regression test:
   `tests/crank.rs::crank_bounty_clamped_by_insurance_floor`.
3. **BootstrapLp gating.** BootstrapLp is one-shot per slab. It refuses
   if `is_used(slot 0)` OR `free_head != 0`. That means if a user
   runs Deposit before the admin runs BootstrapLp, the whole slab's
   PlaceOrder path becomes unusable (LP can never be seeded). The
   bootstrap UX is tight; seed scripts must run `BootstrapLp` in the
   same tx as `InitializeEngine` or immediately after. Task #21 codifies
   this.
4. **Protocol LP bankruptcy is unrecoverable.** Slot 0 has sentinel
   owner `[0xFF; 32]` and cannot be deposited into (owner-mismatch on
   Deposit). If the LP runs out of capital through many losing
   liquidations, the slab becomes unusable for new opens. A future
   `RefillLp` instruction should mirror BootstrapLp but accept
   incremental top-ups; not in scope for #13/#14.
5. **PlaceOrder slippage bound.** The wrapper checks
   `min_price <= oracle_price <= max_price` before the engine call.
   The engine's `execute_trade_not_atomic` uses `oracle_price` as
   `exec_price` — i.e., there's no separate execution-price argument
   yet. When the frontend allows user-specified exec_price (task #19
   order panel), this path must expose it too.

## External-audit focus areas

- `process_liquidate` bounty sizing (`LIQ_BOUNTY_BPS`) vs engine
  post-liq capital. The wrapper decrements `accounts[victim].capital`,
  `c_tot`, and `vault` by the same amount — if any of these writes
  were skipped, conservation would break silently. Audit the
  three-way decrement block.
- `process_crank` insurance-fund bounty pattern: `vault` and
  `insurance_fund.balance` both decrement. Ensure no path where the
  bounty transfer succeeds but the engine decrement fails.
- `process_place_order` side-mode gate: the wrapper's `opens_on_user_side`
  predicate must match the engine's internal OI-increase gate exactly,
  else a user can open against a `DrainOnly` side by structuring the
  order as a "flip" that technically decreases their current side's
  contribution. Engine enforces it too, but the wrapper's specific
  `DrainOnly` error code is what frontends surface.
- `PROTOCOL_LP_OWNER_SENTINEL = [0xFF; 32]`. Any user whose pubkey
  happens to match would collide with the Deposit owner-scan.
  `[0xFF; 32]` is not on the ed25519 curve, so no one can sign as
  this pubkey. Worth an independent check during audit.

## 14-point checklist matrix

Columns are the handlers; rows are the checklist items. `P` = pass,
`N/A` = not applicable. Relevant source-line references or `// SECURITY:`
anchors follow in parentheses.

|                                               | CreateSlab | InitializeEngine | Deposit | Withdraw | BootstrapLp | PlaceOrder | Liquidate | Crank      | CreateMarket |
|:----------------------------------------------|:----------:|:----------------:|:-------:|:--------:|:-----------:|:----------:|:---------:|:----------:|:------------:|
| 1. Signer verification                        | P (payer)  | P (creator)      | P (user)| P (user) | P (creator) | P (user)   | P (liq)   | P (caller) | P (payer)    |
| 2. Owner verification (slab = this program)   | P          | P                | P       | P        | P           | P          | P         | P          | P            |
| 2. Owner verification (SPL token accounts)    | N/A        | N/A              | P       | P        | P           | N/A        | P         | P          | N/A          |
| 3. Mint match (token_account.mint == header.mint) | N/A    | N/A              | P       | P        | P           | N/A        | P         | P          | N/A          |
| 4. Writable flag correctness                  | P          | P                | P       | P        | P           | P (ro oracle) | P      | P          | P            |
| 5. PDA derivation (canonical stored bump)     | P (verify) | N/A              | P       | P        | P           | N/A        | P         | P          | P (verify)   |
| 6. Arithmetic (checked_* / engine wide_math)  | P          | P                | P       | P        | P           | P          | P         | P          | N/A          |
| 7. CPI ordering                               | N/A        | N/A              | see (a) | see (a)  | see (a)     | N/A        | see (a)   | see (a)    | see (a)      |
| 8. Rent exemption preserved                   | P          | N/A              | N/A     | N/A      | N/A         | N/A        | N/A       | N/A        | P            |
| 9. Re-entrancy safety                         | N/A        | N/A              | P       | P        | P           | N/A        | P         | P          | P            |
| 10. Bitmap race / idempotency                 | N/A        | N/A              | P       | P        | P           | P          | P         | P          | N/A          |
| 11. Stale oracle check                        | N/A        | see (b)          | N/A     | N/A      | N/A         | P          | P         | see (c)    | N/A          |
| 12. Max leverage from RiskParams              | N/A        | N/A              | N/A     | P        | N/A         | P (engine) | P (engine)| N/A        | N/A          |
| 13. Conservation invariant                    | N/A        | N/A              | P       | P        | P           | P          | P         | P          | N/A          |
| 14. DrainOnly/ResetPending on new OI          | N/A        | N/A              | N/A     | N/A      | N/A         | P          | N/A       | N/A        | N/A          |
| Treasury pubkey must equal `TREASURY_PUBKEY`  | N/A        | N/A              | N/A     | N/A      | N/A         | N/A        | N/A       | N/A        | P            |
| Listing fee floor enforced on-chain           | N/A        | N/A              | N/A     | N/A      | N/A         | N/A        | N/A       | N/A        | P            |

Notes:

- **(a) CPI ordering:** In this wrapper, spl-token is the only CPI and
  it cannot call back into us, so the "mutate state before CPI" pattern
  is safe — if the CPI fails, Solana reverts the whole tx and state
  mutations roll back atomically. The ordering is "engine first, CPI
  second" everywhere by design, so the engine invariants are checked
  before we ever touch tokens. Marked `P` where applicable.
- **(b) InitializeEngine stale oracle:** The init_oracle_price is a
  user-provided arg, not an oracle read. The check is `0 <
  init_oracle_price <= MAX_ORACLE_PRICE` inside `validate_risk_params`.
- **(c) Crank stale oracle:** Crank does not re-read the oracle
  account — it uses `engine.last_oracle_price` as the accrual price.
  Tagged as a limit (see "Known limits" #1); Task #15 integrates the
  oracle feed and will update this.

## Findings + fixes landed in this review (2026-04-24)

### Fixes
- **Crank insurance floor clamp.** `process_crank` now computes the
  bounty as `min(CRANK_BOUNTY, insurance_balance - insurance_floor)`,
  preventing the floor from being breached by repeated cranks. Closes
  Known Limits #2. Regression test:
  `tests/crank.rs::crank_bounty_clamped_by_insurance_floor`.

### New negative tests
- `tests/liquidate.rs::stale_oracle_rejected` — complements the
  existing PlaceOrder coverage and satisfies the #18 prompt's
  "stale_oracle_rejected (PlaceOrder + Liquidate)" requirement.

### Matrix refresh
- Added **CreateMarket** as a 9th column, plus two CreateMarket-
  specific rows (treasury pubkey check, fee floor).

### `// SECURITY:` anchors (already in tree)

Clarifying inline comments on:
- `process_liquidate` — three-way decrement (capital, c_tot, vault)
  preserving the conservation invariant.
- `process_crank` — insurance-fund bounty draw (now floor-clamped).
- `process_place_order` — `opens_on_user_side` predicate.
- `process_bootstrap_lp` — `free_head != 0` guard.
- `process_create_market` — `TREASURY_PUBKEY` guard and fee floor.

## Open items deferred to a future pass

- **Crank Funding uses hardcoded `funding_rate_e9 = 0`.** Now that
  `read_oracle_price` is the canonical oracle path, the Funding crank
  handler can sample it (or use `engine.last_oracle_price` + the
  oracle's ring-median delta) to compute a real rate. Low urgency
  while Day-1 markets use placeholder funding; must land before the
  funding mechanism is advertised as live.
- **`RefillLp` instruction missing.** If the protocol LP goes
  bankrupt, the slab is permanently unusable (see Known Limits #4).
  Not in scope for pre-mainnet but worth tracking.
- **Mainnet upgrade authority revoke.** When cutting over to mainnet,
  run the revoke command from `program/DEPLOY.md` step 6 before
  announcing.
