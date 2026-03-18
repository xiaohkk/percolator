
# Risk Engine Spec (Source of Truth) — v11.11

**Combined Single-Document Native 128-bit Revision (Keeper-Current-State / Fee-Sweep-Seniority / Maintenance-Fee-Neutrality Edition)**

**Design:** Protected Principal + Junior Profit Claims + Lazy A/K Side Indices (Native 128-bit Base-10 Scaling)  
**Status:** implementation source-of-truth (normative language: MUST / MUST NOT / SHOULD / MAY)  
**Scope:** perpetual DEX risk engine for a single quote-token vault.  
**Goal:** preserve oracle-manipulation resistance, conservation, bounded insolvency handling, and liveness while supporting lazy ADL across the opposing open-interest side without global scans, without canonical-order dependencies, and without sequential prefix requirements for user settlement.

This is a single combined spec. It supersedes prior delta-style revisions by restating the full current design in one document, explicitly scaled for native 128-bit high-throughput VM execution.

## Change summary from v11.10

This revision fixes the remaining real non-minor issues from the latest consistency pass and tightens the normative body around keeper behavior, fee-debt sweep ordering, and optional maintenance-fee designs.

1. **Keeper account actions are now explicitly current-state gated.** `keeper_crank` may not perform liquidation decisions, standalone warmup conversion, or standalone fee-debt extraction on an account unless that account has already been brought to current state with `touch_account_full(i, oracle_price, now_slot)` in the same instruction (or the action is explicitly defined safe on a stored-flat account).

2. **The generic fee-debt sweep rule is now consistent with loss seniority.** §7.5 now states that fee debt must be swept as soon as newly available capital is no longer senior-encumbered by already-realized trading losses on the same local state. This preserves the intended `deposit` / trade / liquidation ordering while still forbidding cross-instruction deferral.

3. **Optional recurring maintenance fees are now protocol-neutral by construction.** The spec now forbids realizing maintenance fees by mutating `K_side`, `PNL_i`, or `PNL_pos_tot`, and requires any position-dependent recurring fee design to realize only into `I` and/or `fee_credits_i` through a dedicated lazy fee accumulator or a formally equivalent method.

4. **Maintenance-fee realization is now explicitly bounded.** If a recurring maintenance-fee realization would exceed `MAX_PROTOCOL_FEE_ABS` or the permitted one-step `fee_credits_i` write range, the implementation must split the interval or realization into bounded internal chunks rather than overflow or fail unpredictably.

5. **Keeper pseudocode and required tests are now aligned with the normative body.** The new text explicitly covers keeper current-state gating and maintenance-fee neutrality / boundedness in both the normative sections and the minimum test suite.


## 0. Security goals (normative)

The engine MUST provide the following properties.

1. **Protected principal for flat accounts:** An account with effective position `0` MUST NOT have its protected principal directly reduced by another account's insolvency.

2. **Explicit open-position ADL eligibility:** Accounts with open positions MAY be subject to deterministic protocol ADL if they are on the eligible opposing side of a bankrupt liquidation. ADL MUST operate through explicit protocol state, not hidden execution.

3. **Oracle manipulation safety (within warmup window `T`):** Profits created by short-lived oracle distortion MUST NOT be withdrawable as principal immediately; they are time-gated by warmup and economically capped by system backing.

4. **Profit-first haircuts:** When the system is undercollateralized, haircuts MUST apply to junior profit claims before any protected principal of flat accounts is impacted.

5. **Conservation:** The engine MUST NOT create withdrawable claims exceeding vault tokens, except for explicitly bounded rounding slack.

6. **Liveness:** The engine MUST NOT require `OI == 0`, manual admin recovery, a global scan, or reconciliation of an unrelated prefix of accounts before a user can safely settle, withdraw, trade, or liquidate.

7. **No zombie poisoning:** Non-interacting accounts MUST NOT indefinitely pin `PNL_pos_tot` and collapse the haircut ratio for all users; touched accounts MUST make warmup progress.

8. **Funding / mark / ADL exactness under laziness:** Any economic quantity whose correct value depends on the position held over an interval MUST be represented through the A/K side-index mechanism or a formally equivalent event-segmented method. Integer rounding MUST NOT mint positive aggregate claims.

9. **No hidden protocol MM:** The protocol MUST NOT secretly internalize user flow against an undisclosed residual inventory.

10. **Defined recovery from precision stress:** The engine MUST define deterministic recovery when side precision is exhausted. It MUST NOT rely on assertion failure, silent overflow, or permanent `DrainOnly` states.

11. **No sequential quantity dependency:** Same-epoch account settlement MUST be fully local. It MAY depend on the account's own stored basis and current global side state, but MUST NOT require a canonical-order prefix or global carry cursor.

12. **Protocol-fee neutrality:** Explicit protocol fees MUST either be collected into `I` immediately or tracked as account-local fee debt. They MUST NOT be socialized through `h`, and unpaid explicit fees MUST NOT inflate bankruptcy deficit `D`.

13. **Synthetic liquidation price integrity:** A synthetic liquidation close MUST execute at the current oracle mark with zero execution-price slippage. Any liquidation penalty MUST be represented only by explicit fee state.

14. **Loss seniority over explicit fees:** When a trade or non-bankruptcy liquidation realizes losses for an account, those losses are senior to explicit protocol fees. The protocol MUST NOT extract a fee from capital that is economically owed first to a winning counterparty.

15. **Instruction-final funding anti-retroactivity:** If an instruction mutates any funding-rate input, the stored next-interval funding rate `r_last` MUST correspond to the instruction's final post-reset state, not any intermediate state.

16. **Deterministic overflow handling:** Any arithmetic condition that is not proven unreachable by the spec's numeric bounds MUST have a deterministic fail-safe or bounded fallback path. Silent wrap, unchecked panic, or undefined truncation are forbidden.

**Atomic execution model (normative):** Every top-level external instruction defined in §10 MUST be atomic. If any required precondition, checked-arithmetic guard, or conservative-failure condition fails, the instruction MUST roll back all state mutations performed since that instruction began.

## 1. Types, units, scaling, and arithmetic requirements

### 1.1 Amounts

- `u128` unsigned amounts are denominated in quote-token atomic units, positive PnL aggregates, OI, fixed-point position magnitudes, and bounded fee amounts.

- `i128` signed amounts represent realized PnL, K-space liabilities, and fee-credit balances.

- All persistent state MUST fit natively into 128-bit boundaries. Emulated wide multi-limb integers (for example `u256` / `i256`) are permitted only within transient intermediate math steps.

### 1.2 Prices and internal positions

- `POS_SCALE = 1_000_000` (6 decimal places of position precision).

- `price: u64` is quote-token atomic units per `1` base. There is no separate `PRICE_SCALE`.

- All external price inputs, including `oracle_price`, `exec_price`, and any stored funding price sample, MUST satisfy `0 < price <= MAX_ORACLE_PRICE`.

- Internally the engine stores position bases as signed fixed-point base quantities:

  - `basis_pos_q_i: i128`, with units `(base * POS_SCALE)`.

- The displayed base quantity is `basis_pos_q_i / POS_SCALE` only when the account is attached to the current side state. During same-epoch lazy settlement, the economically relevant quantity is the derived helper `effective_pos_q(i)`.

- Effective notional at oracle is:

  - `notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), price, POS_SCALE)`.

- Trade fees MUST use executed trade size, not account notional:

  - `trade_notional = mul_div_floor_u128(abs(size_q), exec_price, POS_SCALE)`.

- Any `execute_trade` instruction MUST enforce both `size_q <= MAX_TRADE_SIZE_Q` and `trade_notional <= MAX_ACCOUNT_NOTIONAL` before any signed cast or slippage multiplication that depends on `size_q`.

### 1.3 A/K scale

- `ADL_ONE = 1_000_000` (6 decimal places of fractional decay accuracy).

- `A_side` is dimensionless and scaled by `ADL_ONE`.

- `K_side` has units `(ADL scale) * (quote atomic units per 1 base)`.

### 1.4 Concrete normative bounds

The following bounds are normative and MUST be enforced.

- `V <= MAX_VAULT_TVL = 10_000_000_000_000_000`

- `0 < price <= MAX_ORACLE_PRICE = 1_000_000_000_000`

- `abs(basis_pos_q_i) <= MAX_POSITION_ABS_Q = 100_000_000_000_000`

- `abs(effective_pos_q(i)) <= MAX_POSITION_ABS_Q`

- `MAX_ACCOUNT_NOTIONAL = 100_000_000_000_000_000_000`; thus `trade_notional` and `Notional_i` MUST remain `<= MAX_ACCOUNT_NOTIONAL`

- `MAX_TRADE_SIZE_Q = 200_000_000_000_000`; every `execute_trade` input MUST satisfy `0 < size_q <= MAX_TRADE_SIZE_Q`

- `|funding_rate_bps_per_slot_last| <= MAX_ABS_FUNDING_BPS_PER_SLOT = 10_000`

- `MAX_FUNDING_DT = 65_535`

- `MAX_OI_SIDE_Q = 100_000_000_000_000`

- `MAX_ACTIVE_POSITIONS_PER_SIDE` MUST be a finite implementation-enforced bound on concurrently stored nonzero positions per side, and MUST satisfy `MAX_ACTIVE_POSITIONS_PER_SIDE <= MAX_MATERIALIZED_ACCOUNTS`.

- `MAX_MATERIALIZED_ACCOUNTS = 1_000_000`

- `0 <= trading_fee_bps <= MAX_TRADING_FEE_BPS = 10_000`

- `0 <= maintenance_bps <= initial_bps <= MAX_MARGIN_BPS = 10_000`

- `0 <= liquidation_fee_bps <= MAX_LIQUIDATION_FEE_BPS = 10_000`

- `0 <= min_liquidation_abs <= liquidation_fee_cap <= MAX_PROTOCOL_FEE_ABS = MAX_ACCOUNT_NOTIONAL`

- `0 <= I_floor <= MAX_VAULT_TVL`

- `MAX_ACCOUNT_POSITIVE_PNL = 100_000_000_000_000_000_000_000_000_000_000`

- `MAX_PNL_POS_TOT = MAX_MATERIALIZED_ACCOUNTS * MAX_ACCOUNT_POSITIVE_PNL = 100_000_000_000_000_000_000_000_000_000_000_000_000`

- `MIN_A_SIDE = 1_000` (side truncates into `DrainOnly` at 0.1% survival fraction).

- `A_side > 0` whenever `OI_eff_side > 0` and the side is still representable.

The following interpretation is normative for dust accounting.

- `stored_pos_count_side` MAY be used as a q-unit conservative term in phantom-dust accounting because each live stored position can contribute at most one additional q-unit from threshold crossing when a global `A_side` truncation occurs.

### 1.4.1 Time monotonicity and account freshness invariants

- Any top-level instruction or helper call that accepts `now_slot` MUST require both `now_slot >= current_slot` and `now_slot >= slot_last` before state mutation.

- Timed helpers MUST NOT rewind `current_slot`. If `accrue_market_to(now_slot, ...)` succeeds, it MUST leave `current_slot = now_slot`.

- Any account materialized inside an instruction that accepts `now_slot` MUST use `slot_anchor = now_slot` for both `w_start_i` and `last_fee_slot_i`.

- A newly materialized account MUST NOT inherit a stale global slot anchor and MUST NOT initialize `last_fee_slot_i = 0`.

- A newly materialized or re-zeroed account MUST have `a_basis_i = ADL_ONE`. The engine MUST NOT leave `a_basis_i = 0`.

### 1.5 Arithmetic requirements

The engine MUST satisfy all of the following.

1. All products involving `A_side`, `K_side`, `k_snap_i`, `basis_pos_q_i`, `effective_pos_q(i)`, `price`, funding deltas, mark deltas, fee quantities, or ADL deltas MUST use checked arithmetic unless the spec explicitly requires exact wide intermediate arithmetic instead.

2. `dt` inside `accrue_market_to` MUST be split into internal sub-steps with `dt <= MAX_FUNDING_DT`.

3. The conservation check `V >= C_tot + I` and any `Residual` computation MUST use checked `u128` addition for `C_tot + I`. Overflow is invariant violation.

4. Signed division with positive denominator MUST use the exact helper in §4.8.

5. Positive ceiling division MUST use the exact helper in §4.8.

6. Warmup-cap computation `w_slope_i * elapsed` MUST use `saturating_mul_u128_u64` or a formally equivalent min-preserving construction.

7. Every decrement of `stored_pos_count_*`, `stale_account_count_*`, or `phantom_dust_bound_*_q` MUST use checked subtraction. Underflow indicates corruption and MUST fail conservatively.

8. Every increment of `stored_pos_count_*`, `phantom_dust_bound_*_q`, or `materialized_account_count` MUST use checked addition. Overflow indicates corrupted capacity accounting and MUST fail conservatively.

9. In `accrue_market_to`, the funding contribution MUST delay division by `10_000` until after multiplication by `A_side`, and the receiver-side funding gain MUST be derived from the payer-side loss so rounding cannot mint positive aggregate claims.

10. `K_side` is cumulative across epochs. Implementations MUST use checked arithmetic and revert on persistent-state `i128` overflow.

11. The calculation of same-epoch or epoch-mismatch `pnl_delta` MUST evaluate the signed K-difference in an exact wide intermediate before multiplication and division. It MUST NOT perform unchecked `i128` subtraction first.

12. Every call site that computes `new_PNL = PNL_i + delta` or `new_fee_credits = fee_credits_i + delta` MUST use checked `i128` addition or subtraction before writing state.

13. Haircut paths `floor(PNL_pos_i * h_num / h_den)` and `floor(x * h_num / h_den)` MUST use exact wide multiply-divide floor because the intermediate product can exceed `u128::MAX` even though the final quotient is bounded.

14. The ADL quote-deficit path MUST compute the exact required K-index delta with a helper that performs exact wide `ceil(D * A_old * POS_SCALE / OI)` arithmetic and returns either an exact value or an `OverI128Magnitude` result. It MUST NOT use a helper bounded by `u128::MAX` for this step.

15. If an ADL K-index delta computation is not representable as an `i128` magnitude, or if the final signed addition `K_opp + delta_K_exact` would overflow `i128`, the engine MUST route the quote deficit through `absorb_protocol_loss(D)` and continue the quantity-socialization path without modifying `K_opp`.

16. `PNL_i` and `fee_credits_i` MUST be maintained in the closed interval `[i128::MIN + 1, i128::MAX]`. Any operation that would set either value to exactly `i128::MIN` is non-compliant and MUST fail conservatively.

17. Global A-truncation dust added in `enqueue_adl` MUST be accounted using checked arithmetic and the exact conservative bound from §5.6.

18. `set_pnl` MUST enforce both per-account positive-PnL bound and aggregate positive-PnL bound before mutation.

19. `materialized_account_count` MUST be bounded so that `MAX_MATERIALIZED_ACCOUNTS * MAX_ACCOUNT_POSITIVE_PNL <= MAX_PNL_POS_TOT <= u128::MAX`.

20. Explicit protocol-fee helpers MUST bound each charged `fee` by `MAX_PROTOCOL_FEE_ABS` and MUST NOT write any unpaid fee amount into `PNL_i`.

21. `accrue_market_to` MUST apply mark-to-market exactly once per invocation from the pre-invocation `P_last` to the final `oracle_price`. Funding is the only component that may be sub-stepped.

22. Any operation that changes funding-rate inputs MUST recompute and store `r_last` exactly once after the instruction's final post-reset state is known. Mid-instruction recomputation is forbidden.

23. `accrue_market_to` MUST apply funding only when the invocation snapshot has live effective OI on both sides. If either snapped side OI is zero, the funding adjustment for that invocation is exactly zero.

24. Any recurring maintenance-fee realization MUST be bounded or internally chunked so that:
   - every explicit fee amount passed to `charge_fee_to_insurance` is `<= MAX_PROTOCOL_FEE_ABS`,
   - every incremental `fee_credits_i` write uses checked `i128` arithmetic and cannot produce `i128::MIN`,
   - and no maintenance-fee realization path mutates `K_side`, `PNL_i`, or `PNL_pos_tot`.

### 1.5.1 Reference 128-bit boundary proof

By clamping constants to base-10 metrics, on-chain state fits natively in 128-bit registers without persistent-state truncation.

- **Same-epoch quantity numerator:** `MAX_POSITION_ABS_Q * ADL_ONE = 10^14 * 10^6 = 10^20` (fits natively in `u128`).

- **Max account-notional numerator:** `MAX_POSITION_ABS_Q * MAX_ORACLE_PRICE = 10^14 * 10^12 = 10^26` (fits natively in `u128` / `i128`).

- **Max trade-notional numerator under explicit trade-size bound:** `MAX_TRADE_SIZE_Q * MAX_ORACLE_PRICE = 2 × 10^14 * 10^12 = 2 × 10^26` (fits natively in `u128` / `i128`), while `trade_notional` itself remains bounded by the separate required check `trade_notional <= MAX_ACCOUNT_NOTIONAL`.

- **Trade-slippage numerator:** `MAX_TRADE_SIZE_Q * MAX_ORACLE_PRICE = 2 × 10^14 * 10^12 = 2 × 10^26` (fits natively in `i128`), with final realized slippage bounded by `2 × 10^20` before the separate `trade_notional <= MAX_ACCOUNT_NOTIONAL` gate.

- **Mark K-step:** `ADL_ONE * MAX_ORACLE_PRICE = 10^6 * 10^12 = 10^18` (fits natively in `i128`).

- **Funding raw term per bounded sub-step:** `MAX_ORACLE_PRICE * MAX_ABS_FUNDING_BPS_PER_SLOT * MAX_FUNDING_DT = 10^12 * 10^4 * 65535 ≈ 6.5535 × 10^20` (fits natively in `u128`).

- **Funding payer K-step:** `ADL_ONE * funding_term_raw / 10_000 ≈ 6.5535 × 10^22` (fits natively in `u128`).

- **Funding receiver numerator:** `delta_K_payer_abs * ADL_ONE ≈ 6.5535 × 10^28` (fits natively in `u128`).

- **Fee numerators:** `MAX_ACCOUNT_NOTIONAL * 10_000 = 10^20 * 10^4 = 10^24` (fits natively in `u128`).

- **A-side quantity-socialization product:** `ADL_ONE * MAX_OI_SIDE_Q = 10^6 * 10^14 = 10^20` (fits natively in `u128`).

- **K lifetime headroom:** even at extreme sustained boundaries, persistent-state `K_side` remains far below `i128::MAX` over any realistic system lifetime; checked arithmetic remains mandatory.

- **Wide transient paths only:** exact transient wide math is required only for:
  1. `pnl_delta` (because the exact signed K-difference and resulting numerator can exceed 128 bits before division),
  2. exact haircut multiply-divides,
  3. exact ADL `delta_K_abs` representability fallback.

- **Aggregate positive-PnL storage:** by construction, `materialized_account_count <= 10^6`, `PNL_i^+ <= 10^32`, so `PNL_pos_tot <= 10^38 < u128::MAX` and `PNL_pos_tot` cannot overflow if account materialization and `set_pnl` bounds are enforced.
- **Per-account positive-PnL headroom:** even at the extreme receiver-side funding bound of roughly `6.5535 × 10^24` quote atoms per bounded funding sub-step on the maximum position, reaching `MAX_ACCOUNT_POSITIVE_PNL = 10^32` requires roughly `1.5 × 10^7` maxed sub-steps (about `3 × 10^4` years at one-second slots). The per-account cap therefore exists to make aggregate storage exact, not to constrain any realistic operational horizon.


## 2. State model

### 2.1 Account state

For each account `i`, the engine stores at least:

- `C_i: u128` — protected principal.

- `PNL_i: i128` — realized PnL claim.

- `R_i: u128` — reserved positive PnL, with `0 <= R_i <= max(PNL_i, 0)`.

- `basis_pos_q_i: i128` — signed fixed-point base basis at the last explicit position mutation or forced zeroing. This is not necessarily the current effective quantity.

- `a_basis_i: u128` — side multiplier in effect when `basis_pos_q_i` was last explicitly attached.

- `k_snap_i: i128` — last realized `K_side` snapshot relevant to the current stored basis.

- `epoch_snap_i: u64` — side epoch in which the stored basis is defined.

- `fee_credits_i: i128`.

- `last_fee_slot_i: u64`.

- `w_start_i: u64`.

- `w_slope_i: u128`.

Fee-credit bound and exact debt definition:

- `fee_credits_i` MUST be initialized to `0`.

- The engine MUST maintain `-(i128::MAX) <= fee_credits_i <= i128::MAX` at all times. `fee_credits_i == i128::MIN` is forbidden.

- `FeeDebt_i = fee_debt_u128_checked(fee_credits_i)`.

- Any operation that would decrement `fee_credits_i` to exactly `i128::MIN` or below MUST fail conservatively.

### 2.1.1 Canonical zero-position defaults and account initialization

The engine MUST define a canonical zero-position account state.

For an account in canonical zero-position state:

- `basis_pos_q_i = 0`

- `a_basis_i = ADL_ONE`

- `k_snap_i = 0`

- `fee_credits_i` is unchanged unless the caller explicitly changes it

- `epoch_snap_i` is a caller-supplied zero-position epoch anchor

A helper that resets an account to zero position in a known side epoch `e` MUST set:

- `basis_pos_q_i = 0`

- `a_basis_i = ADL_ONE`

- `k_snap_i = 0`

- `epoch_snap_i = e`

When a new account is materialized, the canonical materialization helper MUST take a `slot_anchor: u64` and MUST initialize at minimum:

- `C_i = 0`, `PNL_i = 0`, `R_i = 0`, `basis_pos_q_i = 0`, `fee_credits_i = 0`

- `a_basis_i = ADL_ONE`

- `k_snap_i = 0`

- `epoch_snap_i = 0`

- `w_start_i = slot_anchor`

- `w_slope_i = 0`

- `last_fee_slot_i = slot_anchor`

The materialization helper MUST require `slot_anchor >= current_slot` and `slot_anchor >= slot_last`, and a timed top-level instruction MUST pass its own `now_slot` as `slot_anchor`.

A newly materialized account MUST be inserted only through a helper that increments `materialized_account_count` in checked arithmetic and enforces `materialized_account_count <= MAX_MATERIALIZED_ACCOUNTS`.

### 2.2 Global engine state

The engine stores at least:

- `V: u128`

- `I: u128`

- `I_floor: u128`

- `current_slot: u64`

- `P_last: u64`

- `slot_last: u64`

- `r_last: i64`

- `fund_px_last: u64`

- `A_long: u128`

- `A_short: u128`

- `K_long: i128`

- `K_short: i128`

- `epoch_long: u64`

- `epoch_short: u64`

- `K_epoch_start_long: i128`

- `K_epoch_start_short: i128`

- `OI_eff_long: u128`

- `OI_eff_short: u128`

- `mode_long ∈ {Normal, DrainOnly, ResetPending}`

- `mode_short ∈ {Normal, DrainOnly, ResetPending}`

- `stored_pos_count_long: u64`

- `stored_pos_count_short: u64`

- `stale_account_count_long: u64`

- `stale_account_count_short: u64`

- `phantom_dust_bound_long_q: u128`

- `phantom_dust_bound_short_q: u128`

- `C_tot: u128 = Σ C_i`

- `PNL_pos_tot: u128 = Σ max(PNL_i, 0)`

- `materialized_account_count: u64`

### 2.3 Initial state

Market initialization MUST take, at minimum, `init_slot`, `init_oracle_price`, and a configured `I_floor`.

At market initialization, the engine MUST set:

- `V = 0`

- `I = 0`

- `I_floor = configured I_floor`

- `C_tot = 0`

- `PNL_pos_tot = 0`

- `materialized_account_count = 0`

- `current_slot = init_slot`

- `slot_last = init_slot`

- `P_last = init_oracle_price`

- `fund_px_last = init_oracle_price`

- `r_last = initial_funding_rate`, where `initial_funding_rate` is computed from the just-initialized state; if the funding formula depends on skew or live OI, this MUST be `0` for an empty market

- `A_long = ADL_ONE`, `A_short = ADL_ONE`

- `K_long = 0`, `K_short = 0`

- `epoch_long = 0`, `epoch_short = 0`

- `K_epoch_start_long = 0`, `K_epoch_start_short = 0`

- `OI_eff_long = 0`, `OI_eff_short = 0`

- `mode_long = Normal`, `mode_short = Normal`

- `stored_pos_count_long = 0`, `stored_pos_count_short = 0`

- `stale_account_count_long = 0`, `stale_account_count_short = 0`

- `phantom_dust_bound_long_q = 0`, `phantom_dust_bound_short_q = 0`

### 2.4 Side modes

A side may be in one of three modes:

- `Normal`: ordinary operation.

- `DrainOnly`: the side is live but has decayed below the safe precision threshold; OI on that side may decrease but MUST NOT increase.

- `ResetPending`: the side has been fully drained and its prior epoch is awaiting stale-account reconciliation. During `ResetPending`, no operation may increase OI on that side.

### 2.5 `begin_full_drain_reset(side)`

The engine MUST provide a helper that begins a full-drain epoch rollover for one side. It MUST:

1. require `OI_eff_side == 0`

2. set `K_epoch_start_side = K_side`

3. increment `epoch_side` by exactly `1` using checked `u64` arithmetic

4. set `A_side = ADL_ONE`

5. set `stale_account_count_side = stored_pos_count_side`

6. set `phantom_dust_bound_side_q = 0`

7. set `mode_side = ResetPending`

### 2.6 `MIN_A_SIDE` is a live-side trigger, not a snapshot invariant

`MIN_A_SIDE` applies only to the current live `A_side` and triggers `DrainOnly`. It is not a lower bound on historical `a_basis_i`.

### 2.7 `finalize_side_reset(side)`

`finalize_side_reset(side)` MAY succeed only if all of the following hold:

1. `mode_side == ResetPending`

2. `OI_eff_side == 0`

3. `stale_account_count_side == 0`

4. `stored_pos_count_side == 0`

On success, the engine MUST set `mode_side = Normal`.

### 2.8 `maybe_finalize_ready_reset_sides_before_oi_increase()`

The engine MUST provide a helper that checks each side independently and, if all `finalize_side_reset(side)` preconditions already hold, immediately invokes `finalize_side_reset(side)`.

This helper MUST NOT begin a new reset, mutate `A_side`, `K_side`, `epoch_side`, `OI_eff_side`, or any account state. It may only transition an already-eligible clean-empty side from `ResetPending` to `Normal`.

## 3. Junior-profit solvency via the global haircut ratio

### 3.1 Residual backing available to junior profits

Define:

- `senior_sum = checked_add_u128(C_tot, I)`

- `Residual = max(0, V - senior_sum)`

`Residual` is the only backing for positive realized PnL that has not been converted into principal.

Invariant: the engine MUST maintain `V >= senior_sum` at all times.

### 3.2 Haircut ratio `h`

Let:

- if `PNL_pos_tot == 0`, define `h = 1`

- else:

  - `h_num = min(Residual, PNL_pos_tot)`

  - `h_den = PNL_pos_tot`

### 3.3 Effective positive PnL and net equity on current state

For account `i`:

- `PNL_pos_i = max(PNL_i, 0)`

- if `PNL_pos_tot == 0`, then `PNL_eff_pos_i = PNL_pos_i`

- else `PNL_eff_pos_i = wide_mul_div_floor_u128(PNL_pos_i, h_num, h_den)`

Define current-state equity:

- `Eq_real_raw_i = checked_add_i128(C_i as i128, min(PNL_i, 0))`

- `Eq_real_raw_i = checked_add_i128(Eq_real_raw_i, PNL_eff_pos_i as i128)`

- `Eq_real_i = max(0, Eq_real_raw_i)`

- `Eq_net_raw_i = checked_sub_i128(Eq_real_i, FeeDebt_i as i128)`

- `Eq_net_i = max(0, Eq_net_raw_i)`

All margin checks MUST use `Eq_net_i` on the **current touched state**.

### 3.4 Conservatism under pending A/K side effects

The engine computes `h` only over stored realized state. Therefore:

- pending positive mark / funding / ADL effects MUST NOT be withdrawable until touch,

- pending negative mark / funding / ADL effects MAY temporarily make `C_tot` / `PNL_pos_tot` conservative relative to a fully-cranked state,

- pending lazy ADL obligations MUST NOT be counted as backing in `Residual`.

### 3.5 Rounding and conservation

Because each `PNL_eff_pos_i` is floored independently:

- `Σ PNL_eff_pos_i <= h_num <= Residual`.

## 4. Canonical helpers

### 4.1 `checked_add_u128(a, b)`, `checked_add_u64(a, b)`, `checked_mul_u128(a, b)`

These helpers MUST either return the exact native result or signal overflow.

- `checked_add_u128(a, b)` returns the exact `u128` sum or fails.

- `checked_add_u64(a, b)` returns the exact `u64` sum or fails.

- `checked_mul_u128(a, b)` returns the exact `u128` product or fails.

### 4.2 `checked_add_i128(a, b)`, `checked_sub_i128(a, b)`, `checked_neg_i128(x)`

These helpers MUST return the exact signed result if it lies in `[i128::MIN + 1, i128::MAX]`; otherwise they MUST fail conservatively.

`checked_neg_i128(i128::MIN)` is forbidden.

### 4.3 `set_capital(i, new_C)`

When changing `C_i` from `old_C` to `new_C`, the engine MUST update `C_tot` by the signed delta in a checked manner and then set `C_i = new_C`.

### 4.4 `set_pnl(i, new_PNL)`

When changing `PNL_i` from `old` to `new`, the engine MUST:

1. require `new != i128::MIN`

2. if `new > 0`, require `(new as u128) <= MAX_ACCOUNT_POSITIVE_PNL`

3. let `old_pos = max(old, 0) as u128`

4. let `new_pos = max(new, 0) as u128`

5. if `new_pos > old_pos`, compute `candidate = checked_add_u128(PNL_pos_tot, new_pos - old_pos)` and require `candidate <= MAX_PNL_POS_TOT`

6. else compute `candidate = PNL_pos_tot - (old_pos - new_pos)` using checked subtraction

7. set `PNL_pos_tot = candidate`

8. set `PNL_i = new`

9. clamp `R_i := min(R_i, new_pos)`

All code paths that modify PnL MUST call `set_pnl`.

### 4.5 `set_position_basis_q(i, new_basis_pos_q)`

When changing stored `basis_pos_q_i` from `old` to `new`, the engine MUST update `stored_pos_count_long` and `stored_pos_count_short` exactly once using the sign flags of `old` and `new`, then write `basis_pos_q_i = new_basis_pos_q`.

For a single logical position change, `set_position_basis_q` MUST be called exactly once with the final target. Passing through an intermediate zero value is not permitted.

If the call would increase a side's stored nonzero-position count above `MAX_ACTIVE_POSITIONS_PER_SIDE`, the helper MUST fail conservatively before mutation.

### 4.6 `attach_effective_position(i, new_eff_pos_q)`

This helper MUST convert a current effective quantity into a new position basis at the current side state.

Preconditions:

- `abs(new_eff_pos_q) <= MAX_POSITION_ABS_Q`

- the caller has already enforced any side-mode gating and OI-cap checks required for the intended top-level operation

If the account currently has a nonzero same-epoch basis and this helper is about to discard that basis (by writing either `0` or a different nonzero basis), then the engine MUST first account for any orphaned unresolved same-epoch quantity remainder:

- let `s = side(basis_pos_q_i)`

- if `epoch_snap_i == epoch_s`, compute `rem = (abs(basis_pos_q_i) * A_s) mod a_basis_i` in exact `u128` arithmetic

- if `rem != 0`, invoke `inc_phantom_dust_bound(s)`

A caller MUST NOT use `attach_effective_position` as a no-op refresh. If `new_eff_pos_q` equals the account's current `effective_pos_q(i)` with the same sign, the helper SHOULD preserve the existing basis and snapshots rather than discard and recreate them.

If `new_eff_pos_q == 0`, it MUST:

- `set_position_basis_q(i, 0)`

- reset the account to canonical zero-position defaults anchored to the current epoch of the side that was just discarded, if any; otherwise anchor to `0`

If `new_eff_pos_q != 0`, it MUST:

- `set_position_basis_q(i, new_eff_pos_q)`

- `a_basis_i = A_side(new_eff_pos_q)`

- `k_snap_i = K_side(new_eff_pos_q)`

- `epoch_snap_i = epoch_side(new_eff_pos_q)`

### 4.7 Phantom-dust helpers

`inc_phantom_dust_bound(side)` MUST increment `phantom_dust_bound_side_q` by exactly `1` q-unit using checked addition.

`inc_phantom_dust_bound_by(side, amount_q)` MUST increment `phantom_dust_bound_side_q` by exactly `amount_q` q-units using checked addition.

### 4.8 Exact helper definitions (normative)

The engine MUST use the following exact helpers.

**Signed conservative floor division**

```text
floor_div_signed_conservative(n, d):
    require d > 0
    q = trunc_toward_zero(n / d)
    r = n % d
    if n < 0 and r != 0:
        return q - 1
    else:
        return q
```

**Positive checked ceiling division**

```text
ceil_div_positive_checked(n, d):
    require d > 0
    q = n / d
    r = n % d
    if r != 0:
        return q + 1
    else:
        return q
```

**Exact native multiply-divide floor for nonnegative inputs**

```text
mul_div_floor_u128(a, b, d):
    require d > 0
    compute exact native product p = a * b   // overflowing u128::MAX is forbidden
    return floor(p / d)
```

**Exact native multiply-divide ceil for nonnegative inputs**

```text
mul_div_ceil_u128(a, b, d):
    require d > 0
    compute exact native product p = a * b   // overflowing u128::MAX is forbidden
    return ceil(p / d)
```

**Exact wide multiply-divide floor for nonnegative inputs**

```text
wide_mul_div_floor_u128(a, b, d):
    require d > 0
    compute exact wide product p = a * b
    return floor(p / d)
```

**Exact wide signed multiply-divide floor from K-pair (for `pnl_delta` only)**

```text
wide_signed_mul_div_floor_from_k_pair(abs_basis_u128, k_now_i128, k_then_i128, den_u128):
    require den_u128 > 0
    d = (wide signed)k_now_i128 - (wide signed)k_then_i128
    p = abs_basis_u128 * abs(d)    // exact wide product
    q = floor(p / den_u128)
    r = p mod den_u128
    if d < 0:
        mag = q + 1 if r != 0 else q
        require mag <= i128::MAX
        return -(mag as i128)
    else:
        require q <= i128::MAX
        return q as i128
```

**Exact wide ADL quotient helper with representability fallback**

```text
wide_mul_div_ceil_u128_or_over_i128max(a, b, d):
    require d > 0
    q = ceil((wide)a * (wide)b / d)
    if q > i128::MAX:
        return OverI128Magnitude
    else:
        return Value(q as u128)
```

**Checked fee-debt conversion**

```text
fee_debt_u128_checked(fee_credits):
    require fee_credits != i128::MIN
    if fee_credits >= 0:
        return 0
    else:
        return (-fee_credits) as u128
```

**Saturating warmup-cap multiply**

```text
saturating_mul_u128_u64(a, b):
    if a == 0 or b == 0:
        return 0
    if a > u128::MAX / (b as u128):
        return u128::MAX
    else:
        return a * (b as u128)
```

### 4.9 `absorb_protocol_loss(loss)`

This helper is the normative accounting path for uncovered losses that are no longer attached to an open position.

Precondition: `loss > 0`.

Given `loss` as a `u128` quote amount:

1. `available_I = I.saturating_sub(I_floor)`

2. `pay_I = min(loss, available_I)`

3. `I := I - pay_I`

4. `loss_rem := loss - pay_I`

5. if `loss_rem > 0`, no additional decrement to `V` occurs. The uncovered loss is represented by junior undercollateralization through `h`.

### 4.10 `charge_fee_to_insurance(i, fee)`

This helper is the only normative path for charging explicit protocol fees such as trading fees or liquidation fees.

Preconditions:

- `fee <= MAX_PROTOCOL_FEE_ABS`

- the caller has already computed `fee` according to the relevant fee rule

The helper MUST:

1. `fee_paid = min(fee, C_i)`

2. `set_capital(i, C_i - fee_paid)`

3. `I = checked_add_u128(I, fee_paid)`

4. `fee_shortfall = fee - fee_paid`

5. if `fee_shortfall > 0`, compute `new_fee_credits = checked_sub_i128(fee_credits_i, fee_shortfall as i128)` and set `fee_credits_i = new_fee_credits`

Unpaid explicit fees are account-local fee debt. They MUST NOT be written into `PNL_i`, MUST NOT change `PNL_pos_tot`, and MUST NOT be included in bankruptcy deficit `D`.

### 4.11 `recompute_next_funding_rate_from_final_state(oracle_price)`

If the funding-rate formula depends on mutable engine state (for example skew, OI, utilization, side modes, oracle-related funding inputs, or explicit funding-configuration state), then after the instruction's final post-reset state is known the engine MUST recompute and store the next-interval `r_last` exactly once.

Funding-rate inputs MAY depend on market exposure, side modes, oracle-related funding inputs, and explicit funding-configuration state. They MUST NOT depend directly on `current_slot`, wall-clock time, passive passage of time, vault-only capital bookkeeping such as `V`, `C_tot`, `I`, account principal deposits / withdrawals, or account-local fee debt.

This helper MUST:

1. use the final post-reset state of the current top-level instruction

2. use the just-settled current `oracle_price` or the implementation's defined current funding price sample basis

3. write the resulting next-interval rate into `r_last`

4. never retroactively reprice slots already accrued by `accrue_market_to`

## 5. Unified A/K side-index mechanics

### 5.1 Eager-equivalent event law

For one side of the book, a single eager global event on absolute fixed-point position `q_q >= 0` and realized PnL `p` has the form:

- `q_q' = α q_q`

- `p' = p + β * q_q / POS_SCALE`

where:

- `α ∈ [0, 1]` is the surviving-position fraction,

- `β` is quote PnL per unit pre-event base position.

The cumulative side indices compose as:

- `A_new = A_old * α`

- `K_new = K_old + A_old * β`

### 5.2 `effective_pos_q(i)`

For an account `i` on side `s` with nonzero basis:

- if `epoch_snap_i != epoch_s`, then `effective_pos_q(i) = 0` for current-market risk purposes until the account is touched and zeroed

- else `effective_abs_pos_q(i) = floor(abs(basis_pos_q_i) * A_s / a_basis_i)`

- `effective_pos_q(i) = sign(basis_pos_q_i) * effective_abs_pos_q(i)`

### 5.3 `settle_side_effects(i)`

When touching account `i`:

1. If `basis_pos_q_i == 0`, return immediately.

2. Let `s = side(basis_pos_q_i)`.

3. If `epoch_snap_i == epoch_s` (same epoch):

   - compute `q_eff_new = floor(abs(basis_pos_q_i) * A_s / a_basis_i)` using checked arithmetic

   - compute `den = a_basis_i * POS_SCALE` using checked `u128` arithmetic

   - compute `pnl_delta = wide_signed_mul_div_floor_from_k_pair(abs(basis_pos_q_i), K_s, k_snap_i, den)`

   - compute `new_PNL = checked_add_i128(PNL_i, pnl_delta)`

   - `set_pnl(i, new_PNL)`

   - if `q_eff_new == 0`:

     - `inc_phantom_dust_bound(s)`

     - `set_position_basis_q(i, 0)`

     - reset the account to canonical zero-position defaults anchored to `epoch_s`

   - else:

     - do not change `basis_pos_q_i` or `a_basis_i`

     - set `k_snap_i = K_s`

     - set `epoch_snap_i = epoch_s`

4. Else (epoch mismatch):

   - require `mode_s == ResetPending`

   - require `checked_add_u64(epoch_snap_i, 1) == epoch_s`

   - compute `den = a_basis_i * POS_SCALE` using checked `u128` arithmetic

   - compute `pnl_delta = wide_signed_mul_div_floor_from_k_pair(abs(basis_pos_q_i), K_epoch_start_s, k_snap_i, den)`

   - compute `new_PNL = checked_add_i128(PNL_i, pnl_delta)`

   - `set_pnl(i, new_PNL)`

   - `set_position_basis_q(i, 0)`

   - decrement `stale_account_count_s` using checked subtraction

   - reset the account to canonical zero-position defaults anchored to `epoch_s`

### 5.4 `accrue_market_to(now_slot, oracle_price)`

Before any operation that depends on current market state, the engine MUST call `accrue_market_to(now_slot, oracle_price)`.

This helper MUST:

1. require `now_slot >= current_slot`

2. require `now_slot >= slot_last`

3. require `0 < oracle_price <= MAX_ORACLE_PRICE`

4. snapshot `OI_eff_long` and `OI_eff_short` at the start of the invocation; those OI values are fixed for all funding sub-steps in this invocation

5. if `now_slot == slot_last` and `oracle_price == P_last`:
   - set `current_slot = now_slot`
   - return

6. apply mark-to-market **exactly once** from the pre-invocation `P_last` to the final `oracle_price`:
   - let `delta_p = (oracle_price as i128) - (P_last as i128)`
   - if `delta_p != 0`:
     - if snapped `OI_eff_long > 0`, add `A_long * delta_p` to `K_long` using checked `i128` arithmetic
     - if snapped `OI_eff_short > 0`, add `-(A_short * delta_p)` to `K_short` using checked `i128` arithmetic

7. let `dt_rem = now_slot - slot_last`

8. while `dt_rem > 0`:
   - let `dt = min(dt_rem, MAX_FUNDING_DT)`
   - if `r_last != 0` **and** snapped `OI_eff_long > 0` **and** snapped `OI_eff_short > 0`:
     - `funding_term_raw = fund_px_last * abs(r_last) * dt`, computed natively in checked arithmetic
     - if `r_last > 0`, longs are payer and shorts are receiver
     - if `r_last < 0`, shorts are payer and longs are receiver
     - let `A_p = A_side(payer)` and `A_r = A_side(receiver)`
     - compute payer K-space loss first:
       - `delta_K_payer_abs = mul_div_ceil_u128(A_p, funding_term_raw, 10_000)`
     - derive receiver K-space gain from the payer loss:
       - `delta_K_receiver_abs = mul_div_floor_u128(delta_K_payer_abs, A_r, A_p)`
     - apply with checked persistent-state `i128` arithmetic:
       - `K_payer -= delta_K_payer_abs`
       - `K_receiver += delta_K_receiver_abs`
   - `dt_rem -= dt`

9. update `slot_last = now_slot`, `P_last = oracle_price`, `fund_px_last = oracle_price`, and `current_slot = now_slot`

Normative clarification:

- Step 6 is one-shot mark application for the whole invocation.

- Step 8 is the only sub-stepped component.

- If either snapped side OI is zero, funding is skipped for the entire invocation.

- An implementation MUST NOT re-apply the same `delta_p` once per funding sub-step.

### 5.5 Funding anti-retroactivity

In this source-of-truth spec, funding-rate inputs MAY depend on market state such as OI, skew, side modes, oracle-related funding inputs, and explicit funding-configuration state. They MUST NOT depend directly on `current_slot`, wall-clock time, passive passage of time, vault-only capital bookkeeping such as `V`, `C_tot`, `I`, account principal deposits / withdrawals, or account-local fee debt.

Before any operation that can change funding-rate inputs, the engine MUST:

1. call `accrue_market_to(now_slot, oracle_price)` using the currently stored `r_last`

2. apply the instruction's state changes

3. perform end-of-instruction reset scheduling / finalization if required

4. if funding-rate inputs changed, call `recompute_next_funding_rate_from_final_state(oracle_price)` exactly once using the instruction's final post-reset state

No top-level instruction may recompute `r_last` at an intermediate point and then overwrite or retain that mid-instruction value after final resets.

This requirement applies equally to instructions whose only funding-input mutation arises from end-of-instruction reset scheduling or finalization.

### 5.6 `enqueue_adl(ctx, liq_side, q_close_q, D)`

Suppose a bankrupt or non-bankruptcy synthetic liquidation from side `liq_side` removes `q_close_q >= 0` fixed-point base quantity from that side, and may additionally route uncovered deficit `D >= 0` as a `u128` quote amount.

For non-bankruptcy quantity socialization, `D = 0`.

For bankruptcy quantity socialization, `D` is the uncovered negative realized PnL remaining after the liquidated account's principal has been exhausted.

Preconditions:

- `opp = opposite(liq_side)`

- `ctx` is the current top-level instruction's reset-scheduling context

The engine MUST perform the following in order:

1. If `q_close_q > 0`, decrease the liquidated side OI:
   - `OI_eff_liq_side := OI_eff_liq_side - q_close_q` using checked subtraction.

2. Read `OI = OI_eff_opp` at this moment.

3. If `OI == 0`:
   - if `D > 0`, invoke `absorb_protocol_loss(D)`
   - return

4. If `OI > 0` and `stored_pos_count_opp == 0`:
   - require `q_close_q <= OI`
   - let `OI_post = OI - q_close_q`
   - if `D > 0`, invoke `absorb_protocol_loss(D)` and do not modify `K_opp`
   - set `OI_eff_opp := OI_post`
   - if `OI_post == 0`:
     - set `ctx.pending_reset_opp = true`
     - set `ctx.pending_reset_liq_side = true`
   - return

5. Else (`OI > 0` and `stored_pos_count_opp > 0`):
   - require `q_close_q <= OI`
   - let `A_old = A_opp`
   - let `OI_post = OI - q_close_q`

6. If `D > 0`:
   - let `adl_scale = checked_mul_u128(A_old, POS_SCALE)`
   - compute `delta_K_abs_result = wide_mul_div_ceil_u128_or_over_i128max(D, adl_scale, OI)`
   - if `delta_K_abs_result == OverI128Magnitude`, invoke `absorb_protocol_loss(D)` and do not modify `K_opp`
   - else let `delta_K_abs = value(delta_K_abs_result)`, `delta_K_exact = -(delta_K_abs as i128)`, and test whether `K_opp + delta_K_exact` fits in `i128`
   - if it fits, apply `K_opp := K_opp + delta_K_exact`
   - if it does not fit, invoke `absorb_protocol_loss(D)` instead and do not modify `K_opp`

7. If `OI_post == 0`:
   - set `OI_eff_opp := 0`
   - set `ctx.pending_reset_opp = true`
   - set `ctx.pending_reset_liq_side = true`
   - return

8. Compute the product natively:
   - `A_prod_exact = A_old * OI_post`

9. Compute natively:
   - `A_candidate = floor(A_prod_exact / OI)`
   - `A_trunc_rem = A_prod_exact mod OI`

10. If `A_candidate > 0`:
    - set `A_opp := A_candidate`
    - set `OI_eff_opp := OI_post`
    - only if `A_trunc_rem != 0`, account for global A-truncation dust:
      - let `N_opp = stored_pos_count_opp as u128`
      - let `global_a_dust_bound = checked_add_u128(N_opp, ceil_div_positive_checked(checked_add_u128(OI, N_opp), A_old))`
      - apply `inc_phantom_dust_bound_by(opp, global_a_dust_bound)`
    - if `A_opp < MIN_A_SIDE`, set `mode_opp = DrainOnly`
    - return

11. If `A_candidate == 0` while `OI_post > 0`, the side has exhausted representable quantity precision. The engine MUST enter a precision-exhaustion terminal drain:
    - set `OI_eff_opp := 0`
    - set `OI_eff_liq_side := 0`
    - set `ctx.pending_reset_opp = true`
    - set `ctx.pending_reset_liq_side = true`

Normative intent:

- Quantity socialization MUST never assert-fail due to `A_side` rounding to zero.

- Global A-truncation dust MUST be bounded in `phantom_dust_bound_opp_q` when and only when actual truncation occurs.

- Real quote deficits MUST NOT be written into `K_opp` when there are no opposing stored positions left to realize that K change.

- When an ADL event drains effective OI to zero on both sides, both sides MUST enter the reset lifecycle.

### 5.7 `schedule_end_of_instruction_resets(ctx)`

This helper MUST be called exactly once, after all explicit position mutations and snapshot attachments in each top-level external instruction.

It MUST perform the following in order.

#### 5.7.A Bilateral-empty dust clearance

If:

- `stored_pos_count_long == 0`, and

- `stored_pos_count_short == 0`,

then:

1. define `clear_bound_q = checked_add_u128(phantom_dust_bound_long_q, phantom_dust_bound_short_q)`

2. define `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0) or (phantom_dust_bound_short_q > 0)`

3. if `has_residual_clear_work`:
   - require `OI_eff_long == OI_eff_short`; otherwise fail conservatively
   - if `OI_eff_long <= clear_bound_q` and `OI_eff_short <= clear_bound_q`:
     - set `OI_eff_long = 0`
     - set `OI_eff_short = 0`
     - set `ctx.pending_reset_long = true`
     - set `ctx.pending_reset_short = true`
   - else fail conservatively

#### 5.7.B Unilateral-empty symmetric dust clearance

Else if:

- `stored_pos_count_long == 0`, and

- `stored_pos_count_short > 0`,

then:

1. define `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0)`

2. if `has_residual_clear_work`:
   - require `OI_eff_long == OI_eff_short`; otherwise fail conservatively
   - if `OI_eff_long <= phantom_dust_bound_long_q`:
     - set `OI_eff_long = 0`
     - set `OI_eff_short = 0`
     - set `ctx.pending_reset_long = true`
     - set `ctx.pending_reset_short = true`
   - else fail conservatively

#### 5.7.C Symmetric counterpart

Else if:

- `stored_pos_count_short == 0`, and

- `stored_pos_count_long > 0`,

then:

1. define `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_short_q > 0)`

2. if `has_residual_clear_work`:
   - require `OI_eff_long == OI_eff_short`; otherwise fail conservatively
   - if `OI_eff_short <= phantom_dust_bound_short_q`:
     - set `OI_eff_long = 0`
     - set `OI_eff_short = 0`
     - set `ctx.pending_reset_long = true`
     - set `ctx.pending_reset_short = true`
   - else fail conservatively

#### 5.7.D DrainOnly zero-OI reset scheduling

After the above dust-clear logic:

- if `mode_long == DrainOnly` and `OI_eff_long == 0`, set `ctx.pending_reset_long = true`

- if `mode_short == DrainOnly` and `OI_eff_short == 0`, set `ctx.pending_reset_short = true`

### 5.8 `finalize_end_of_instruction_resets(ctx)`

This helper MUST be called exactly once at the end of each top-level external instruction, after §5.7.

Once either `ctx.pending_reset_long` or `ctx.pending_reset_short` becomes true during a top-level external instruction, that instruction MUST NOT perform any additional account touches, liquidations, or explicit position mutations that rely on live authoritative OI. It MUST proceed directly to §§5.7–5.8 after completing any already-started local bookkeeping that does not read or mutate live side exposure.

It MUST, in order:

- if `ctx.pending_reset_long` and `mode_long != ResetPending`, invoke `begin_full_drain_reset(long)`

- if `ctx.pending_reset_short` and `mode_short != ResetPending`, invoke `begin_full_drain_reset(short)`

- if `mode_long == ResetPending` and `OI_eff_long == 0` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, invoke `finalize_side_reset(long)`

- if `mode_short == ResetPending` and `OI_eff_short == 0` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, invoke `finalize_side_reset(short)`

## 6. Warmup

### 6.1 Parameter

- `T = warmup_period_slots`.

- If `T == 0`, warmup is instantaneous.

### 6.2 Available gross positive PnL

- `AvailGross_i = max(PNL_i, 0) - R_i`.

### 6.3 Warmable gross amount

If `T == 0`, define:

- `WarmableGross_i = AvailGross_i`.

Otherwise let:

- `elapsed = current_slot - w_start_i`

- `cap = saturating_mul_u128_u64(w_slope_i, elapsed)`

Then:

- `WarmableGross_i = min(AvailGross_i, cap)`.

### 6.4 Warmup slope update rule

After any change that increases `AvailGross_i`:

- if `AvailGross_i == 0`, then `w_slope_i = 0`

- else if `T > 0`, then `w_slope_i = max(1, floor(AvailGross_i / T))`

- else (`T == 0`), then `w_slope_i = AvailGross_i`

- `w_start_i = current_slot`

### 6.5 Restart-on-new-profit rule via eager auto-conversion

When an operation increases `AvailGross_i`, the invoking routine MUST provide `old_warmable_i`, which is `WarmableGross_i` evaluated strictly before the profit-increasing event.

The engine MUST:

1. If `old_warmable_i > 0`, execute the profit-conversion logic of §7.4 substituting `x = old_warmable_i`.

2. If step 1 increased `C_i`, the invoking routine MUST immediately execute the fee-debt sweep of §7.5 before any subsequent step in the same top-level routine that may consume capital, assess margin, or absorb uncovered losses.

3. After step 1 (or immediately if `old_warmable_i == 0`), update the warmup slope per §6.4 using the new remaining `AvailGross_i`.

## 7. Loss settlement, uncovered loss resolution, profit conversion, and fee-debt sweep

### 7.1 Loss settlement from principal

If `PNL_i < 0`, the engine MUST immediately attempt to settle from principal:

1. require `PNL_i != i128::MIN`

2. `need = (-PNL_i) as u128`

3. `pay = min(need, C_i)`

4. apply:
   - `set_capital(i, C_i - pay)`
   - `new_PNL = checked_add_i128(PNL_i, pay as i128)`
   - `set_pnl(i, new_PNL)`

### 7.2 Open-position negative remainder

If after §7.1:

- `PNL_i < 0` and `effective_pos_q(i) != 0`,

then the account MUST NOT be silently zeroed. It remains liquidatable and must be resolved through liquidation / ADL.

### 7.3 Zero-position negative remainder

If after §7.1:

- `PNL_i < 0` and `effective_pos_q(i) == 0`,

then the engine MUST:

1. call `absorb_protocol_loss((-PNL_i) as u128)`

2. `set_pnl(i, 0)`

A capital-only instruction that does not call `settle_side_effects(i)` MAY invoke this path only when `basis_pos_q_i == 0`. It MUST NOT treat `effective_pos_q(i) == 0` arising from a stale or epoch-mismatched nonzero stored basis as a flat-account loss path.

### 7.4 Profit conversion

Let `x = WarmableGross_i`. If `x == 0`, do nothing.

Compute `y` using the pre-conversion haircut ratio:

- if `PNL_pos_tot == 0`, `y = x`

- else `y = wide_mul_div_floor_u128(x, h_num, h_den)`

Apply:

- `new_PNL = checked_sub_i128(PNL_i, x as i128)`

- `set_pnl(i, new_PNL)`

- `set_capital(i, checked_add_u128(C_i, y))`

Then handle the warmup schedule as follows:

- if `T == 0`, set `w_start_i = current_slot` and `w_slope_i = 0` if `AvailGross_i == 0` else `AvailGross_i`

- else if `AvailGross_i == 0`, set `w_slope_i = 0` and `w_start_i = current_slot`

- else:
  - set `w_start_i = current_slot`
  - preserve the existing `w_slope_i`

### 7.5 Fee-debt sweep after capital increase

After any operation that increases `C_i`, the enclosing routine MUST sweep fee debt as soon as that newly available capital is no longer senior-encumbered by already-realized trading losses on the same local state.

Normative ordering:

- if the enclosing routine already knows current-state realized trading losses that are payable from principal, those losses are senior and MUST be settled first via §7.1 (and, for allowed true-flat capital-only paths, §7.3) before this sweep consumes the same capital

- once that senior-loss ordering is satisfied, the fee-debt sweep MUST occur immediately in the same routine before any later withdrawal, margin check, or protocol-loss routing that relies on the remaining capital

The sweep itself is:

1. `debt = fee_debt_u128_checked(fee_credits_i)`

2. `pay = min(debt, C_i)`

3. apply:
   - `set_capital(i, C_i - pay)`
   - `fee_credits_i = checked_add_i128(fee_credits_i, pay as i128)`
   - `I = checked_add_u128(I, pay)`

Explicit fee debt is senior to future withdrawals and margin availability but is not itself realized PnL.

## 8. Fees

### 8.1 Trading fees

Trading fees are explicit transfers to insurance and MUST NOT be socialized through `h`.

Canonical symmetric fee schedule:

- `fee = mul_div_ceil_u128(trade_notional, trading_fee_bps, 10_000)`

- if `trading_fee_bps > 0` and `trade_notional > 0`, then `fee >= 1`

- if `trading_fee_bps == 0` or `trade_notional == 0`, then `fee = 0`

The fee MUST be charged using `charge_fee_to_insurance(i, fee)` from §4.10.

If an implementation supports asymmetric maker / taker or per-account fee schedules, it MUST instantiate them as explicit bounded `fee_i` values per charged account, and each `fee_i` MUST still be routed through `charge_fee_to_insurance`.

### 8.2 Maintenance fees

Maintenance fees MAY be charged and MAY create negative `fee_credits_i`.

Any maintenance-fee design MUST preserve Protocol-fee neutrality (§0.12):

- it MUST realize value only into `I` and/or `fee_credits_i`

- it MUST NOT realize maintenance fees by mutating `K_side`, `PNL_i`, or `PNL_pos_tot`

- it MUST NOT socialize maintenance fees through counterparty PnL, ADL quote deficit `D`, or haircut `h`

If a recurring maintenance fee depends on the position held over an interval, the implementation MUST represent it through a dedicated lazy fee accumulator or a formally equivalent event-segmented method that measures held position over time without relying on stale stored basis quantities. Realization on touch MUST write only to `I` and/or `fee_credits_i`; it MUST NOT reuse the profit/loss `K_side` indices.

If the implementation charges account-local recurring maintenance fees by elapsed time, then on each touch of account `i` it MUST:

1. compute the fee only over the interval `[last_fee_slot_i, current_slot]`

2. if an immediate explicit fee amount is charged in this touch, route it through `charge_fee_to_insurance`; if the exact immediate fee over the full interval would exceed `MAX_PROTOCOL_FEE_ABS`, split the interval or realization into bounded internal chunks before charging

3. if the fee model is debt-first, realize the debt only through checked `fee_credits_i` writes; if the exact debt increment over the full interval would exceed the permitted one-step write bound, split the interval or realization into bounded internal chunks

4. update `last_fee_slot_i = current_slot`

### 8.3 Fee debt as margin liability

`FeeDebt_i = fee_debt_u128_checked(fee_credits_i)`:

- MUST reduce `Eq_net_i`

- MUST be swept whenever principal becomes available

- MUST NOT directly change `Residual` or `PNL_pos_tot`

### 8.4 Liquidation fees

Liquidation fees MUST be charged during non-bankruptcy or bankruptcy synthetic liquidation.

The protocol MUST define:

- `liquidation_fee_bps`

- `liquidation_fee_cap`

- `min_liquidation_abs`

For a liquidation that closes `q_close_q` at `oracle_price`, define:

- if `q_close_q == 0`, then `liq_fee = 0`

- else:
  - `closed_notional = mul_div_floor_u128(q_close_q, oracle_price, POS_SCALE)`
  - `liq_fee_raw = mul_div_ceil_u128(closed_notional, liquidation_fee_bps, 10_000)`
  - `liq_fee = min(max(liq_fee_raw, min_liquidation_abs), liquidation_fee_cap)`

The liquidation fee MUST be charged using `charge_fee_to_insurance(i, liq_fee)`.

## 9. Margin checks and liquidation

### 9.1 Margin requirements

On current touched state, define:

- `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`

- `MM_req = mul_div_floor_u128(Notional_i, maintenance_bps, 10_000)`

- `IM_req = mul_div_floor_u128(Notional_i, initial_bps, 10_000)`

Healthy conditions:

- maintenance healthy if `Eq_net_i > MM_req as i128`

- initial-margin healthy if `Eq_net_i >= IM_req as i128`

### 9.2 Risk-increasing definition

A trade is risk-increasing when either:

1. `abs(new_eff_pos_q_i) > abs(old_eff_pos_q_i)`, or

2. the position sign flips across zero.

Flat to nonzero is also risk-increasing.

### 9.3 Liquidation eligibility

An account is liquidatable when after a full `touch_account_full`:

- `effective_pos_q(i) != 0`, and

- `Eq_net_i <= MM_req as i128`.

### 9.4 Partial / non-bankruptcy liquidation

This section defines a successful **non-bankruptcy** synthetic liquidation. It may reduce the position and leave it nonzero, or it may close the position fully to flat, but it MUST NOT leave uncovered negative realized PnL attached to a flat account.

Preconditions:

- the enclosing `liquidate(...)` top-level instruction has already called `touch_account_full(i, oracle_price, now_slot)`

- no additional `touch_account_full(i, ...)` may be performed inside this local routine

- let `old_eff_pos_q_i = effective_pos_q(i)`, require `old_eff_pos_q_i != 0`

- let `liq_side = side(old_eff_pos_q_i)`

A successful non-bankruptcy liquidation MUST:

1. choose a quantity `q_close_q` such that `0 < q_close_q <= abs(old_eff_pos_q_i)`

2. because the close is synthetic, execute exactly at `oracle_price` with zero execution-price slippage

3. compute `new_eff_pos_q_i = old_eff_pos_q_i - sign(old_eff_pos_q_i) * q_close_q`

4. apply the resulting effective position using `attach_effective_position(i, new_eff_pos_q_i)`

5. `OI_eff_liq_side` MUST NOT be decremented anywhere except through `enqueue_adl`

6. settle losses from principal via §7.1

7. compute `liq_fee` per §8.4 on the quantity actually closed in this step and charge it using `charge_fee_to_insurance(i, liq_fee)`

8. invoke `enqueue_adl(ctx, liq_side, q_close_q, 0)` to decrease global OI and socialize the quantity reduction with zero quote deficit

9. if `effective_pos_q(i) == 0`, require `PNL_i >= 0` after the loss settlement of step 6

10. if `ctx.pending_reset_long` or `ctx.pending_reset_short` became true in step 8, the liquidation MUST perform no further live-OI-dependent health logic in this instruction and MUST return control to the caller for §§5.7–5.8. This short-circuit MUST NOT waive step 9.

11. if `effective_pos_q(i) != 0`, evaluate `Eq_net_i`, `MM_req`, and any relevant current-state haircut inputs using the **current post-step-8 state** and require maintenance healthy on that current post-step-8 state

If a candidate partial liquidation would fail any of the above postconditions, the engine MUST NOT commit it as a successful partial liquidation; it MUST instead perform bankruptcy liquidation or reject according to the liquidation policy.

### 9.5 Bankruptcy liquidation

This section defines a local bankruptcy-liquidation routine. It assumes the enclosing top-level `liquidate(...)` instruction has already touched the account.

Preconditions:

- the enclosing `liquidate(...)` top-level instruction has already called `touch_account_full(i, oracle_price, now_slot)`

- no additional `touch_account_full(i, ...)` may be performed inside this local routine

The engine MUST be able to perform a bankruptcy liquidation:

1. let `old_eff_pos_q_i = effective_pos_q(i)`, require `old_eff_pos_q_i != 0`, and let `liq_side = side(old_eff_pos_q_i)`

2. set `q_close_q = abs(old_eff_pos_q_i)`; bankruptcy liquidation closes the account's full remaining effective position

3. because the close is synthetic, it MUST execute exactly at `oracle_price` with zero execution-price slippage

4. use `attach_effective_position(i, 0)`

5. `OI_eff_liq_side` MUST NOT be decremented anywhere except through `enqueue_adl`

6. settle losses from principal (§7.1)

7. calculate `liq_fee` per §8.4 using the quantity actually closed and charge it using `charge_fee_to_insurance(i, liq_fee)`

8. determine the uncovered bankruptcy deficit `D`:
   - if `PNL_i < 0`, let `D = (-PNL_i) as u128`
   - else let `D = 0`

9. invoke `enqueue_adl(ctx, liq_side, q_close_q, D)`

10. if `D > 0`, apply `set_pnl(i, 0)` after the deficit has been routed

Unpaid liquidation fee shortfall remains local `fee_credits_i` debt. It MUST NOT be added to `D`.

### 9.6 Side-mode gating

Before any top-level instruction rejects an OI-increasing operation because a side is in `ResetPending`, it MUST first invoke `maybe_finalize_ready_reset_sides_before_oi_increase()`.

Any operation that would increase net side OI on a side whose mode is `DrainOnly` or `ResetPending` MUST be rejected.

## 10. External operations

### 10.0 Account materialization

Any external operation that references an account identifier MUST first ensure the account is materialized.

For a top-level instruction that accepts `now_slot`, any missing referenced account MUST be materialized using the canonical initialization of §2.1.1 with `slot_anchor = now_slot` before any per-account logic.

`touch_account_full(i, ...)` is a local canonical settle subroutine. It assumes account `i` is already materialized.

An implementation MAY physically delete empty accounts, but if it does so it MUST update `materialized_account_count` with checked arithmetic and MUST preserve all aggregate invariants. Implementations are not required to support deletion.

### 10.1 `touch_account_full(i, oracle_price, now_slot)`

`touch_account_full` is the canonical **local** settle subroutine. It is not itself a complete top-level reset lifecycle.

Preconditions:

- account `i` is already materialized

- require `now_slot >= current_slot`

- require `now_slot >= slot_last`

- require `0 < oracle_price <= MAX_ORACLE_PRICE`

It MUST perform, in order:

1. `current_slot = now_slot`

2. `accrue_market_to(now_slot, oracle_price)`

3. `old_avail = max(PNL_i, 0) - R_i`

4. `old_warmable_i = WarmableGross_i` evaluated strictly before any profit-increasing state transition in this call

5. `settle_side_effects(i)`

6. `new_avail = max(PNL_i, 0) - R_i`

7. if `new_avail > old_avail`:
   - record `capital_before_restart = C_i`
   - invoke the restart-on-new-profit rule (§6.5) passing `old_warmable_i`
   - if `C_i > capital_before_restart`, immediately sweep fee debt (§7.5)

8. settle losses from principal (§7.1)

9. realize any configured maintenance-fee accrual under §8.2, and if such logic is time-based update `last_fee_slot_i = current_slot`

10. if `effective_pos_q(i) == 0` and `PNL_i < 0`, resolve uncovered loss per §7.3

11. convert warmable profits (§7.4)

12. sweep fee debt (§7.5)

This local settle subroutine MUST NOT itself begin a side reset and MUST NOT itself recompute `r_last`.


### 10.1.1 `settle_account(i, oracle_price, now_slot)` — standalone top-level settle wrapper

If an implementation exposes settlement as a standalone external instruction, it MUST expose this wrapper rather than exposing raw `touch_account_full` directly.

Procedure:

1. initialize fresh instruction context `ctx`

2. if account `i` is not yet materialized, materialize it using `slot_anchor = now_slot` per §10.0

3. `touch_account_full(i, oracle_price, now_slot)`

4. `schedule_end_of_instruction_resets(ctx)`

5. `finalize_end_of_instruction_resets(ctx)`

6. if funding-rate inputs changed, recompute `r_last` exactly once from the final post-reset state

7. assert `OI_eff_long == OI_eff_short`

### 10.2 `deposit(i, amount, now_slot)`

`deposit` is a pure capital-transfer instruction. It MUST NOT implicitly call `touch_account_full` or otherwise mutate side state.

Procedure:

1. require `now_slot >= current_slot`

2. require `now_slot >= slot_last`

3. if account `i` is not yet materialized, materialize it using `slot_anchor = now_slot` per §10.0

4. `current_slot = now_slot`

5. require `checked_add_u128(V, amount) <= MAX_VAULT_TVL`

6. `V += amount`

7. `set_capital(i, checked_add_u128(C_i, amount))`

8. settle losses from principal (§7.1)

9. if `basis_pos_q_i == 0` and `PNL_i < 0`, resolve uncovered loss per §7.3

10. immediately apply fee-debt sweep (§7.5)

Because `deposit` cannot mutate OI, stored positions, stale-account counts, phantom-dust bounds, side modes, or any permitted funding-rate input, it MAY omit §§5.7–5.8 and MUST NOT recompute `r_last`.

### 10.3 `withdraw(i, amount, oracle_price, now_slot)`

Before step 1, ensure account `i` is materialized per §10.0.

Procedure:

1. initialize fresh instruction context `ctx`

2. `touch_account_full(i, oracle_price, now_slot)`

3. require `amount <= C_i`

4. if `effective_pos_q(i) != 0`, require post-withdraw `Eq_net_i` to satisfy initial margin  
   **Normative clarification:** when evaluating post-withdraw `Eq_net_i`, the simulation MUST reflect both `C_i := C_i - amount` and `V := V - amount`, or equivalently use the unchanged pre-withdraw `Residual`; the simulation MUST NOT temporarily reduce `C_i` without also reducing `V`

5. apply:
   - `set_capital(i, C_i - amount)`
   - `V -= amount`

6. `schedule_end_of_instruction_resets(ctx)`

7. `finalize_end_of_instruction_resets(ctx)`

8. if funding-rate inputs changed, recompute `r_last` exactly once from the final post-reset state

9. assert `OI_eff_long == OI_eff_short`

### 10.4 `execute_trade(a, b, oracle_price, now_slot, size_q, exec_price)`

`size_q > 0` means account `a` buys base from account `b`.

Before step 1, ensure both accounts `a` and `b` are materialized per §10.0.

Procedure:

1. initialize fresh instruction context `ctx`

2. require `a != b`

3. require `size_q > 0`

4. require `size_q <= MAX_TRADE_SIZE_Q`

5. require `0 < exec_price <= MAX_ORACLE_PRICE`

6. compute `trade_notional = mul_div_floor_u128(size_q, exec_price, POS_SCALE)` using checked arithmetic and require `trade_notional <= MAX_ACCOUNT_NOTIONAL`

7. `touch_account_full(a, oracle_price, now_slot)`

8. `touch_account_full(b, oracle_price, now_slot)`

9. let `old_eff_pos_q_a = effective_pos_q(a)` and `old_eff_pos_q_b = effective_pos_q(b)`

10. record post-touch pre-trade warmup anchors for each account:
   - `old_avail_a = max(PNL_a, 0) - R_a`
   - `old_avail_b = max(PNL_b, 0) - R_b`
   - `old_warmable_a = WarmableGross_a`
   - `old_warmable_b = WarmableGross_b`

11. invoke `maybe_finalize_ready_reset_sides_before_oi_increase()`

12. define resulting effective positions using checked signed arithmetic:
    - `new_eff_pos_q_a = checked_add_i128(old_eff_pos_q_a, size_q as i128)`
    - `new_eff_pos_q_b = checked_sub_i128(old_eff_pos_q_b, size_q as i128)`

13. require `abs(new_eff_pos_q_a) <= MAX_POSITION_ABS_Q` and `abs(new_eff_pos_q_b) <= MAX_POSITION_ABS_Q`

14. reject if the trade would increase net side OI on any side whose mode is `DrainOnly` or `ResetPending`

15. apply immediate execution-slippage alignment PnL before fees:
    - `trade_pnl_a_num = (size_q as i128) * ((oracle_price as i128) - (exec_price as i128))`, using checked `i128` arithmetic
    - `trade_pnl_a = floor_div_signed_conservative(trade_pnl_a_num, POS_SCALE)`
    - `trade_pnl_b = checked_neg_i128(trade_pnl_a)`
    - `set_pnl(a, checked_add_i128(PNL_a, trade_pnl_a))`
    - `set_pnl(b, checked_add_i128(PNL_b, trade_pnl_b))`

16. apply the resulting effective positions using `attach_effective_position(a, new_eff_pos_q_a)` and `attach_effective_position(b, new_eff_pos_q_b)`

17. update `OI_eff_long` / `OI_eff_short` atomically from old versus new per-account long / short contributions:
    - `long_contrib(pos) = max(pos, 0) as u128`
    - `short_contrib(pos) = max(-pos, 0) as u128`
    - subtract old contributions for `a` and `b` with checked arithmetic
    - add new contributions for `a` and `b` with checked arithmetic
    - require each side to remain `<= MAX_OI_SIDE_Q`

18. settle post-trade losses from principal for both accounts via §7.1

19. charge explicit trading fees per §8.1 for each charged account using the precomputed `trade_notional`

20. for any account whose `AvailGross_i` increased relative to its post-touch pre-trade state, invoke the restart-on-new-profit rule (§6.5) using the corresponding `old_warmable_i`

21. any fee-debt sweep required by §6.5 MUST occur before the next step

22. enforce post-trade margin using the current post-step-21 state:
    - if the resulting effective position is nonzero, always require maintenance
    - if risk-increasing, also require initial margin
    - if the resulting effective position is zero, require `PNL_i >= 0` after the post-trade loss settlement of step 18; an organic close MUST NOT leave uncovered negative realized-PnL obligations

23. `schedule_end_of_instruction_resets(ctx)`

24. `finalize_end_of_instruction_resets(ctx)`

25. if funding-rate inputs changed, recompute `r_last` exactly once from the final post-reset state

26. assert `OI_eff_long == OI_eff_short`

### 10.5 `liquidate(i, oracle_price, now_slot, ...)`

Before step 1, ensure account `i` is materialized per §10.0.

Procedure:

1. initialize fresh instruction context `ctx`

2. `touch_account_full(i, oracle_price, now_slot)`

3. require liquidation eligibility (§9.3)

4. execute either:
   - a successful partial / non-bankruptcy liquidation per §9.4, or
   - a bankruptcy liquidation per §9.5,
   passing `ctx` through any `enqueue_adl` call

5. if any remaining nonzero position exists after liquidation, it MUST already have been reattached via `attach_effective_position`

6. `schedule_end_of_instruction_resets(ctx)`

7. `finalize_end_of_instruction_resets(ctx)`

8. if funding-rate inputs changed, recompute `r_last` exactly once from the final post-reset state

9. assert `OI_eff_long == OI_eff_short`

### 10.6 `keeper_crank(oracle_price, now_slot, work_plan...)`

A keeper crank is a top-level external instruction and MUST use the same deferred reset lifecycle as other top-level instructions.

Keeper current-state rule:

- Any keeper action that depends on current account state — including liquidation eligibility, any per-account warmup-conversion decision, any per-account fee-debt extraction, or any other account-local cleanup that relies on current PnL / margin / warmup / fee state — MUST first bring that account to current state with `touch_account_full(i, oracle_price, now_slot)` earlier in the same instruction, unless the action is explicitly defined safe on a stored-flat account with `basis_pos_q_i == 0` and no pending side effects.

- A keeper MUST NOT perform standalone warmup conversion, standalone fee-debt sweep, or liquidation-health decisions on an untouched open-position or stale-basis account.

Procedure:

1. initialize fresh instruction context `ctx`

2. `accrue_market_to(now_slot, oracle_price)`

3. a keeper MAY:
   - materialize any missing referenced account using `slot_anchor = now_slot` before touching or liquidating it
   - call `touch_account_full(i, oracle_price, now_slot)` on a bounded window of materialized accounts
   - liquidate unhealthy accounts only after those accounts have been touched to current state in this instruction, passing `ctx` through any `enqueue_adl` call
   - perform additional idempotent keeper-only cleanup only on accounts already touched to current state in this instruction
   - prioritize accounts on a `DrainOnly` or `ResetPending` side
   - explicitly call `finalize_side_reset(side)` when its preconditions already hold, although this is not required because step 5 auto-finalizes eligible `ResetPending` sides
   - if, during this work, either `ctx.pending_reset_long` or `ctx.pending_reset_short` becomes true, the keeper MUST stop processing further accounts in that instruction and proceed directly to steps 4–6

4. `schedule_end_of_instruction_resets(ctx)`

5. `finalize_end_of_instruction_resets(ctx)`

6. if funding-rate inputs changed, recompute `r_last` exactly once from the final post-reset state

7. assert `OI_eff_long == OI_eff_short`

The crank MUST maintain a cursor or equivalent progress mechanism so repeated calls eventually cover active accounts supplied to it.

## 11. Required test properties (minimum)

An implementation MUST include tests that cover at least:

1. Conservation: `V >= C_tot + I` always, and `Σ PNL_eff_pos_i <= Residual`.

2. Oracle manipulation: inflated positive PnL cannot be withdrawn before maturity.

3. Same-epoch local settlement: settlement of one account does not depend on any canonical-order prefix.

4. Non-compounding quantity basis: repeated same-epoch touches without explicit position mutation do not compound quantity-flooring loss.

5. Dynamic dust bound: after any number of same-epoch zeroing events, explicit basis replacements, and ADL multiplier truncations before a reset, authoritative OI on a side with no stored positions is bounded by that side's cumulative `phantom_dust_bound_side_q`.

6. Dust-clear scheduling: dust clearance and reset initiation happen only at end of top-level instructions, never mid-instruction.

7. Epoch-safe reset: accounts cannot be attached to a new epoch before `begin_full_drain_reset` runs at end of instruction.

8. Precision-exhaustion terminal drain: if `A_candidate == 0` with `OI_post > 0`, the engine force-drains both sides instead of reverting or clamping.

9. ADL representability fallback: if `K_opp + delta_K_exact` would overflow stored `i128`, quantity socialization still proceeds and the quote deficit routes through `absorb_protocol_loss`.

10. Warmup anti-retroactivity: newly generated profit cannot inherit old dormant maturity headroom.

11. Pure conversion slope preservation: frequent cranks do not create exponential-decay maturity.

12. Trade slippage alignment: opening or flipping at `exec_price != oracle_price` realizes immediate zero-sum PnL against the oracle.

13. Unit consistency: margin and notional use quote-token atomic units consistently.

14. `set_pnl` underflow safety: negative PnL updates do not underflow `PNL_pos_tot`.

15. `PNL_i == i128::MIN` forbidden: every negation path is safe.

16. Organic close bankruptcy guard: a flat trade cannot bypass ADL by leaving negative `PNL_i` behind.

17. Explicit fee shortfall routing: trading-fee or liquidation-fee shortfall becomes negative `fee_credits_i`, does not touch `PNL_i`, and does not inflate `D`.

18. Funding anti-retroactivity: changing rate inputs near the end of an interval does not retroactively reprice earlier slots.

19. Funding no-mint: payer-driven funding rounding MUST NOT mint positive aggregate claims even when `A_long != A_short`.

20. Flat-account negative remainder: a flat account with negative `PNL_i` after principal exhaustion resolves through `absorb_protocol_loss` only in the allowed flat-account paths.

21. Reset finalization: after reconciling stale accounts, the side can leave `ResetPending` and accept fresh OI again.

22. Immediate fee seniority after restart conversion: if the restart-on-new-profit rule converts matured entitlement into `C_i` while fee debt is outstanding, the fee-debt sweep occurs immediately before later loss-settlement or margin logic can consume that new capital.

23. Post-trade loss settlement: a solvent trader who closes to flat and can pay losses from principal is not rejected due to an unperformed implicit settlement step.

24. Keeper quiescence after pending reset: if a keeper-triggered `enqueue_adl` or precision-exhaustion terminal drain schedules any reset, the same keeper instruction performs no further live-OI-dependent account processing before end-of-instruction reset handling.

25. Keeper reset lifecycle: `keeper_crank` can touch the last dusty or stale account and still trigger the required end-of-instruction reset scheduling / finalization.

26. Clean-empty market lifecycle: a fully drained and fully reconciled market can return to `Normal` and admit fresh OI without getting stuck in a reset loop.

27. Non-representable `delta_K_abs` fallback: if `delta_K_abs` is not representable as `i128`, quote deficit routes through `absorb_protocol_loss` while quantity socialization still proceeds.

28. Explicit-mutation dust accounting: if a trade or liquidation discards a same-epoch basis whose exact effective quantity had a nonzero fractional remainder, `phantom_dust_bound_side_q` increases by exactly `1` q-unit.

29. Global A-truncation dust accounting: if `enqueue_adl` computes `A_candidate = floor(A_old * OI_post / OI)` with nonzero remainder, the engine increments `phantom_dust_bound_opp_q` by at least the conservative bound from §5.6, and that bound is sufficient to cover the additional phantom OI introduced by the global multiplier truncation.

30. Empty-opposing-side deficit fallback: if `stored_pos_count_opp == 0`, real quote deficits route through `absorb_protocol_loss(D)` and are not written into `K_opp`.

31. Unilateral-empty orphan resolution: if one side has `stored_pos_count_side == 0`, its `OI_eff_side` is within that side's phantom-dust bound, and `OI_eff_long == OI_eff_short`, then `schedule_end_of_instruction_resets(ctx)` schedules reset on both sides even if the opposite side still has stored positions.

32. Unilateral-empty corruption guard: if one side has `stored_pos_count_side == 0` but `OI_eff_long != OI_eff_short`, unilateral dust clearance fails conservatively.

33. Automatic reset finalization: the top-level instruction that reconciles the last stale account can leave the side in `Normal` at end-of-instruction without requiring a separate keeper-only finalize call.

34. Trade-path reopenability: if a side is already `ResetPending` but also already eligible for `finalize_side_reset`, an `execute_trade` instruction can auto-finalize that side before OI-increase gating and admit fresh OI in the same instruction.

35. Trading-loss seniority: in `execute_trade`, realized losses are settled from principal before trading fees are charged.

36. Non-bankruptcy-liquidation loss seniority: in §9.4, realized losses are settled from principal before liquidation fees are charged.

37. Synthetic liquidation zero slippage: bankruptcy and non-bankruptcy liquidation perform no execution-price PnL transfer beyond the current oracle mark.

38. Mark one-shot exactness: `accrue_market_to` applies mark exactly once per invocation even when funding requires multiple bounded sub-steps.

39. Current-state health check after partial liquidation: the post-liquidation maintenance check uses the current post-step state, including updated `I`, `PNL_pos_tot`, and haircut ratio.

40. Instruction-final funding recomputation: if a liquidation or keeper action schedules a terminal drain, the stored next-interval `r_last` corresponds to the final post-reset `OI` and side modes, not a stale pre-reset state.

41. Checked signed addition on settlement: every `set_pnl(PNL_i + delta)` call site uses checked signed addition and cannot wrap.

42. Wide K-difference settlement: `pnl_delta` remains correct even when `K_now - K_snap` would overflow `i128` if computed naively.

43. Aggregate positive-PnL bound: if account creation and `set_pnl` caps are enforced, `PNL_pos_tot` cannot overflow `u128`.

44. Self-trade rejection: `execute_trade(a, a, ...)` fails conservatively.

45. Account initialization safety: a newly materialized account cannot divide by zero in `effective_pos_q` and cannot accrue genesis-to-now maintenance fees on first touch.

46. Empty-market funding no-op: if either snapped side OI is zero, `accrue_market_to` applies no funding K-motion for that invocation even when `r_last != 0`.

47. Withdraw final-state funding recomputation: if a withdraw instruction finalizes a reset or otherwise changes funding-rate inputs through the reset lifecycle, `r_last` is recomputed exactly once from the final post-reset state.

48. Trade-size precondition safety: `execute_trade` rejects `size_q > MAX_TRADE_SIZE_Q` or `trade_notional > MAX_ACCOUNT_NOTIONAL` before any signed cast or slippage multiplication.

49. Maintenance-fee seniority on touch: if immediate account-local maintenance fees are enabled, `touch_account_full` settles existing realized trading losses from principal before extracting maintenance fees, so maintenance cannot inflate a later bankruptcy deficit.

50. Partial-liquidation reset short-circuit: even if `enqueue_adl(ctx, liq_side, q_close_q, 0)` schedules a reset, a candidate partial liquidation that leaves the account flat with `PNL_i < 0` is not a successful partial liquidation and must instead fail as partial or route to bankruptcy according to policy.

51. Keeper end-state parity: `keeper_crank` ends with `OI_eff_long == OI_eff_short`.

52. Timed monotonicity: every timed helper or instruction rejects `now_slot < current_slot` or `now_slot < slot_last`, and `accrue_market_to` leaves `current_slot = now_slot` on success.

53. Slot-anchored materialization: a newly materialized account created inside a timed instruction sets both `w_start_i` and `last_fee_slot_i` to that instruction's `now_slot`, not to `0` or a stale global slot.

54. Deposit loss seniority: in `deposit`, newly deposited capital settles existing realized trading losses before any outstanding fee debt is swept.

55. Deposit true-flat routing: a capital-only `deposit` path may invoke §7.3 only when `basis_pos_q_i == 0`; it MUST NOT treat a stale nonzero stored basis with `effective_pos_q(i) == 0` as eligible for flat-account loss socialization.

56. Dust-liquidation minimum-fee floor: if `q_close_q > 0` but `closed_notional` floors to zero, liquidation still charges `min_liquidation_abs` (subject to `liquidation_fee_cap`).

57. Standalone settle wrapper lifecycle: a top-level `settle_account` instruction can reconcile the last stale or dusty account, run required end-of-instruction reset scheduling / finalization, and recompute `r_last` from the final post-reset state when funding inputs changed.

58. Keeper upfront accrual: a `keeper_crank` that performs only maintenance or reset work still enforces timed monotonicity by accruing the market once at instruction start.

59. Keeper current-state gating: `keeper_crank` does not perform liquidation-health checks, standalone warmup conversion, or standalone fee-debt extraction on an account unless that account has already been brought to current state with `touch_account_full(i, oracle_price, now_slot)` in the same instruction, or the action is explicitly defined safe on a stored-flat account.

60. Maintenance-fee neutrality: any recurring maintenance-fee realization increases `I` and/or negative `fee_credits_i` only; it does not mutate `K_side`, `PNL_i`, `PNL_pos_tot`, haircut inputs, or bankruptcy deficit `D`.

61. Bounded maintenance-fee realization: if a recurring maintenance fee over a long interval would exceed `MAX_PROTOCOL_FEE_ABS` or the permitted one-step `fee_credits_i` write range, the implementation splits the realization into bounded internal chunks instead of overflowing, reverting spuriously, or socializing the excess through PnL.


## 12. Reference pseudocode (non-normative)

### 12.1 Compute haircut and current-state equity

```text
senior_sum = checked_add_u128(C_tot, I)
Residual = max(0, V - senior_sum)

if PNL_pos_tot == 0:
    h_num = 1
    h_den = 1
else:
    h_num = min(Residual, PNL_pos_tot)
    h_den = PNL_pos_tot

PNL_pos_i = max(PNL_i, 0)
if PNL_pos_tot == 0:
    PNL_eff_pos_i = PNL_pos_i
else:
    PNL_eff_pos_i = floor((wide)PNL_pos_i * h_num / h_den)

Eq_real_raw = checked_add_i128(C_i as i128, min(PNL_i, 0))
Eq_real_raw = checked_add_i128(Eq_real_raw, PNL_eff_pos_i as i128)
Eq_real_i = max(0, Eq_real_raw)

FeeDebt_i = fee_debt_u128_checked(fee_credits_i)
Eq_net_raw = checked_sub_i128(Eq_real_i, FeeDebt_i as i128)
Eq_net_i = max(0, Eq_net_raw)
```

### 12.2 Same-epoch settlement

```text
if basis_pos_q_i != 0:
    s = side(basis_pos_q_i)
    if epoch_snap_i == epoch_s:
        q_eff_new = floor(abs(basis_pos_q_i) * A_s / a_basis_i)
        den = a_basis_i * POS_SCALE
        pnl_delta = wide_signed_mul_div_floor_from_k_pair(abs(basis_pos_q_i), K_s, k_snap_i, den)
        set_pnl(i, checked_add_i128(PNL_i, pnl_delta))
        if q_eff_new == 0:
            inc_phantom_dust_bound(s)
            set_position_basis_q(i, 0)
            reset_zero_position(i, epoch_s)
        else:
            k_snap_i = K_s
            epoch_snap_i = epoch_s
```

### 12.3 Epoch mismatch

```text
if basis_pos_q_i != 0 and epoch_snap_i != epoch_s:
    assert mode_s == ResetPending
    assert epoch_snap_i + 1 == epoch_s
    den = a_basis_i * POS_SCALE
    pnl_delta = wide_signed_mul_div_floor_from_k_pair(abs(basis_pos_q_i), K_epoch_start_s, k_snap_i, den)
    set_pnl(i, checked_add_i128(PNL_i, pnl_delta))
    set_position_basis_q(i, 0)
    dec_stale_account_count_checked(s)
    reset_zero_position(i, epoch_s)
```

### 12.4 Exact one-shot mark plus sub-stepped funding

```text
accrue_market_to(now_slot, oracle_price):
    assert now_slot >= current_slot
    assert now_slot >= slot_last
    assert 0 < oracle_price <= MAX_ORACLE_PRICE

    oi_long_snap = OI_eff_long
    oi_short_snap = OI_eff_short

    if now_slot == slot_last and oracle_price == P_last:
        current_slot = now_slot
        return

    delta_p = (oracle_price as i128) - (P_last as i128)

    // mark applies exactly once
    if delta_p != 0:
        if oi_long_snap > 0:
            K_long += A_long * delta_p
        if oi_short_snap > 0:
            K_short -= A_short * delta_p

    dt_rem = now_slot - slot_last
    while dt_rem > 0:
        dt = min(dt_rem, MAX_FUNDING_DT)
        if r_last != 0 and oi_long_snap > 0 and oi_short_snap > 0:
            funding_term_raw = fund_px_last * abs(r_last) * dt
            if r_last > 0:
                payer = long
                receiver = short
            else:
                payer = short
                receiver = long

            A_p = A_side(payer)
            A_r = A_side(receiver)

            delta_K_payer_abs = ceil(A_p * funding_term_raw / 10_000)
            delta_K_receiver_abs = floor(delta_K_payer_abs * A_r / A_p)

            K_payer -= delta_K_payer_abs
            K_receiver += delta_K_receiver_abs

        dt_rem -= dt

    slot_last = now_slot
    P_last = oracle_price
    fund_px_last = oracle_price
    current_slot = now_slot
```

### 12.5 Charge explicit fee to insurance without PnL socialization

```text
charge_fee_to_insurance(i, fee):
    assert fee <= MAX_PROTOCOL_FEE_ABS
    fee_paid = min(fee, C_i)
    set_capital(i, C_i - fee_paid)
    I += fee_paid
    fee_shortfall = fee - fee_paid
    if fee_shortfall > 0:
        fee_credits_i = checked_sub_i128(fee_credits_i, fee_shortfall as i128)
```

### 12.6 ADL with representability fallback

```text
enqueue_adl(ctx, liq_side, q_close_q, D):
    opp = opposite(liq_side)

    if q_close_q > 0:
        OI_eff_liq_side -= q_close_q

    OI = OI_eff_opp

    if OI == 0:
        if D > 0:
            absorb_protocol_loss(D)
        return

    if stored_pos_count_opp == 0:
        assert q_close_q <= OI
        OI_post = OI - q_close_q
        if D > 0:
            absorb_protocol_loss(D)
        OI_eff_opp = OI_post
        if OI_post == 0:
            ctx.pending_reset_opp = true
            ctx.pending_reset_liq_side = true
        return

    assert q_close_q <= OI
    A_old = A_opp
    OI_post = OI - q_close_q

    if D > 0:
        adl_scale = A_old * POS_SCALE
        delta_result = wide_mul_div_ceil_u128_or_over_i128max(D, adl_scale, OI)
        if delta_result is Value:
            delta_K_abs = value(delta_result)
            delta_K_exact = -(delta_K_abs as i128)
            if fits_i128(K_opp + delta_K_exact):
                K_opp = K_opp + delta_K_exact
            else:
                absorb_protocol_loss(D)
        else:
            absorb_protocol_loss(D)

    if OI_post == 0:
        OI_eff_opp = 0
        ctx.pending_reset_opp = true
        ctx.pending_reset_liq_side = true
        return

    A_prod_exact = A_old * OI_post
    A_candidate = floor(A_prod_exact / OI)
    A_trunc_rem = A_prod_exact mod OI

    if A_candidate > 0:
        A_opp = A_candidate
        OI_eff_opp = OI_post
        if A_trunc_rem != 0:
            N_opp = stored_pos_count_opp as u128
            global_a_dust_bound = N_opp + ceil((OI + N_opp) / A_old)
            phantom_dust_bound_opp_q += global_a_dust_bound
        if A_opp < MIN_A_SIDE:
            mode_opp = DrainOnly
        return

    OI_eff_opp = 0
    OI_eff_liq_side = 0
    ctx.pending_reset_opp = true
    ctx.pending_reset_liq_side = true
```

### 12.7 Finalize-ready preflight for OI-increasing instructions

```text
maybe_finalize_ready_reset_sides_before_oi_increase():
    if mode_long == ResetPending and OI_eff_long == 0 and stale_account_count_long == 0 and stored_pos_count_long == 0:
        finalize_side_reset(long)
    if mode_short == ResetPending and OI_eff_short == 0 and stale_account_count_short == 0 and stored_pos_count_short == 0:
        finalize_side_reset(short)
```

### 12.8 End-of-instruction dust clearance and finalization

```text
schedule_end_of_instruction_resets(ctx):
    if stored_pos_count_long == 0 and stored_pos_count_short == 0:
        clear_bound_q = phantom_dust_bound_long_q + phantom_dust_bound_short_q
        has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0) or (phantom_dust_bound_short_q > 0)
        if has_residual_clear_work:
            assert OI_eff_long == OI_eff_short
            if OI_eff_long <= clear_bound_q and OI_eff_short <= clear_bound_q:
                OI_eff_long = 0
                OI_eff_short = 0
                ctx.pending_reset_long = true
                ctx.pending_reset_short = true
            else:
                fail_conservatively()

    else if stored_pos_count_long == 0:
        has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0)
        if has_residual_clear_work:
            assert OI_eff_long == OI_eff_short
            if OI_eff_long <= phantom_dust_bound_long_q:
                OI_eff_long = 0
                OI_eff_short = 0
                ctx.pending_reset_long = true
                ctx.pending_reset_short = true
            else:
                fail_conservatively()

    else if stored_pos_count_short == 0:
        has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_short_q > 0)
        if has_residual_clear_work:
            assert OI_eff_long == OI_eff_short
            if OI_eff_short <= phantom_dust_bound_short_q:
                OI_eff_long = 0
                OI_eff_short = 0
                ctx.pending_reset_long = true
                ctx.pending_reset_short = true
            else:
                fail_conservatively()

    if mode_long == DrainOnly and OI_eff_long == 0:
        ctx.pending_reset_long = true
    if mode_short == DrainOnly and OI_eff_short == 0:
        ctx.pending_reset_short = true

finalize_end_of_instruction_resets(ctx):
    if ctx.pending_reset_long and mode_long != ResetPending:
        begin_full_drain_reset(long)
    if ctx.pending_reset_short and mode_short != ResetPending:
        begin_full_drain_reset(short)
    if mode_long == ResetPending and OI_eff_long == 0 and stale_account_count_long == 0 and stored_pos_count_long == 0:
        finalize_side_reset(long)
    if mode_short == ResetPending and OI_eff_short == 0 and stale_account_count_short == 0 and stored_pos_count_short == 0:
        finalize_side_reset(short)
```

### 12.9 Trade-path ordering with loss seniority

```text
execute_trade(...):
    assert 0 < size_q <= MAX_TRADE_SIZE_Q
    trade_notional = floor(size_q * exec_price / POS_SCALE)
    assert trade_notional <= MAX_ACCOUNT_NOTIONAL
    touch_account_full(a)
    touch_account_full(b)
    maybe_finalize_ready_reset_sides_before_oi_increase()
    apply trade-pnl alignment
    attach resulting positions
    update OI atomically
    settle_losses_from_principal(a)
    settle_losses_from_principal(b)
    charge_fee_to_insurance(a, fee_a_from(trade_notional))
    charge_fee_to_insurance(b, fee_b_from(trade_notional))
    run warmup restart logic if AvailGross increased
    enforce post-trade margin on current state
    schedule_end_of_instruction_resets(ctx)
    finalize_end_of_instruction_resets(ctx)
    recompute_next_funding_rate_from_final_state(oracle_price) if inputs changed
```

### 12.10 `touch_account_full` loss-before-maintenance ordering

```text
touch_account_full(i, oracle_price, now_slot):
    assert now_slot >= current_slot
    assert now_slot >= slot_last
    assert 0 < oracle_price <= MAX_ORACLE_PRICE
    current_slot = now_slot
    accrue_market_to(now_slot, oracle_price)
    old_avail = max(PNL_i, 0) - R_i
    old_warmable_i = WarmableGross_i on current_slot before any profit increase
    settle_side_effects(i)
    new_avail = max(PNL_i, 0) - R_i
    if new_avail > old_avail:
        capital_before_restart = C_i
        restart_on_new_profit(old_warmable_i)
        if C_i > capital_before_restart:
            sweep_fee_debt()
    settle_losses_from_principal(i)
    charge_or_extend_account_local_maintenance(i)
    if effective_pos_q(i) == 0 and PNL_i < 0:
        absorb_protocol_loss((-PNL_i) as u128)
        set_pnl(i, 0)
    convert_warmable_profits()
    sweep_fee_debt()
```

### 12.11 Partial-liquidation success path with reset short-circuit guard

```text
partial_liquidation(i, q_close_q, ctx):
    old_eff = effective_pos_q(i)
    new_eff = old_eff - sign(old_eff) * q_close_q
    attach_effective_position(i, new_eff)
    settle_losses_from_principal(i)
    liq_fee = liquidation_fee_from(q_close_q, oracle_price)
    charge_fee_to_insurance(i, liq_fee)
    enqueue_adl(ctx, side(old_eff), q_close_q, 0)
    if effective_pos_q(i) == 0:
        assert PNL_i >= 0
    if ctx.pending_reset_long or ctx.pending_reset_short:
        return
    if effective_pos_q(i) != 0:
        assert maintenance_healthy_on_current_post_step_state(i)
```



### 12.12 Timed account materialization and deposit loss seniority

```text
deposit(i, amount, now_slot):
    assert now_slot >= current_slot
    assert now_slot >= slot_last
    if account i does not exist:
        materialize_account(i, now_slot)
    current_slot = now_slot
    assert V + amount <= MAX_VAULT_TVL
    V += amount
    set_capital(i, C_i + amount)
    settle_losses_from_principal(i)
    if basis_pos_q_i == 0 and PNL_i < 0:
        absorb_protocol_loss((-PNL_i) as u128)
        set_pnl(i, 0)
    sweep_fee_debt()
```

### 12.13 Standalone settle wrapper

```text
settle_account(i, oracle_price, now_slot):
    ctx = fresh_reset_context()
    if account i does not exist:
        materialize_account(i, now_slot)
    touch_account_full(i, oracle_price, now_slot)
    schedule_end_of_instruction_resets(ctx)
    finalize_end_of_instruction_resets(ctx)
    recompute_next_funding_rate_from_final_state(oracle_price) if inputs changed
    assert OI_eff_long == OI_eff_short
```

### 12.14 Keeper upfront accrual, current-state gating, and timed monotonicity

```text
keeper_crank(oracle_price, now_slot, work_plan):
    ctx = fresh_reset_context()
    accrue_market_to(now_slot, oracle_price)

    for each planned account i in bounded work_plan:
        if account i does not exist:
            materialize_account(i, now_slot)

        touch_account_full(i, oracle_price, now_slot)

        // Any liquidation decision or keeper-only cleanup that depends on
        // PnL, margin, warmup, or fee debt must happen only after this touch.
        maybe_liquidate_or_cleanup_current_state_account(i, ctx)

        if ctx.pending_reset_long or ctx.pending_reset_short:
            stop further live-OI-dependent account work
            break

    schedule_end_of_instruction_resets(ctx)
    finalize_end_of_instruction_resets(ctx)
    recompute_next_funding_rate_from_final_state(oracle_price) if inputs changed
    assert OI_eff_long == OI_eff_short
```

## 13. Compatibility notes

- The spec is compatible with LP accounts and user accounts; both share the same protected-principal and junior-profit mechanics.

- The only mandatory `O(1)` global aggregates for solvency are `C_tot` and `PNL_pos_tot`; the A/K side indices add `O(1)` state for lazy settlement.

- The spec deliberately rejects hidden residual matching. Bankruptcy socialization occurs through explicit A/K state only.

- Same-epoch quantity settlement is local and non-compounding. The design does not require a canonical-order carry allocator.

- Rare side-precision stress is handled by `DrainOnly`, dynamically bounded dust clearance, unilateral / bilateral orphan resolution, and precision-exhaustion terminal drain rather than assertion failure or permanent market deadlock.

- By utilizing base-10 scaling bounded within `10^16` TVL limits, explicit `MAX_TRADE_SIZE_Q`, and `MAX_ACCOUNT_NOTIONAL` enforcement, the engine executes inside native 128-bit persistent boundaries while permitting transient exact wide intermediates only where mathematically necessary.

- Any optional recurring maintenance-fee design must realize value only into `I` and/or `fee_credits_i`; it must not reuse profit/loss `K_side` or mutate `PNL_i` / `PNL_pos_tot`.

- Any upgrade path from a version that did not maintain `basis_pos_q_i`, `a_basis_i`, `stored_pos_count_*`, `stale_account_count_*`, `phantom_dust_bound_*_q`, or `materialized_account_count` consistently MUST complete migration before OI-increasing operations are re-enabled.

