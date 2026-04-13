# Risk Engine Spec (Source of Truth) — v12.16.4

**Combined Single-Document Native 128-bit Revision  
(Wrapper-Driven Warmup Horizon / Wrapper-Owned Account-Fee Policy / Wrapper-Supplied High-Precision Funding Side-Index Input / Simplified Scheduled-Plus-Pending Warmup / Exact Candidate-Trade Neutralization / Price-Bounded Resolved-Market Settlement / Whole-Only Automatic Flat Conversion / Full-Local-PnL Maintenance / Immutable Configuration / Unencumbered-Flat Deposit Sweep / Mandatory Post-Partial Local Health Check Edition)**

**Design:** Protected Principal + Junior Profit Claims + Lazy A/K/F Side Indices (Native 128-bit Base-10 Scaling)  
**Status:** implementation source of truth (normative language: MUST / MUST NOT / SHOULD / MAY)  
**Scope:** perpetual DEX risk engine for a single quote-token vault

This revision supersedes v12.16.3. It keeps the two-bucket warmup simplification and fixes the remaining non-minor issues:

1. strict risk-reducing trade checks use **actual applied fee-equity impact**, never nominal requested fee,
2. resolved-market close remains split so **non-positive accounts close immediately after local reconciliation**, while **positive claims remain snapshot-gated**,
3. the ADL dust-bound increment and end-of-instruction reset rules remain exact normative formulas,
4. the positive resolved-close path fails conservatively if a ready snapshot has zero denominator while any account still has positive resolved PnL,
5. active-position side-cap enforcement applies to **every** side-count increment, including sign flips,
6. reserve-creation helpers are anchored to `current_slot`, so there is no ambiguity between helper-local slot arguments and the already-accrued instruction state,
7. `resolve_market` requires the market state to be **already accrued through the resolution slot** (`slot_last == current_slot == now_slot`) before the zero-funding settlement transition, eliminating retroactive funding erasure,
8. resolved positive-payout readiness is now defined by an explicit exact aggregate `neg_pnl_account_count`, eliminating any need for O(n) snapshot-time scans,
9. whole-only live flat conversion now names the exact helper sequence (`consume_released_pnl` then `set_capital`),
10. the instruction-local touched-account set MUST never silently truncate; capacity overflow MUST fail conservatively,
11. pure-capital no-insurance-draw is now scoped explicitly to pure capital-flow instructions, so flat PnL cleanup may absorb realized losses without ambiguity,
12. the K/F settlement helper now explicitly requires at least 256-bit exact intermediates, or a formally equivalent exact method.

The engine core keeps only:

- one **scheduled** reserve bucket plus one **pending** reserve bucket per live account,
- `PNL_matured_pos_tot`,
- the global trade haircut `g`,
- the matured-profit haircut `h`,
- the exact trade-open counterfactual approval metric `Eq_trade_open_raw_i`,
- capital, fee-debt, and insurance accounting,
- lazy A/K/F settlement,
- liquidation and reset mechanics,
- resolved-market local reconciliation, shared positive-payout snapshot capture, and terminal close.

The following policy inputs are wrapper-owned and are **not** computed by the engine core:

- the warmup horizon chosen for a live accrued instruction that may create new reserve,
- any optional wrapper-owned per-account fee policy beyond engine-native trading and liquidation fees,
- the funding rate applied to the elapsed live interval,
- any public execution-price admissibility policy,
- any mark-EWMA or premium-funding model.

The engine validates bounds on those wrapper inputs where applicable, but it does not derive them.

---

## 0. Security goals

The engine MUST provide the following properties.

1. **Protected principal for flat accounts:** an account with effective position `0` MUST NOT have its protected principal directly reduced by another account’s insolvency.
2. **Explicit open-position ADL eligibility:** accounts with open positions MAY be subject to deterministic protocol ADL if they are on the eligible opposing side of a bankrupt liquidation. ADL MUST operate through explicit protocol state, not hidden execution.
3. **Oracle-manipulation safety for extraction:** profits created by short-lived oracle distortion MUST NOT immediately dilute the matured-profit haircut denominator `h`, immediately become withdrawable principal, or immediately satisfy withdrawal or principal-conversion approval checks.
4. **Bounded trade reuse of positive PnL:** fresh positive PnL MAY support the generating account’s own risk-increasing trades only through the global trade haircut `g`. Aggregate positive PnL admitted through `g` MUST NOT exceed current `Residual`.
5. **No same-trade bootstrap from positive slippage:** a candidate trade’s own positive execution-slippage PnL MUST NOT be allowed to make that same trade pass a risk-increasing initial-margin check.
6. **No retroactive maturity inheritance:** fresh positive reserve added at slot `t` MUST NOT inherit time already elapsed on an older scheduled reserve bucket.
7. **No restart of older scheduled reserve:** adding new positive reserve to an account MUST NOT reset the scheduled bucket’s `sched_start_slot`, `sched_horizon`, `sched_anchor_q`, or already accrued maturity progress.
8. **Bounded warmup state:** each live account MUST use at most one scheduled reserve bucket and at most one pending reserve bucket.
9. **Conservative pending semantics:** the pending bucket MAY be more conservative than exact per-increment aging, but it MUST NEVER mature faster than its own stored horizon, and it MUST NEVER accelerate release of the older scheduled bucket.
10. **Profit-first haircuts:** when the system is undercollateralized, haircuts MUST apply to junior profit claims before any protected principal of flat accounts is impacted.
11. **Conservation:** the engine MUST NOT create withdrawable claims exceeding vault tokens, except for explicitly bounded rounding slack.
12. **Live-operation liveness:** on live markets, the engine MUST NOT require `OI == 0`, a global scan, a canonical account-order prefix, or manual admin recovery before a user can safely settle, deposit, withdraw, trade, liquidate, repay fee debt, reclaim, or make keeper progress.
13. **Resolved-close liveness split:** after a resolved account is locally reconciled, an account with `PNL_i <= 0` MUST be closable immediately; an account with `PNL_i > 0` MAY wait for global terminal-readiness and shared snapshot capture before payout.
14. **No zombie poisoning of the matured-profit haircut:** non-interacting accounts MUST NOT indefinitely pin the matured-profit haircut denominator `h` with fresh unwarmed PnL. Touched accounts MUST make warmup progress.
15. **Funding, mark, and ADL exactness under laziness:** any quantity whose correct value depends on the position held over an interval MUST be represented through A/K/F side indices or a formally equivalent event-segmented method. Integer rounding at settlement MUST NOT mint positive aggregate claims.
16. **No hidden protocol MM:** the protocol MUST NOT secretly internalize user flow against an undisclosed residual inventory.
17. **Defined recovery from precision stress:** the engine MUST define deterministic recovery when side precision is exhausted. It MUST NOT rely on assertion failure, silent overflow, or permanent `DrainOnly` states.
18. **No sequential quantity dependency:** same-epoch account settlement MUST be fully local. It MAY depend on the account’s own stored basis and current global side state, but MUST NOT require a canonical-order prefix or global carry cursor.
19. **Protocol-fee neutrality:** explicit protocol fees MUST either be collected into `I` immediately or tracked as account-local fee debt up to the account’s collectible capital-plus-fee-debt limit. Any explicit fee amount beyond that collectible limit MUST be dropped rather than socialized through `h`, through `g`, or inflated into bankruptcy deficit `D`.
20. **Strict risk-reducing neutrality uses actual fee impact:** any “fee-neutral” strict risk-reducing comparison MUST add back the account’s **actual applied fee-equity impact**, not the nominal requested fee amount.
21. **Synthetic liquidation price integrity:** a synthetic liquidation close MUST execute at the current oracle mark with zero execution-price slippage. Any liquidation penalty MUST be represented only by explicit fee state.
22. **Loss seniority over engine-native protocol fees:** when a trade or a non-bankruptcy liquidation realizes trading losses for an account, those losses are senior to engine-native trade and liquidation fee collection from that same local capital state.
23. **Deterministic overflow handling:** any arithmetic condition that is not proven unreachable by the numeric bounds MUST have a deterministic fail-safe or bounded fallback path. Silent wrap, unchecked panic, and undefined truncation are forbidden.
24. **Finite-capacity liveness:** because account capacity is finite, the engine MUST provide permissionless dead-account reclamation or equivalent slot reuse so abandoned empty accounts and flat dust accounts below the live-balance floor cannot permanently exhaust capacity.
25. **Permissionless off-chain keeper compatibility:** candidate discovery MAY be performed entirely off chain. The engine MUST expose exact current-state shortlist processing and targeted per-account settle, liquidate, reclaim, or resolved-close paths so any permissionless keeper can make liquidation and reset progress without any required on-chain phase-1 scan.
26. **No pure-capital insurance draw without accrual:** pure capital-flow instructions (`deposit`, `deposit_fee_credits`, `top_up_insurance_fund`, `charge_account_fee`) that do not call `accrue_market_to` MUST NOT decrement `I` or record uninsured protocol loss.
27. **Configuration immutability within a market instance:** warmup bounds, trade-fee, margin, liquidation, insurance-floor, and live-balance-floor parameters MUST remain fixed for the lifetime of a market instance unless a future revision defines an explicit safe update procedure.
28. **Scheduled-bucket exactness:** the active scheduled reserve bucket MUST mature according to its stored `sched_horizon` up to the required integer flooring and reserve-loss caps.
29. **Resolved-market close exactness:** resolved-market close MUST be defined through canonical helpers. It MUST NOT rely on direct zero-writes that bypass `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, reserve state, or reset counters.
30. **Path-independent touched-account finalization:** flat auto-conversion and fee-debt sweep on live touched accounts MUST depend only on the post-live touched state and the shared conversion snapshot, not on whether the instruction was single-touch or multi-touch.
31. **No resolved payout race:** resolved accounts with positive claims MUST NOT be terminally paid out until stale-account reconciliation is complete across both sides and the shared resolved-payout snapshot is locked.
32. **Path-independent resolved positive payouts:** once stale-account reconciliation is complete and terminal payout becomes unlocked, all positive resolved payouts MUST use one shared resolved-payout snapshot so caller order cannot improve the payout ratio.
33. **Bounded resolved settlement price:** the resolved settlement price used in `resolve_market` MUST remain within an immutable deviation band of the last live effective mark `P_last`.
34. **No permissionless haircut realization of flat released profit:** automatic flat conversion in live instructions MUST occur only at a whole snapshot (`h = 1`). Any lossy conversion of released profit under `h < 1` MUST be an explicit user action.
35. **No retroactive funding erasure at resolution:** `resolve_market` MUST only operate on a market state already accrued through the resolution slot, so the zero-funding settlement transition cannot erase elapsed live funding.
36. **No silent touched-set truncation:** every account touched by live local-touch MUST either be recorded for end-of-instruction finalization or the instruction MUST fail conservatively.
37. **No valid-price sentinel overloading:** no strictly positive price value may be used as an “uninitialized” sentinel for `P_last`, `fund_px_last`, or any other economically meaningful stored price.

**Atomic execution model:** every top-level external instruction defined in §9 MUST be atomic. If any required precondition, checked-arithmetic guard, or conservative-failure condition fails, the instruction MUST roll back all state mutations performed since that instruction began.

---

## 1. Types, units, scaling, bounds, and exact arithmetic

### 1.1 Amounts

- `u128` unsigned amounts are denominated in quote-token atomic units, positive-PnL aggregates, open interest, fixed-point position magnitudes, and bounded fee amounts.
- `i128` signed amounts represent realized PnL, K-space liabilities, funding-index snapshots, and fee-credit balances.
- `wide_signed` means any transient exact signed intermediate domain wider than `i128` (for example `i256`) or an equivalent exact comparison-preserving construction.
- All persistent state MUST fit natively into 128-bit boundaries. Emulated wide integers are permitted only within transient intermediate math steps.

### 1.2 Prices and internal positions

- `POS_SCALE = 1_000_000`.
- `price: u64` is quote-token atomic units per `1` base.
- Every external price input, including `oracle_price`, `exec_price`, `resolved_price`, and any stored funding-price sample, MUST satisfy `0 < price <= MAX_ORACLE_PRICE`.
- The engine stores position bases as signed fixed-point base quantities:
  - `basis_pos_q_i: i128`, units `(base * POS_SCALE)`.
- Oracle notional:
  - `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`.
- Trade fees use executed size:
  - `trade_notional = mul_div_floor_u128(size_q, exec_price, POS_SCALE)`.

### 1.3 A/K/F scales

- `ADL_ONE = 1_000_000`.
- `A_side` is dimensionless and scaled by `ADL_ONE`.
- `K_side` has units `(ADL scale) * (quote atomic units per 1 base)`.
- `FUNDING_DEN = 1_000_000_000`.
- `F_side_num` has units `(ADL scale) * (quote atomic units per 1 base) * FUNDING_DEN`.

### 1.4 Normative bounds

The following bounds are normative and MUST be enforced.

- `MAX_VAULT_TVL = 10_000_000_000_000_000`
- `MAX_ORACLE_PRICE = 1_000_000_000_000`
- `MAX_POSITION_ABS_Q = 100_000_000_000_000`
- `MAX_TRADE_SIZE_Q = MAX_POSITION_ABS_Q`
- `MAX_OI_SIDE_Q = 100_000_000_000_000`
- `MAX_ACCOUNT_NOTIONAL = 100_000_000_000_000_000_000`
- `MAX_PROTOCOL_FEE_ABS = 1_000_000_000_000_000_000_000_000_000_000_000_000`
- `MAX_ABS_FUNDING_E9_PER_SLOT = 1_000_000_000`
- `MAX_TRADING_FEE_BPS = 10_000`
- `MAX_INITIAL_BPS = 10_000`
- `MAX_MAINTENANCE_BPS = 10_000`
- `MAX_LIQUIDATION_FEE_BPS = 10_000`
- `MAX_FUNDING_DT = 65_535`
- `MAX_MATERIALIZED_ACCOUNTS = 1_000_000`
- `MAX_ACTIVE_POSITIONS_PER_SIDE` MUST be finite and MUST NOT exceed `MAX_MATERIALIZED_ACCOUNTS`
- `MAX_ACCOUNT_POSITIVE_PNL = 100_000_000_000_000_000_000_000_000_000_000`
- `MAX_PNL_POS_TOT = 100_000_000_000_000_000_000_000_000_000_000_000_000`
- `MIN_A_SIDE = 1_000`
- `MAX_WARMUP_SLOTS = 18_446_744_073_709_551_615`
- `MAX_RESOLVE_PRICE_DEVIATION_BPS = 10_000`
- `0 <= I_floor <= MAX_VAULT_TVL`
- `0 <= min_liquidation_abs <= liquidation_fee_cap <= MAX_PROTOCOL_FEE_ABS`

Configured values MUST satisfy:

- `0 < MIN_INITIAL_DEPOSIT <= MAX_VAULT_TVL`
- `0 < MIN_NONZERO_MM_REQ < MIN_NONZERO_IM_REQ <= MIN_INITIAL_DEPOSIT`
- `0 <= maintenance_bps <= initial_bps <= MAX_INITIAL_BPS`
- `0 <= H_min <= H_max <= MAX_WARMUP_SLOTS`
- for live accrued instructions, `H_lock` MUST satisfy either `H_lock == 0` or `H_min <= H_lock <= H_max`
- `0 <= resolve_price_deviation_bps <= MAX_RESOLVE_PRICE_DEVIATION_BPS`
- `A_side > 0` whenever `OI_eff_side > 0` and the side is still representable

If the deployment also defines a stale-market resolution delay `permissionless_resolve_stale_slots`, market initialization MUST additionally require:

- `H_max <= permissionless_resolve_stale_slots`

### 1.5 Trusted time and oracle requirements

- `now_slot` in every top-level instruction MUST come from trusted runtime slot metadata or an equivalent trusted source.
- `oracle_price` MUST come from a validated configured oracle feed.
- Any helper or instruction that accepts `now_slot` MUST require `now_slot >= current_slot`.
- Any call to `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` MUST require `now_slot >= slot_last`.
- `current_slot` and `slot_last` MUST be monotonically nondecreasing.
- The engine MUST NOT overload any strictly positive price value as an uninitialized sentinel for `P_last`, `fund_px_last`, or any equivalent stored price field. If an implementation needs an initialization flag, it MUST use a separate dedicated state predicate.

### 1.6 Required exact helpers

Implementations MUST provide exact checked helpers for at least:

- checked `add`, `sub`, and `mul` on `u128` and `i128`,
- checked cast helpers,
- exact conservative signed floor division,
- exact floor and ceil multiply-divide helpers,
- `fee_debt_u128_checked(fee_credits_i)`,
- `fee_credit_headroom_u128_checked(fee_credits_i)`,
- `wide_signed_mul_div_floor_from_kf_pair(abs_basis, k_then, k_now, f_then, f_now, den)`, implemented with at least exact 256-bit signed intermediates or a formally equivalent exact method.

### 1.7 Arithmetic requirements

The engine MUST satisfy all of the following.

1. Every product involving `A_side`, `K_side`, `F_side_num`, `k_snap_i`, `f_snap_i`, `basis_pos_q_i`, `effective_pos_q(i)`, `price`, the raw funding numerator `fund_px_0 * funding_rate_e9_per_slot * dt_sub`, trade-haircut numerators, trade-open counterfactual positive-aggregate numerators, scheduled-bucket release numerators, or ADL deltas MUST use checked arithmetic.
2. When `funding_rate_e9_per_slot != 0` and `dt > 0`, `accrue_market_to` MUST split `dt` into sub-steps each `<= MAX_FUNDING_DT`. Mark-to-market is applied once before the funding loop.
3. The conservation check `V >= C_tot + I` and any `Residual` computation MUST use checked addition for `C_tot + I`.
4. Signed division with positive denominator MUST use exact conservative floor division.
5. Exact multiply-divide helpers MUST return the exact quotient even when the exact product exceeds native `u128`, provided the final quotient fits.
6. `PendingWarmupTot = PNL_pos_tot - PNL_matured_pos_tot` MUST use checked subtraction.
7. Haircut paths `floor(ReleasedPos_i * h_num / h_den)`, `floor(PosPNL_i * g_num / g_den)`, and the exact candidate-open trade-haircut path of §3.4 MUST use exact multiply-divide helpers.
8. Funding sub-steps MUST use the same exact `fund_num_step = fund_px_0 * funding_rate_e9_per_slot * dt_sub` value for both sides’ `F_side_num` deltas. The engine MUST NOT floor-divide `fund_num_step` inside `accrue_market_to`.
9. `K_side` and `F_side_num` are cumulative across epochs. Implementations MUST use checked arithmetic and fail conservatively on persistent `i128` overflow.
10. Same-epoch or epoch-mismatch settlement MUST combine `K_side` and `F_side_num` through the exact helper `wide_signed_mul_div_floor_from_kf_pair`.
11. The ADL quote-deficit path MUST compute `delta_K_abs = ceil(D_rem * A_old * POS_SCALE / OI_before)` using exact wide arithmetic.
12. If a K-index delta magnitude is representable but `K_opp + delta_K_exact` overflows `i128`, the engine MUST route `D_rem` through `record_uninsured_protocol_loss` while still continuing quantity socialization.
13. `PNL_i` MUST be maintained in `[i128::MIN + 1, i128::MAX]`, and `fee_credits_i` in `[i128::MIN + 1, 0]`.
14. Every decrement of `stored_pos_count_*`, `stale_account_count_*`, or `phantom_dust_bound_*_q` MUST use checked subtraction.
15. Every increment of `stored_pos_count_*`, `phantom_dust_bound_*_q`, `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, `V`, or `I` MUST use checked addition and MUST enforce the relevant bound.
16. `trade_notional <= MAX_ACCOUNT_NOTIONAL` MUST be enforced before charging trade fees.
17. Any out-of-range price input, invalid oracle read, invalid `H_lock`, invalid `funding_rate_e9_per_slot`, or non-monotonic slot input MUST fail conservatively before state mutation.
18. `charge_fee_to_insurance` MUST cap its applied fee at the account’s exact collectible capital-plus-fee-debt headroom. It MUST never set `fee_credits_i < -(i128::MAX)`.
19. Any direct fee-credit repayment path MUST cap its applied amount at the exact current `FeeDebt_i`. It MUST never set `fee_credits_i > 0`.
20. Any direct insurance top-up or direct fee-credit repayment path that increases `V` or `I` MUST use checked addition and MUST enforce `MAX_VAULT_TVL`.
21. Scheduled- and pending-bucket mutations MUST preserve the invariants of §2.1 and MUST use checked arithmetic.
22. The exact counterfactual trade-open computation MUST recompute the account’s positive-PnL contribution and the global positive-PnL aggregate with the candidate trade’s own positive slippage gain removed.
23. Any wrapper-owned fee amount routed through the canonical helper MUST satisfy `fee_abs <= MAX_PROTOCOL_FEE_ABS`.
24. Fresh reserve MUST NOT be merged into an older scheduled bucket unless that bucket was itself created in the current slot, has the same `H_lock`, and has `sched_release_q == 0`.
25. Pending-bucket horizon updates MUST be monotone nondecreasing with `pending_horizon_i = max(pending_horizon_i, H_lock)` whenever new reserve is merged into an existing pending bucket.
26. If `reserve_mode` does not create new reserve (`ImmediateRelease` or `UseHLock(0)`), `PNL_matured_pos_tot` MUST increase only by the true newly released increment.
27. Funding exactness MUST NOT depend on a bare global remainder with no per-account snapshot. Any retained fractional precision across calls MUST be represented through `F_side_num` and `f_snap_i`.
28. Any strict risk-reducing fee-neutral comparison MUST add back `fee_equity_impact_i`, not nominal fee.
29. Any helper precondition reachable from a top-level instruction MUST fail conservatively rather than panic or assert on caller-controlled inputs or mutable market state.
30. The instruction-local touched-account set MUST never silently drop an account; if capacity is exceeded, the instruction MUST fail conservatively.
31. `phantom_dust_bound_long_q` and `phantom_dust_bound_short_q` are bounded by `u128` representability; any attempted overflow is a conservative failure.

---

## 2. State model

### 2.1 Account state

For each materialized account `i`, the engine stores at least:

- `C_i: u128` — protected principal
- `PNL_i: i128` — realized PnL claim
- `R_i: u128` — total reserved positive PnL, with `0 <= R_i <= max(PNL_i, 0)`
- `basis_pos_q_i: i128`
- `a_basis_i: u128`
- `k_snap_i: i128`
- `f_snap_i: i128`
- `epoch_snap_i: u64`
- `fee_credits_i: i128`

Each live account additionally stores at most two reserve segments.

**Scheduled reserve bucket** (older bucket, matures linearly):

- `sched_present_i: bool`
- `sched_remaining_q_i: u128`
- `sched_anchor_q_i: u128`
- `sched_start_slot_i: u64`
- `sched_horizon_i: u64`
- `sched_release_q_i: u128`

**Pending reserve bucket** (newest bucket, does not mature while pending):

- `pending_present_i: bool`
- `pending_remaining_q_i: u128`
- `pending_horizon_i: u64`

Derived local quantities on a touched state:

- `PosPNL_i = max(PNL_i, 0)`
- if `market_mode == Live`, `ReleasedPos_i = PosPNL_i - R_i`
- if `market_mode == Resolved`, `ReleasedPos_i = PosPNL_i`
- `FeeDebt_i = fee_debt_u128_checked(fee_credits_i)`

Reserve invariants on live markets:

- `R_i = (sched_remaining_q_i if sched_present_i else 0) + (pending_remaining_q_i if pending_present_i else 0)`
- if `sched_present_i`:
  - `0 < sched_anchor_q_i`
  - `0 < sched_remaining_q_i <= sched_anchor_q_i`
  - `H_min <= sched_horizon_i <= H_max`
  - `0 <= sched_release_q_i <= sched_anchor_q_i`
- if `pending_present_i`:
  - `0 < pending_remaining_q_i`
  - `H_min <= pending_horizon_i <= H_max`
- the pending bucket is always economically newer than the scheduled bucket
- if `R_i == 0`, both buckets MUST be absent
- if `sched_present_i == false`, the pending bucket MAY still be present
- the pending bucket MUST NEVER auto-mature while pending
- when promoted, the pending bucket becomes the scheduled bucket with:
  - `sched_remaining_q = pending_remaining_q`
  - `sched_anchor_q = pending_remaining_q`
  - `sched_start_slot = current_slot`
  - `sched_horizon = pending_horizon`
  - `sched_release_q = 0`
- if `market_mode == Resolved`, reserve storage is economically inert and MUST be cleared by `prepare_account_for_resolved_touch(i)` before any resolved-account touch mutates `PNL_i`

Fee-credit bounds:

- `fee_credits_i` MUST be initialized to `0`
- the engine MUST maintain `-(i128::MAX) <= fee_credits_i <= 0`
- `fee_credits_i == i128::MIN` is forbidden

### 2.2 Global engine state

The engine stores at least:

- `V: u128`
- `I: u128`
- `I_floor: u128`
- `current_slot: u64`
- `P_last: u64`
- `slot_last: u64`
- `fund_px_last: u64`
- `A_long: u128`
- `A_short: u128`
- `K_long: i128`
- `K_short: i128`
- `F_long_num: i128`
- `F_short_num: i128`
- `epoch_long: u64`
- `epoch_short: u64`
- `K_epoch_start_long: i128`
- `K_epoch_start_short: i128`
- `F_epoch_start_long_num: i128`
- `F_epoch_start_short_num: i128`
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
- `materialized_account_count: u64`
- `neg_pnl_account_count: u64` — exact number of materialized accounts with `PNL_i < 0`
- `C_tot: u128 = Σ C_i`
- `PNL_pos_tot: u128 = Σ max(PNL_i, 0)`
- `PNL_matured_pos_tot: u128 = Σ ReleasedPos_i`

Resolved-market state:

- `market_mode ∈ {Live, Resolved}`
- `resolved_price: u64`
- `resolved_slot: u64`
- `resolved_payout_snapshot_ready: bool`
- `resolved_payout_h_num: u128`
- `resolved_payout_h_den: u128`

Derived global quantity:

- `PendingWarmupTot = PNL_pos_tot - PNL_matured_pos_tot`

Global invariants:

- `PNL_matured_pos_tot <= PNL_pos_tot <= MAX_PNL_POS_TOT`
- `C_tot <= V <= MAX_VAULT_TVL`
- `I <= V`
- `0 <= neg_pnl_account_count <= materialized_account_count <= MAX_MATERIALIZED_ACCOUNTS`
- `F_long_num` and `F_short_num` MUST remain representable as `i128`
- if `market_mode == Resolved`, `resolved_price > 0`
- if `resolved_payout_snapshot_ready == false`, then `resolved_payout_h_num == 0` and `resolved_payout_h_den == 0`
- if `resolved_payout_snapshot_ready == true`, then `resolved_payout_h_num <= resolved_payout_h_den`

### 2.3 Instruction context

Every top-level live instruction that uses the standard lifecycle MUST initialize a fresh ephemeral context `ctx` with at least:

- `pending_reset_long: bool`
- `pending_reset_short: bool`
- `H_lock_shared: u64`
- `touched_accounts[]` — a deduplicated instruction-local list of touched account storage indices

If an implementation uses a fixed-capacity touched set, that capacity MUST be sufficient for the maximum number of distinct accounts any single top-level instruction in this revision can touch. If capacity would be exceeded, the instruction MUST fail conservatively. Silent truncation is forbidden.

### 2.4 Configuration immutability

No external instruction in this revision may change:

- `H_min`
- `H_max`
- `trading_fee_bps`
- `maintenance_bps`
- `initial_bps`
- `liquidation_fee_bps`
- `liquidation_fee_cap`
- `min_liquidation_abs`
- `MIN_INITIAL_DEPOSIT`
- `MIN_NONZERO_MM_REQ`
- `MIN_NONZERO_IM_REQ`
- `I_floor`
- `resolve_price_deviation_bps`
- `MAX_ACTIVE_POSITIONS_PER_SIDE`

### 2.5 Materialized-account capacity

The engine MUST track the number of currently materialized account slots. That count MUST NOT exceed `MAX_MATERIALIZED_ACCOUNTS`.

A missing account is one whose slot is not currently materialized. Missing accounts MUST NOT be auto-materialized by `settle_account`, `withdraw`, `execute_trade`, `liquidate`, `resolve_market`, `force_close_resolved`, or `keeper_crank`.

Only the following path MAY materialize a missing account:

- `deposit(i, amount, now_slot)` with `amount >= MIN_INITIAL_DEPOSIT`.

### 2.6 Canonical zero-position defaults

The canonical zero-position account defaults are:

- `basis_pos_q_i = 0`
- `a_basis_i = ADL_ONE`
- `k_snap_i = 0`
- `f_snap_i = 0`
- `epoch_snap_i = 0`

### 2.7 Account materialization

`materialize_account(i)` MAY succeed only if the account is currently missing and materialized-account capacity remains below `MAX_MATERIALIZED_ACCOUNTS`.

On success, it MUST:

- increment `materialized_account_count`,
- leave `neg_pnl_account_count` unchanged because the new account starts with `PNL_i = 0`,
- set `C_i = 0`,
- set `PNL_i = 0`,
- set `R_i = 0`,
- set canonical zero-position defaults,
- set `fee_credits_i = 0`,
- leave both reserve buckets absent.

### 2.8 Permissionless empty- or flat-dust-account reclamation

The engine MUST provide a permissionless reclamation path `reclaim_empty_account(i, now_slot)`.

It MAY succeed only if all of the following hold:

- account `i` is materialized,
- trusted `now_slot >= current_slot`,
- `0 <= C_i < MIN_INITIAL_DEPOSIT`,
- `PNL_i == 0`,
- `R_i == 0`,
- both reserve buckets are absent,
- `basis_pos_q_i == 0`,
- `fee_credits_i <= 0`.

On success, it MUST:

- if `C_i > 0`:
  - `dust = C_i`
  - `set_capital(i, 0)`
  - `I = checked_add_u128(I, dust)`
- forgive any negative `fee_credits_i`
- reset local fields to canonical zero
- mark the slot missing or reusable
- decrement `materialized_account_count`
- require `neg_pnl_account_count` is unchanged (the reclaim precondition already requires `PNL_i == 0`)

### 2.9 Initial market state

At market initialization, the engine MUST set:

- `V = 0`
- `I = 0`
- `I_floor = configured I_floor`
- `C_tot = 0`
- `PNL_pos_tot = 0`
- `PNL_matured_pos_tot = 0`
- `current_slot = init_slot`
- `slot_last = init_slot`
- `P_last = init_oracle_price`
- `fund_px_last = init_oracle_price`
- `A_long = ADL_ONE`, `A_short = ADL_ONE`
- `K_long = 0`, `K_short = 0`
- `F_long_num = 0`, `F_short_num = 0`
- `epoch_long = 0`, `epoch_short = 0`
- `K_epoch_start_long = 0`, `K_epoch_start_short = 0`
- `F_epoch_start_long_num = 0`, `F_epoch_start_short_num = 0`
- `OI_eff_long = 0`, `OI_eff_short = 0`
- `mode_long = Normal`, `mode_short = Normal`
- `stored_pos_count_long = 0`, `stored_pos_count_short = 0`
- `stale_account_count_long = 0`, `stale_account_count_short = 0`
- `phantom_dust_bound_long_q = 0`, `phantom_dust_bound_short_q = 0`
- `materialized_account_count = 0`
- `neg_pnl_account_count = 0`
- `market_mode = Live`
- `resolved_price = 0`
- `resolved_slot = init_slot`
- `resolved_payout_snapshot_ready = false`
- `resolved_payout_h_num = 0`
- `resolved_payout_h_den = 0`

### 2.10 Side modes and reset lifecycle

A side may be in one of:

- `Normal`
- `DrainOnly`
- `ResetPending`

`begin_full_drain_reset(side)` MAY succeed only if `OI_eff_side == 0`. It MUST:

1. set `K_epoch_start_side = K_side`
2. set `F_epoch_start_side_num = F_side_num`
3. increment `epoch_side` by exactly `1`
4. set `A_side = ADL_ONE`
5. set `stale_account_count_side = stored_pos_count_side`
6. set `phantom_dust_bound_side_q = 0`
7. set `mode_side = ResetPending`

`finalize_side_reset(side)` MAY succeed only if:

- `mode_side == ResetPending`
- `OI_eff_side == 0`
- `stale_account_count_side == 0`
- `stored_pos_count_side == 0`

On success, it MUST set `mode_side = Normal`.

`maybe_finalize_ready_reset_sides_before_oi_increase()` MUST finalize any already-ready reset side before any OI-increasing operation checks side modes.

### 2.10.1 Epoch-gap invariant

For every materialized account with `basis_pos_q_i != 0` on side `s`, the engine MUST maintain exactly one of:

- `epoch_snap_i == epoch_s`, or
- `mode_s == ResetPending` and `epoch_snap_i + 1 == epoch_s`.

Epoch gaps larger than `1` are forbidden.

---

## 3. Solvency, haircuts, and live equity

### 3.1 Residual backing

Define:

- `senior_sum = checked_add_u128(C_tot, I)`
- `Residual = max(0, V - senior_sum)`

Invariant: the engine MUST maintain `V >= senior_sum`.

### 3.2 Positive-PnL aggregates

Define:

- `PosPNL_i = max(PNL_i, 0)`
- if `market_mode == Live`, `ReleasedPos_i = PosPNL_i - R_i`
- if `market_mode == Resolved`, `ReleasedPos_i = PosPNL_i`
- on live markets, `PendingWarmupTot = PNL_pos_tot - PNL_matured_pos_tot = Σ R_i`

Reserved fresh positive PnL increases `PNL_pos_tot` immediately but MUST NOT increase `PNL_matured_pos_tot` until warmup release.

### 3.3 Matured withdrawal and conversion haircut `h`

Let:

- if `PNL_matured_pos_tot == 0`, define `h = 1`
- else:
  - `h_num = min(Residual, PNL_matured_pos_tot)`
  - `h_den = PNL_matured_pos_tot`

For account `i`:

- if `PNL_matured_pos_tot == 0`, `PNL_eff_matured_i = ReleasedPos_i`
- else `PNL_eff_matured_i = mul_div_floor_u128(ReleasedPos_i, h_num, h_den)`

### 3.4 Trade-collateral haircut `g`

Let:

- if `PNL_pos_tot == 0`, define `g = 1`
- else:
  - `g_num = min(Residual, PNL_pos_tot)`
  - `g_den = PNL_pos_tot`

For account `i`:

- if `PNL_pos_tot == 0`, `PNL_eff_trade_i = PosPNL_i`
- else `PNL_eff_trade_i = mul_div_floor_u128(PosPNL_i, g_num, g_den)`

Aggregate bound:

- `Σ PNL_eff_trade_i <= g_num <= Residual`

### 3.5 Live equity lanes

All raw equity comparisons in this section MUST use an exact widened signed domain.

For account `i` on a touched state:

- `Eq_withdraw_raw_i = (C_i as wide_signed) + min(PNL_i, 0) + (PNL_eff_matured_i as wide_signed) - (FeeDebt_i as wide_signed)`
- `Eq_trade_raw_i = (C_i as wide_signed) + min(PNL_i, 0) + (PNL_eff_trade_i as wide_signed) - (FeeDebt_i as wide_signed)`
- `Eq_maint_raw_i = (C_i as wide_signed) + (PNL_i as wide_signed) - (FeeDebt_i as wide_signed)`

Derived clamped quantity:

- `Eq_net_i = max(0, Eq_maint_raw_i)`

For candidate trade approval only, define:

- `candidate_trade_pnl_i` = signed execution-slippage PnL created by the candidate trade
- `TradeGain_i_candidate = max(candidate_trade_pnl_i, 0) as u128`
- `PNL_trade_open_i = PNL_i - (TradeGain_i_candidate as i128)`
- `PosPNL_trade_open_i = max(PNL_trade_open_i, 0)`

Counterfactual positive aggregate:

- `PNL_pos_tot_trade_open_i = checked_add_u128(checked_sub_u128(PNL_pos_tot, PosPNL_i), PosPNL_trade_open_i)`

Counterfactual trade haircut:

- if `PNL_pos_tot_trade_open_i == 0`, `PNL_eff_trade_open_i = PosPNL_trade_open_i`
- else:
  - `g_open_num_i = min(Residual, PNL_pos_tot_trade_open_i)`
  - `g_open_den_i = PNL_pos_tot_trade_open_i`
  - `PNL_eff_trade_open_i = mul_div_floor_u128(PosPNL_trade_open_i, g_open_num_i, g_open_den_i)`

Then:

- `Eq_trade_open_raw_i = (C_i as wide_signed) + min(PNL_trade_open_i, 0) + (PNL_eff_trade_open_i as wide_signed) - (FeeDebt_i as wide_signed)`

Interpretation:

- `Eq_withdraw_raw_i` is the extraction lane
- `Eq_trade_open_raw_i` is the only compliant risk-increasing trade approval metric
- `Eq_maint_raw_i` is the maintenance lane
- strict risk-reducing comparisons MUST use exact widened `Eq_maint_raw_i`, never a clamped net quantity

---

## 4. Canonical helpers

### 4.1 `set_capital(i, new_C)`

When changing `C_i`, the engine MUST update `C_tot` by the exact signed delta and then set `C_i = new_C`.

### 4.2 `set_position_basis_q(i, new_basis_pos_q)`

When changing stored `basis_pos_q_i` from `old` to `new`, the engine MUST update `stored_pos_count_long` and `stored_pos_count_short` exactly once using the sign flags of `old` and `new`, then write `basis_pos_q_i = new`.

Any transition that increments a side-count — including 0-to-nonzero attachments and sign flips — MUST enforce `MAX_ACTIVE_POSITIONS_PER_SIDE`.

### 4.3 `promote_pending_to_scheduled(i)`

Preconditions:

- `market_mode == Live`
- `current_slot` is already the trusted slot anchor for the current instruction state

Effects:

1. if `sched_present_i == true`, return
2. if `pending_present_i == false`, return
3. create the scheduled bucket:
   - `sched_present_i = true`
   - `sched_remaining_q_i = pending_remaining_q_i`
   - `sched_anchor_q_i = pending_remaining_q_i`
   - `sched_start_slot_i = current_slot`
   - `sched_horizon_i = pending_horizon_i`
   - `sched_release_q_i = 0`
4. clear the pending bucket

This helper MUST NOT change `R_i`.

### 4.4 `append_new_reserve(i, reserve_add, H_lock)`

Preconditions:

- `reserve_add > 0`
- `market_mode == Live`
- `H_lock > 0`
- `H_min <= H_lock <= H_max`
- `current_slot` is already the trusted slot anchor for the current instruction state

Effects:

1. if the scheduled bucket is absent and the pending bucket is present, call `promote_pending_to_scheduled(i)`
2. if the scheduled bucket is absent:
   - create a scheduled bucket with:
     - `sched_remaining_q = reserve_add`
     - `sched_anchor_q = reserve_add`
     - `sched_start_slot = current_slot`
     - `sched_horizon = H_lock`
     - `sched_release_q = 0`
3. else if the scheduled bucket is present, the pending bucket is absent, and all of the following hold:
   - `sched_start_slot == current_slot`
   - `sched_horizon == H_lock`
   - `sched_release_q == 0`
   then exact same-slot merge into the scheduled bucket is permitted:
   - `sched_remaining_q += reserve_add`
   - `sched_anchor_q += reserve_add`
4. else if the pending bucket is absent:
   - create a pending bucket with:
     - `pending_remaining_q = reserve_add`
     - `pending_horizon = H_lock`
5. else:
   - `pending_remaining_q += reserve_add`
   - `pending_horizon = max(pending_horizon, H_lock)`
6. set `R_i += reserve_add`

Normative consequences:

- fresh reserve never inherits elapsed time from an older scheduled bucket
- adding fresh reserve never resets the older scheduled bucket
- repeated additions only ever mutate the newest pending bucket once an older scheduled bucket exists

### 4.5 `apply_reserve_loss_newest_first(i, reserve_loss)`

Preconditions:

- `reserve_loss > 0`
- `reserve_loss <= R_i`
- `market_mode == Live`

Effects:

1. consume reserve from the pending bucket first, if present
2. then consume reserve from the scheduled bucket
3. require full consumption of `reserve_loss`
4. decrement `R_i` by the exact consumed amount
5. clear any now-empty bucket

### 4.6 `prepare_account_for_resolved_touch(i)`

Preconditions:

- `market_mode == Resolved`

Effects:

1. clear the scheduled bucket
2. clear the pending bucket
3. set `R_i = 0`
4. do **not** mutate `PNL_matured_pos_tot`

### 4.7 `set_pnl(i, new_PNL, reserve_mode)`

`reserve_mode ∈ {UseHLock(H_lock), ImmediateRelease, NoPositiveIncreaseAllowed}`.

Every persistent mutation of `PNL_i` after materialization MUST go through this helper. Whenever this helper changes the sign of `PNL_i` across zero, it MUST update `neg_pnl_account_count` exactly once:
- if `PNL_i < 0` before the write and `new_PNL >= 0`, decrement `neg_pnl_account_count`,
- if `PNL_i >= 0` before the write and `new_PNL < 0`, increment `neg_pnl_account_count`,
- otherwise leave `neg_pnl_account_count` unchanged.

Let:

- `old_pos = max(PNL_i, 0)`
- if `market_mode == Live`, `old_rel = old_pos - R_i`
- if `market_mode == Resolved`, require `R_i == 0` and set `old_rel = old_pos`
- `new_pos = max(new_PNL, 0)`
- `old_neg = (PNL_i < 0)`
- `new_neg = (new_PNL < 0)`

Procedure:

1. require `new_PNL != i128::MIN`
2. require `new_pos <= MAX_ACCOUNT_POSITIVE_PNL`
3. update `PNL_pos_tot` by the exact delta from `old_pos` to `new_pos`
4. require resulting `PNL_pos_tot <= MAX_PNL_POS_TOT`

If `new_pos > old_pos`:

5. `reserve_add = new_pos - old_pos`
6. set `PNL_i = new_PNL` and update `neg_pnl_account_count` according to `old_neg` and `new_neg`
7. if `reserve_mode == NoPositiveIncreaseAllowed`, fail conservatively
8. if `reserve_mode == ImmediateRelease`, add `reserve_add` to `PNL_matured_pos_tot` and return
9. if `reserve_mode == UseHLock(0)`, add `reserve_add` to `PNL_matured_pos_tot` and return
10. require `market_mode == Live`
11. call `append_new_reserve(i, reserve_add, H_lock)`
12. leave `PNL_matured_pos_tot` unchanged
13. require `PNL_matured_pos_tot <= PNL_pos_tot`
14. return

If `new_pos <= old_pos`:

15. `pos_loss = old_pos - new_pos`
16. if `market_mode == Live`:
   - `reserve_loss = min(pos_loss, R_i)`
   - if `reserve_loss > 0`, call `apply_reserve_loss_newest_first(i, reserve_loss)`
   - `matured_loss = pos_loss - reserve_loss`
17. if `market_mode == Resolved`:
   - require `R_i == 0`
   - `matured_loss = pos_loss`
18. if `matured_loss > 0`, subtract `matured_loss` from `PNL_matured_pos_tot`
19. set `PNL_i = new_PNL` and update `neg_pnl_account_count` according to `old_neg` and `new_neg`
20. if `new_pos == 0` and `market_mode == Live`, require `R_i == 0` and both buckets absent
21. require `PNL_matured_pos_tot <= PNL_pos_tot`

### 4.8 `consume_released_pnl(i, x)`

This helper removes only matured released positive PnL on a live account and MUST leave both reserve buckets unchanged.

Preconditions:

- `market_mode == Live`
- `0 < x <= ReleasedPos_i`

Effects:

1. decrease `PNL_i` by exactly `x`
2. decrease `PNL_pos_tot` by exactly `x`
3. decrease `PNL_matured_pos_tot` by exactly `x`
4. leave `R_i`, the scheduled bucket, and the pending bucket unchanged
5. require `PNL_matured_pos_tot <= PNL_pos_tot`

### 4.9 `advance_profit_warmup(i)`

Preconditions:

- `market_mode == Live`

Procedure:

1. if `R_i == 0`, require both buckets absent and return
2. if the scheduled bucket is absent and the pending bucket is present, call `promote_pending_to_scheduled(i)`
3. if the scheduled bucket is still absent, return
4. let `elapsed = current_slot - sched_start_slot`
5. let `sched_total = min(sched_anchor_q, floor(sched_anchor_q * elapsed / sched_horizon))`
6. require `sched_total >= sched_release_q`
7. `sched_increment = sched_total - sched_release_q`
8. `release = min(sched_remaining_q, sched_increment)`
9. if `release > 0`:
   - `sched_remaining_q -= release`
   - `R_i -= release`
   - `PNL_matured_pos_tot += release`
10. set `sched_release_q = sched_total`
11. if the scheduled bucket is now empty:
   - clear it
   - if the pending bucket is present, call `promote_pending_to_scheduled(i)`
12. if `R_i == 0`, require both buckets absent
13. require `PNL_matured_pos_tot <= PNL_pos_tot`

### 4.10 `attach_effective_position(i, new_eff_pos_q)`

This helper converts a current effective quantity into a new position basis at the current side state.

If discarding a same-epoch nonzero basis, it MUST first account for orphaned unresolved same-epoch quantity remainder by incrementing the appropriate phantom-dust bound when that remainder is nonzero.

If `new_eff_pos_q == 0`, it MUST:

- zero the stored basis via `set_position_basis_q(i, 0)`
- reset snapshots to canonical zero-position defaults

If `new_eff_pos_q != 0`, it MUST:

- require `abs(new_eff_pos_q) <= MAX_POSITION_ABS_Q`
- write the new basis via `set_position_basis_q(i, new_eff_pos_q)`
- set `a_basis_i = A_side(new_eff_pos_q)`
- set `k_snap_i = K_side(new_eff_pos_q)`
- set `f_snap_i = F_side_num(new_eff_pos_q)`
- set `epoch_snap_i = epoch_side(new_eff_pos_q)`

### 4.11 Phantom-dust helpers

- `inc_phantom_dust_bound(side)` increments by exactly `1` q-unit.
- `inc_phantom_dust_bound_by(side, amount_q)` increments by exactly `amount_q`.

### 4.12 `max_safe_flat_conversion_released(i, x_cap, h_num, h_den)`

This helper returns the largest `x_safe <= x_cap` such that converting `x_safe` released profit on a live flat account cannot make the account’s exact post-conversion raw maintenance equity negative.

Implementation law:

1. if `x_cap == 0`, return `0`
2. let `E_before = Eq_maint_raw_i` on the current exact state
3. if `E_before <= 0`, return `0`
4. if `h_den == 0` or `h_num == h_den`, return `x_cap`
5. let `haircut_loss_num = h_den - h_num`
6. return `min(x_cap, floor(E_before * h_den / haircut_loss_num))` using an exact capped multiply-divide or an equivalent exact wide comparison

### 4.13 `compute_trade_pnl(size_q, oracle_price, exec_price)`

For a bilateral trade where `size_q > 0` means account `a` buys base from account `b`, the execution-slippage PnL applied before fees MUST be:

- `trade_pnl_num = size_q * (oracle_price - exec_price)`
- `trade_pnl_a = floor_div_signed_conservative(trade_pnl_num, POS_SCALE)`
- `trade_pnl_b = -trade_pnl_a`

This helper MUST use checked signed arithmetic and exact conservative floor division.

### 4.14 `charge_fee_to_insurance(i, fee_abs) -> FeeChargeOutcome`

Preconditions:

- `fee_abs <= MAX_PROTOCOL_FEE_ABS`

Return value:

- `fee_paid_to_insurance_i`
- `fee_equity_impact_i`
- `fee_dropped_i`

Definitions:

- `fee_paid_to_insurance_i` = amount immediately paid out of capital into `I`
- `fee_equity_impact_i` = total actual reduction in the account’s raw equity from this fee application, equal to capital paid plus collectible fee debt added
- `fee_dropped_i = fee_abs - fee_equity_impact_i` = permanently uncollectible tail

Effects:

1. `debt_headroom = fee_credit_headroom_u128_checked(fee_credits_i)`
2. `collectible = checked_add_u128(C_i, debt_headroom)`
3. `fee_equity_impact_i = min(fee_abs, collectible)`
4. `fee_paid_to_insurance_i = min(fee_equity_impact_i, C_i)`
5. if `fee_paid_to_insurance_i > 0`:
   - `set_capital(i, C_i - fee_paid_to_insurance_i)`
   - `I = checked_add_u128(I, fee_paid_to_insurance_i)`
6. `fee_shortfall = fee_equity_impact_i - fee_paid_to_insurance_i`
7. if `fee_shortfall > 0`, subtract it from `fee_credits_i`
8. `fee_dropped_i = fee_abs - fee_equity_impact_i`

This helper MUST NOT mutate `PNL_i`, `PNL_pos_tot`, `PNL_matured_pos_tot`, reserve state, or any `K_side`.

### 4.15 Insurance-loss helpers

- `use_insurance_buffer(loss_abs)` spends insurance down to `I_floor` and returns the remainder.
- `record_uninsured_protocol_loss(loss_abs)` leaves the uncovered loss represented through `Residual` and junior haircuts.
- `absorb_protocol_loss(loss_abs)` = `use_insurance_buffer` then `record_uninsured_protocol_loss` if needed.

---

## 5. Unified A/K/F side-index mechanics

### 5.1 Eager-equivalent event law

For one side, a single eager global event on absolute fixed-point position `q_q >= 0` and realized PnL `p` has the form:

- `q_q' = α q_q`
- `p' = p + β * q_q / POS_SCALE`

The cumulative indices compose as:

- `A_new = A_old * α`
- `K_new = K_old + A_old * β`

### 5.2 `effective_pos_q(i)`

For an account with nonzero basis:

- let `s = side(basis_pos_q_i)`
- if `epoch_snap_i != epoch_s`, define `effective_pos_q(i) = 0`
- else `effective_abs_pos_q(i) = mul_div_floor_u128(abs(basis_pos_q_i), A_s, a_basis_i)`
- `effective_pos_q(i) = sign(basis_pos_q_i) * effective_abs_pos_q(i)`

### 5.2.1 Side-OI components

For any signed fixed-point position `q`:

- `OI_long_component(q) = max(q, 0) as u128`
- `OI_short_component(q) = max(-q, 0) as u128`

### 5.2.2 Exact bilateral trade side-OI after-values

For a bilateral trade with old and new effective positions for both counterparties:

- `OI_long_after_trade = (((OI_eff_long - old_long_a) - old_long_b) + new_long_a) + new_long_b`
- `OI_short_after_trade = (((OI_eff_short - old_short_a) - old_short_b) + new_short_a) + new_short_b`

These exact after-values MUST be used both for gating and for final writeback.

### 5.3 `settle_side_effects_live(i, H_lock)`

When touching account `i` on a live market:

1. if `basis_pos_q_i == 0`, return
2. let `s = side(basis_pos_q_i)`
3. let `den = checked_mul_u128(a_basis_i, POS_SCALE)`
4. if `epoch_snap_i == epoch_s`:
   - `q_eff_new = mul_div_floor_u128(abs(basis_pos_q_i), A_s, a_basis_i)`
   - `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i), k_snap_i, K_s, f_snap_i, F_s_num, den)`
   - `set_pnl(i, PNL_i + pnl_delta, UseHLock(H_lock))`
   - if `q_eff_new == 0`:
     - increment the appropriate phantom-dust bound
     - zero the basis
     - reset snapshots to canonical zero-position defaults
   - else:
     - update `k_snap_i`
     - update `f_snap_i`
     - update `epoch_snap_i`
5. else:
   - require `mode_s == ResetPending`
   - require `epoch_snap_i + 1 == epoch_s`
   - `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i), k_snap_i, K_epoch_start_s, f_snap_i, F_epoch_start_s_num, den)`
   - `set_pnl(i, PNL_i + pnl_delta, UseHLock(H_lock))`
   - zero the basis
   - decrement `stale_account_count_s`
   - reset snapshots

### 5.4 `settle_side_effects_resolved(i)`

When touching account `i` on a resolved market:

1. if `basis_pos_q_i == 0`, return
2. require stale one-epoch-lag conditions on its side
3. compute `pnl_delta` against `(K_epoch_start_s, F_epoch_start_s_num)`
4. `set_pnl(i, PNL_i + pnl_delta, ImmediateRelease)`
5. zero the basis
6. decrement `stale_account_count_s`
7. reset snapshots

### 5.5 `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`

Before any live operation that depends on current market state, the engine MUST call `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`.

This helper MUST:

1. require `market_mode == Live`
2. require trusted `now_slot >= slot_last`
3. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
4. require `abs(funding_rate_e9_per_slot) <= MAX_ABS_FUNDING_E9_PER_SLOT`
5. let `dt = now_slot - slot_last`
6. snapshot `OI_long_0 = OI_eff_long`, `OI_short_0 = OI_eff_short`, and `fund_px_0 = fund_px_last`
7. mark-to-market once:
   - `ΔP = oracle_price - P_last`
   - if `OI_long_0 > 0`, add `A_long * ΔP` to `K_long`
   - if `OI_short_0 > 0`, subtract `A_short * ΔP` from `K_short`
8. funding transfer:
   - if `funding_rate_e9_per_slot != 0` and `dt > 0` and both snapped OI sides are nonzero:
     - split `dt` into sub-steps `dt_sub <= MAX_FUNDING_DT`
     - for each sub-step:
       - `fund_num_step = fund_px_0 * funding_rate_e9_per_slot * dt_sub`
       - `F_long_num -= A_long * fund_num_step`
       - `F_short_num += A_short * fund_num_step`
9. update `slot_last = now_slot`
10. update `P_last = oracle_price`
11. update `fund_px_last = oracle_price`

### 5.6 `enqueue_adl(ctx, liq_side, q_close_q, D)`

Suppose a bankrupt liquidation from side `liq_side` leaves an uncovered deficit `D >= 0`. Let `opp = opposite(liq_side)`.

This helper MUST:

1. decrement `OI_eff_liq_side` by `q_close_q` if `q_close_q > 0`
2. spend insurance first: `D_rem = use_insurance_buffer(D)`
3. let `OI_before = OI_eff_opp`
4. if `OI_before == 0`:
   - if `D_rem > 0`, route it through `record_uninsured_protocol_loss`
   - if `OI_eff_long == 0` and `OI_eff_short == 0`, set both pending-reset flags true
   - return
5. if `OI_before > 0` and `stored_pos_count_opp == 0`:
   - require `q_close_q <= OI_before`
   - set `OI_eff_opp = OI_before - q_close_q`
   - if `D_rem > 0`, route it through `record_uninsured_protocol_loss`
   - if `OI_eff_long == 0` and `OI_eff_short == 0`, set both pending-reset flags true
   - return
6. otherwise:
   - require `q_close_q <= OI_before`
   - `A_old = A_opp`
   - `OI_post = OI_before - q_close_q`
7. if `D_rem > 0`:
   - compute `delta_K_abs = ceil(D_rem * A_old * POS_SCALE / OI_before)` using exact wide arithmetic
   - if the magnitude is non-representable or the signed `K_opp + delta_K_exact` overflows, route `D_rem` through `record_uninsured_protocol_loss`
   - else apply `K_opp += delta_K_exact` with `delta_K_exact = -delta_K_abs`
8. if `OI_post == 0`:
   - set `OI_eff_opp = 0`
   - set both pending-reset flags true
   - return
9. compute `A_candidate = floor(A_old * OI_post / OI_before)`
10. if `A_candidate > 0`:
   - set `A_opp = A_candidate`
   - set `OI_eff_opp = OI_post`
   - if `OI_post < OI_before`:
     - `N_opp = stored_pos_count_opp as u128`
     - `global_a_dust_bound = N_opp + ceil((OI_before + N_opp) / A_old)`
     - increment the appropriate phantom-dust bound by `global_a_dust_bound`
   - if `A_opp < MIN_A_SIDE`, set `mode_opp = DrainOnly`
   - return
11. if `A_candidate == 0` while `OI_post > 0`:
   - set `OI_eff_long = 0`
   - set `OI_eff_short = 0`
   - set both pending-reset flags true

### 5.7 `schedule_end_of_instruction_resets(ctx)`

This helper MUST be called exactly once at the end of every top-level instruction that can touch accounts, mutate side state, liquidate, or resolved-close.

Procedure:

1. **Bilateral-empty dust clearance**  
   If `stored_pos_count_long == 0` and `stored_pos_count_short == 0`:
   - `clear_bound_q = phantom_dust_bound_long_q + phantom_dust_bound_short_q`
   - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0) or (phantom_dust_bound_short_q > 0)`
   - if `has_residual_clear_work`:
     - require `OI_eff_long == OI_eff_short`
     - if `OI_eff_long <= clear_bound_q` and `OI_eff_short <= clear_bound_q`:
       - set `OI_eff_long = 0`
       - set `OI_eff_short = 0`
       - set both pending-reset flags true
     - else fail conservatively

2. **Unilateral-empty dust clearance, long side empty**  
   Else if `stored_pos_count_long == 0` and `stored_pos_count_short > 0`:
   - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0)`
   - if `has_residual_clear_work`:
     - require `OI_eff_long == OI_eff_short`
     - if `OI_eff_long <= phantom_dust_bound_long_q`:
       - set `OI_eff_long = 0`
       - set `OI_eff_short = 0`
       - set both pending-reset flags true
     - else fail conservatively

3. **Unilateral-empty dust clearance, short side empty**  
   Else if `stored_pos_count_short == 0` and `stored_pos_count_long > 0`:
   - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_short_q > 0)`
   - if `has_residual_clear_work`:
     - require `OI_eff_long == OI_eff_short`
     - if `OI_eff_short <= phantom_dust_bound_short_q`:
       - set `OI_eff_long = 0`
       - set `OI_eff_short = 0`
       - set both pending-reset flags true
     - else fail conservatively

4. **DrainOnly zero-OI scheduling**
   - if `mode_long == DrainOnly` and `OI_eff_long == 0`, set `pending_reset_long = true`
   - if `mode_short == DrainOnly` and `OI_eff_short == 0`, set `pending_reset_short = true`

### 5.8 `finalize_end_of_instruction_resets(ctx)`

This helper MUST:

1. if `pending_reset_long` and `mode_long != ResetPending`, invoke `begin_full_drain_reset(long)`
2. if `pending_reset_short` and `mode_short != ResetPending`, invoke `begin_full_drain_reset(short)`
3. if `mode_long == ResetPending` and `OI_eff_long == 0` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, invoke `finalize_side_reset(long)`
4. if `mode_short == ResetPending` and `OI_eff_short == 0` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, invoke `finalize_side_reset(short)`

---

## 6. Loss settlement, live finalization, and resolved-close helpers

### 6.1 `settle_losses_from_principal(i)`

If `PNL_i < 0`, the engine MUST attempt to settle from principal immediately:

1. `need = (-PNL_i) as u128`
2. `pay = min(need, C_i)`
3. apply:
   - `set_capital(i, C_i - pay)`
   - `set_pnl(i, PNL_i + pay, NoPositiveIncreaseAllowed)`

### 6.2 Open-position negative remainder

If after §6.1:

- `PNL_i < 0`, and
- `effective_pos_q(i) != 0`

then the account MUST remain liquidatable.

### 6.3 Flat-account negative remainder

If after §6.1:

- `PNL_i < 0`, and
- `effective_pos_q(i) == 0`

then the engine MUST:

1. `absorb_protocol_loss((-PNL_i) as u128)`
2. `set_pnl(i, 0, NoPositiveIncreaseAllowed)`

This path is allowed only for already-authoritative flat accounts.

### 6.4 `fee_debt_sweep(i)`

After any operation that increases `C_i`, or after a full current-state authoritative touch where capital is no longer senior-encumbered by attached trading losses, the engine MUST pay down fee debt:

1. `debt = fee_debt_u128_checked(fee_credits_i)`
2. `pay = min(debt, C_i)`
3. if `pay > 0`:
   - `set_capital(i, C_i - pay)`
   - add `pay` to `fee_credits_i`
   - `I = I + pay`

This sweep leaves `Eq_maint_raw_i`, `Eq_trade_raw_i`, and `Eq_withdraw_raw_i` unchanged because capital and fee debt move one for one.

### 6.5 `touch_account_live_local(i, ctx)`

This is the canonical live local touch.

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. add `i` to `ctx.touched_accounts[]` if not already present
4. `advance_profit_warmup(i)`
5. `settle_side_effects_live(i, ctx.H_lock_shared)`
6. `settle_losses_from_principal(i)`
7. if `effective_pos_q(i) == 0` and `PNL_i < 0`, resolve uncovered flat loss
8. MUST NOT auto-convert
9. MUST NOT fee-sweep

### 6.6 `finalize_touched_accounts_post_live(ctx)`

This helper is mandatory for every live instruction that uses `touch_account_live_local`.

Procedure:

1. compute one shared post-live conversion snapshot:
   - `Residual_snapshot = max(0, V - (C_tot + I))`
   - `PNL_matured_pos_tot_snapshot = PNL_matured_pos_tot`
   - if `PNL_matured_pos_tot_snapshot > 0`:
     - `h_snapshot_num = min(Residual_snapshot, PNL_matured_pos_tot_snapshot)`
     - `h_snapshot_den = PNL_matured_pos_tot_snapshot`
     - `whole_snapshot = (h_snapshot_num == h_snapshot_den)`
   - else:
     - define `whole_snapshot = false`
2. iterate `ctx.touched_accounts[]` in deterministic ascending storage-index order:
   - if `basis_pos_q_i == 0`, `ReleasedPos_i > 0`, and `whole_snapshot == true`:
     - `released = ReleasedPos_i`
     - `consume_released_pnl(i, released)`
     - `set_capital(i, C_i + released)`
   - fee-sweep the account

### 6.7 Resolved positive-payout readiness

Positive resolved payouts MUST NOT begin until the market is terminal-ready for positive claims.

A market is **positive-payout ready** only when all of the following hold:

- `stale_account_count_long == 0`
- `stale_account_count_short == 0`
- `stored_pos_count_long == 0`
- `stored_pos_count_short == 0`
- `neg_pnl_account_count == 0`

`neg_pnl_account_count` is therefore the exact O(1) readiness aggregate for remaining negative claims. Because every persistent mutation of `PNL_i` must flow through `set_pnl`, implementations MUST maintain this aggregate exactly through that helper rather than by ad hoc snapshot-time iteration.

### 6.8 `capture_resolved_payout_snapshot_if_needed()`

This helper MAY succeed only if:

- `market_mode == Resolved`
- `resolved_payout_snapshot_ready == false`
- the market is positive-payout ready per §6.7

On success:

1. `Residual_snapshot = max(0, V - (C_tot + I))`
2. if `PNL_matured_pos_tot == 0`:
   - `resolved_payout_h_num = 0`
   - `resolved_payout_h_den = 0`
3. else:
   - `resolved_payout_h_num = min(Residual_snapshot, PNL_matured_pos_tot)`
   - `resolved_payout_h_den = PNL_matured_pos_tot`
4. set `resolved_payout_snapshot_ready = true`

### 6.9 `force_close_resolved_terminal_nonpositive(i) -> payout`

This helper terminally closes a resolved account whose local claim is already non-positive and returns its terminal payout.

Preconditions:

- `market_mode == Resolved`
- account `i` is materialized
- `basis_pos_q_i == 0`
- `PNL_i <= 0`

Procedure:

1. if `PNL_i < 0`, resolve uncovered flat loss via §6.3
2. fee-sweep the account
3. forgive any remaining negative `fee_credits_i`
4. let `payout = C_i`
5. if `payout > 0`:
   - `set_capital(i, 0)`
   - `V = V - payout`
6. require `PNL_i == 0`, `R_i == 0`, both reserve buckets absent, and `basis_pos_q_i == 0`
7. reset local fields and free the slot

### 6.10 `force_close_resolved_terminal_positive(i) -> payout`

This helper terminally closes a resolved account with a positive claim and returns its terminal payout.

Preconditions:

- `market_mode == Resolved`
- account `i` is materialized
- `basis_pos_q_i == 0`
- `PNL_i > 0`
- `resolved_payout_snapshot_ready == true`
- `resolved_payout_h_den > 0`

Procedure:

1. let `x = max(PNL_i, 0)`
2. let `y = floor(x * resolved_payout_h_num / resolved_payout_h_den)`
3. `set_pnl(i, 0, NoPositiveIncreaseAllowed)`
4. `set_capital(i, C_i + y)`
5. fee-sweep the account
6. forgive any remaining negative `fee_credits_i`
7. let `payout = C_i`
8. if `payout > 0`:
   - `set_capital(i, 0)`
   - `V = V - payout`
9. require `PNL_i == 0`, `R_i == 0`, both reserve buckets absent, and `basis_pos_q_i == 0`
10. reset local fields and free the slot

Impossible states — for example `resolved_payout_snapshot_ready == true` with `PNL_i > 0` but `resolved_payout_h_den == 0` — MUST fail conservatively rather than falling back to `y = x`.

---

## 7. Fees

This revision has no engine-native recurring maintenance fee. The engine core defines native trading fees, native liquidation fees, and the canonical helper for optional wrapper-owned account fees.

### 7.1 Trading fees

Define:

- `fee = mul_div_ceil_u128(trade_notional, trading_fee_bps, 10_000)`

Rules:

- if `trading_fee_bps == 0` or `trade_notional == 0`, then `fee = 0`
- if `trading_fee_bps > 0` and `trade_notional > 0`, then `fee >= 1`

The fee MUST be charged using `charge_fee_to_insurance(i, fee)`.

### 7.2 Liquidation fees

For a liquidation that closes `q_close_q` at `oracle_price`:

- if `q_close_q == 0`, `liq_fee = 0`
- else:
  - `closed_notional = mul_div_floor_u128(q_close_q, oracle_price, POS_SCALE)`
  - `liq_fee_raw = mul_div_ceil_u128(closed_notional, liquidation_fee_bps, 10_000)`
  - `liq_fee = min(max(liq_fee_raw, min_liquidation_abs), liquidation_fee_cap)`

### 7.3 Optional wrapper-owned account fees

A wrapper MAY impose additional account fees by routing an amount `fee_abs` through `charge_fee_to_insurance(i, fee_abs)`, provided `fee_abs <= MAX_PROTOCOL_FEE_ABS`.

### 7.4 Fee debt as margin liability

`FeeDebt_i`:

- MUST reduce `Eq_maint_raw_i`, `Eq_trade_raw_i`, `Eq_trade_open_raw_i`, and `Eq_withdraw_raw_i`
- MUST be swept whenever capital becomes available and is no longer senior-encumbered by already-realized trading losses on the same local state
- MUST NOT directly change `Residual`, `PNL_pos_tot`, or `PNL_matured_pos_tot`
- includes unpaid native trading fees, native liquidation fees, and any wrapper-owned account fees routed through the canonical helper
- any explicit fee amount beyond collectible capacity is dropped rather than written into `PNL_i` or `D`

---

## 8. Margin checks and liquidation

### 8.1 Margin requirements

After live touch reconciliation, define:

- `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`

If `effective_pos_q(i) == 0`:

- `MM_req_i = 0`
- `IM_req_i = 0`

Else:

- `MM_req_i = max(mul_div_floor_u128(Notional_i, maintenance_bps, 10_000), MIN_NONZERO_MM_REQ)`
- `IM_req_i = max(mul_div_floor_u128(Notional_i, initial_bps, 10_000), MIN_NONZERO_IM_REQ)`

Healthy conditions:

- maintenance healthy if exact `Eq_net_i > MM_req_i`
- withdrawal healthy if exact `Eq_withdraw_raw_i >= IM_req_i`
- risk-increasing trade approval healthy if exact `Eq_trade_open_raw_i >= IM_req_post_i`

### 8.2 Risk-increasing and strictly risk-reducing trades

A trade for account `i` is risk-increasing when either:

1. `abs(new_eff_pos_q_i) > abs(old_eff_pos_q_i)`, or
2. the position sign flips across zero, or
3. `old_eff_pos_q_i == 0` and `new_eff_pos_q_i != 0`

A trade is strictly risk-reducing when:

- `old_eff_pos_q_i != 0`
- `new_eff_pos_q_i != 0`
- `sign(new_eff_pos_q_i) == sign(old_eff_pos_q_i)`
- `abs(new_eff_pos_q_i) < abs(old_eff_pos_q_i)`

### 8.3 Liquidation eligibility

An account is liquidatable when after a full current-state authoritative live touch:

- `effective_pos_q(i) != 0`, and
- `Eq_net_i <= MM_req_i`

### 8.4 Partial liquidation

A liquidation MAY be partial only if:

- `0 < q_close_q < abs(old_eff_pos_q_i)`

A successful partial liquidation MUST:

1. use the current touched state
2. compute the nonzero remaining effective position
3. close `q_close_q` synthetically at `oracle_price`; this adds **no** additional execution-slippage PnL because the synthetic execution price equals the oracle price
4. apply the remaining position with `attach_effective_position`
5. settle realized losses from principal
6. charge the liquidation fee on the closed quantity
7. invoke `enqueue_adl(ctx, liq_side, q_close_q, 0)`
8. even if a pending reset is scheduled, still require the remaining nonzero position to be maintenance healthy on the current post-step state before returning

### 8.5 Full-close or bankruptcy liquidation

A deterministic full-close liquidation MUST:

1. use the current touched state
2. close the full remaining effective position synthetically at `oracle_price`; this adds **no** additional execution-slippage PnL because the synthetic execution price equals the oracle price
3. zero the basis with `attach_effective_position(i, 0)`
4. settle realized losses from principal
5. charge liquidation fee
6. define bankruptcy deficit `D = max(-PNL_i, 0)`
7. invoke `enqueue_adl(ctx, liq_side, q_close_q, D)` if `q_close_q > 0` or `D > 0`
8. if `D > 0`, set `PNL_i = 0` with `NoPositiveIncreaseAllowed`

### 8.6 Side-mode gating

Before any top-level instruction rejects an OI-increasing operation because a side is in `ResetPending`, it MUST first invoke `maybe_finalize_ready_reset_sides_before_oi_increase()`.

Any operation that would increase net side open interest on a side whose mode is `DrainOnly` or `ResetPending` MUST be rejected.

For `execute_trade`, this prospective check MUST use the exact bilateral candidate after-values of §5.2.2 on both sides.

---

## 9. External operations

### 9.0 Standard live instruction lifecycle

`H_lock` and `funding_rate_e9_per_slot` are wrapper-owned logical inputs, not public caller-owned fields. Public or permissionless wrappers MUST derive them internally.

Unless explicitly noted otherwise, a live external state-mutating operation that depends on current market state executes in this order:

1. validate monotonic slot, oracle input, funding-rate bound, and `H_lock` bound
2. initialize fresh `ctx` with `H_lock_shared = H_lock`
3. call `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` exactly once
4. set `current_slot = now_slot`
5. perform the endpoint’s exact current-state inner execution
6. call `finalize_touched_accounts_post_live(ctx)` exactly once
7. call `schedule_end_of_instruction_resets(ctx)` exactly once
8. call `finalize_end_of_instruction_resets(ctx)` exactly once
9. assert `OI_eff_long == OI_eff_short` at the end of every live top-level instruction that can mutate side state or live exposure

### 9.1 `settle_account(i, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock)`

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market once
5. set `current_slot`
6. `touch_account_live_local(i, ctx)`
7. `finalize_touched_accounts_post_live(ctx)`
8. schedule resets
9. finalize resets
10. assert `OI_eff_long == OI_eff_short`

### 9.2 `deposit(i, amount, now_slot)`

`deposit` is pure capital transfer. It MUST NOT call `accrue_market_to`, MUST NOT mutate side state, and MUST NOT mutate reserve state.

Procedure:

1. require `market_mode == Live`
2. require `now_slot >= current_slot`
3. if account `i` is missing:
   - require `amount >= MIN_INITIAL_DEPOSIT`
   - materialize the account
4. set `current_slot = now_slot`
5. require `V + amount <= MAX_VAULT_TVL`
6. set `V = V + amount`
7. `set_capital(i, C_i + amount)`
8. `settle_losses_from_principal(i)`
9. MUST NOT invoke flat-loss insurance absorption
10. if `basis_pos_q_i == 0` and `PNL_i >= 0`, fee-sweep

### 9.2.1 `deposit_fee_credits(i, amount, now_slot)`

This is direct external repayment of fee debt.

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. `pay = min(amount, FeeDebt_i)`
6. if `pay == 0`, return
7. require `V + pay <= MAX_VAULT_TVL`
8. set `V = V + pay`
9. set `I = I + pay`
10. add `pay` to `fee_credits_i`
11. require `fee_credits_i <= 0`

### 9.2.2 `top_up_insurance_fund(amount, now_slot)`

1. require `market_mode == Live`
2. require `now_slot >= current_slot`
3. set `current_slot = now_slot`
4. require `V + amount <= MAX_VAULT_TVL`
5. set `V = V + amount`
6. set `I = I + amount`

### 9.2.3 `charge_account_fee(i, fee_abs, now_slot)`

Optional wrapper-facing pure fee instruction.

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
4. require `fee_abs <= MAX_PROTOCOL_FEE_ABS`
5. set `current_slot = now_slot`
6. `charge_fee_to_insurance(i, fee_abs)`

### 9.2.4 `settle_flat_negative_pnl(i, now_slot)`

Permissionless live-only cleanup path for an already-flat authoritative account carrying negative `PNL_i`.

This instruction is **not** a pure capital-flow instruction. It is an authoritative PnL-cleanup path for an already-flat account and MAY therefore absorb realized losses without calling `accrue_market_to`.

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. require `basis_pos_q_i == 0`
6. require `R_i == 0` and both reserve buckets absent
7. if `PNL_i >= 0`, return
8. settle losses from principal
9. if `PNL_i < 0`, absorb protocol loss and set `PNL_i = 0`
10. require `PNL_i == 0`

### 9.3 `withdraw(i, amount, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock)`

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market
5. set `current_slot`
6. `touch_account_live_local(i, ctx)`
7. `finalize_touched_accounts_post_live(ctx)`
8. require `amount <= C_i`
9. require post-withdraw capital is either `0` or `>= MIN_INITIAL_DEPOSIT`
10. if `effective_pos_q(i) != 0`, require withdrawal health on the hypothetical post-withdraw state where both `V` and `C_tot` decrease by `amount`
11. apply `set_capital(i, C_i - amount)` and `V = V - amount`
12. schedule resets
13. finalize resets
14. assert `OI_eff_long == OI_eff_short`

### 9.3.1 `convert_released_pnl(i, x_req, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock)`

Explicit voluntary conversion of matured released positive PnL for any live account.

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market
5. set `current_slot`
6. `touch_account_live_local(i, ctx)`
7. require `0 < x_req <= ReleasedPos_i`
8. compute current `h`
9. if `basis_pos_q_i == 0`, require `x_req <= max_safe_flat_conversion_released(i, x_req, h_num, h_den)`
10. `consume_released_pnl(i, x_req)`
11. `set_capital(i, C_i + floor(x_req * h_num / h_den))`
12. fee-sweep
13. if `effective_pos_q(i) != 0`, require the post-conversion state is maintenance healthy
14. `finalize_touched_accounts_post_live(ctx)`
15. schedule resets
16. finalize resets
17. assert `OI_eff_long == OI_eff_short`

### 9.4 `execute_trade(a, b, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock, size_q, exec_price)`

`size_q > 0` means account `a` buys base from account `b`.

Procedure:

1. require `market_mode == Live`
2. require both accounts are materialized
3. require `a != b`
4. validate slot and prices
5. require `0 < size_q <= MAX_TRADE_SIZE_Q`
6. require `trade_notional <= MAX_ACCOUNT_NOTIONAL`
7. initialize `ctx`
8. accrue market
9. set `current_slot`
10. touch both accounts locally
11. capture pre-trade effective positions, maintenance requirements, and exact widened raw maintenance buffers
12. finalize any already-ready reset sides before OI increase
13. compute candidate post-trade effective positions
14. require position bounds
15. compute exact bilateral candidate OI after-values
16. enforce `MAX_OI_SIDE_Q`
17. reject any trade that would increase OI on a blocked side
18. compute `trade_pnl_a` and `trade_pnl_b` via `compute_trade_pnl(size_q, oracle_price, exec_price)` and apply execution-slippage PnL before fees:
   - `set_pnl(a, PNL_a + trade_pnl_a, UseHLock(H_lock))`
   - `set_pnl(b, PNL_b + trade_pnl_b, UseHLock(H_lock))`
19. attach the resulting effective positions
20. write the exact candidate OI after-values
21. settle post-trade losses from principal for both accounts
22. if a resulting effective position is zero, require `PNL_i >= 0` before fees
23. compute and charge explicit trading fees, capturing `fee_equity_impact_a` and `fee_equity_impact_b`
24. compute post-trade `Notional_post_i`, `IM_req_post_i`, `MM_req_post_i`, and `Eq_trade_open_raw_i`
25. enforce post-trade approval independently for both accounts:
   - if resulting effective position is zero, require exact `Eq_maint_raw_i >= 0`
   - else if risk-increasing, require exact `Eq_trade_open_raw_i >= IM_req_post_i`
   - else if exact maintenance health already holds, allow
   - else if strictly risk-reducing, allow only if both:
     - `((Eq_maint_raw_post_i + fee_equity_impact_i) - MM_req_post_i) > (Eq_maint_raw_pre_i - MM_req_pre_i)`
     - `min(Eq_maint_raw_post_i + fee_equity_impact_i, 0) >= min(Eq_maint_raw_pre_i, 0)`
   - else reject
26. `finalize_touched_accounts_post_live(ctx)`
27. schedule resets
28. finalize resets
29. assert `OI_eff_long == OI_eff_short`

### 9.5 `liquidate(i, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock, policy)`

`policy ∈ {FullClose, ExactPartial(q_close_q)}`.

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market
5. set `current_slot`
6. touch the account locally
7. require liquidation eligibility
8. execute either exact partial liquidation or full-close liquidation on the already-touched state
9. `finalize_touched_accounts_post_live(ctx)`
10. schedule resets
11. finalize resets
12. assert `OI_eff_long == OI_eff_short`

### 9.6 `keeper_crank(now_slot, oracle_price, funding_rate_e9_per_slot, H_lock, ordered_candidates[], max_revalidations)`

`ordered_candidates[]` is keeper-supplied and untrusted. It MAY be empty; an empty call is a valid “accrue-only plus finalize” instruction.

1. require `market_mode == Live`
2. initialize `ctx`
3. validate slot and oracle
4. accrue market exactly once
5. set `current_slot = now_slot`
6. iterate candidates in keeper-supplied order until budget exhausted or a pending reset is scheduled:
   - missing-account skips do not count
   - touching a materialized account counts against `max_revalidations`
   - `touch_account_live_local(candidate, ctx)`
   - if the account is liquidatable after touch and a current-state-valid liquidation-policy hint is present, execute liquidation on the already-touched state
7. `finalize_touched_accounts_post_live(ctx)`
8. schedule resets
9. finalize resets
10. assert `OI_eff_long == OI_eff_short`

### 9.7 `resolve_market(resolved_price, now_slot)`

Privileged deployment-owned transition.

Preconditions:

- `market_mode == Live`
- `now_slot == current_slot`
- `now_slot == slot_last`

This means the wrapper has already synchronized live accrual to the resolution slot. The zero-funding settlement transition therefore has `dt = 0` and cannot erase elapsed live funding.

Procedure:

1. require validated `0 < resolved_price <= MAX_ORACLE_PRICE`
2. require exact settlement-band check against `P_last`
3. call `accrue_market_to(now_slot, resolved_price, 0)`
4. set `current_slot = now_slot`
5. set `market_mode = Resolved`
6. set `resolved_price = resolved_price`
7. set `resolved_slot = now_slot`
8. clear resolved payout snapshot state
9. set `PNL_matured_pos_tot = PNL_pos_tot`
10. set `OI_eff_long = 0` and `OI_eff_short = 0`
11. for each side:
    - if `mode_side != ResetPending`, invoke `begin_full_drain_reset(side)`
    - if the resulting side state is `ResetPending` and `stale_account_count_side == 0` and `stored_pos_count_side == 0`, invoke `finalize_side_reset(side)`
12. require both open-interest sides are zero

### 9.8 `force_close_resolved(i, now_slot)`

Multi-stage resolved-market progress path.

An implementation MUST expose an explicit outcome distinguishing:
- `ProgressOnly` — local reconciliation progressed but no terminal close occurred yet,
- `Closed { payout }` — the account was terminally closed and paid out `payout`.

A zero payout MUST NOT be the sole encoding of “not yet closeable.”

1. require `market_mode == Resolved`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. `prepare_account_for_resolved_touch(i)`
6. `settle_side_effects_resolved(i)`
7. settle losses from principal if needed
8. resolve uncovered flat loss if needed
9. if `mode_long == ResetPending` and `OI_eff_long == 0` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, finalize the long side
10. if `mode_short == ResetPending` and `OI_eff_short == 0` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, finalize the short side
11. if `PNL_i <= 0`, return `Closed { payout }` from `force_close_resolved_terminal_nonpositive(i)`
12. if `PNL_i > 0`:
    - if the market is not positive-payout ready, return `ProgressOnly` after persisting the local reconciliation
    - if the shared resolved payout snapshot is not ready, capture it
    - return `Closed { payout }` from `force_close_resolved_terminal_positive(i)`

### 9.9 `reclaim_empty_account(i, now_slot)`

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
4. require the flat-clean reclaim preconditions of §2.8
5. set `current_slot = now_slot`
6. require final reclaim eligibility of §2.8
7. execute the reclamation effects of §2.8

---

## 10. Permissionless off-chain shortlist keeper mode

1. The engine does **not** require any on-chain phase-1 search, barrier classifier, or no-false-negative scan proof.
2. `ordered_candidates[]` is keeper-supplied and untrusted. It MAY be stale, incomplete, duplicated, adversarially ordered, or produced by approximate heuristics.
3. Optional liquidation-policy hints are untrusted. They MUST be ignored unless they encode one of the supported policies and pass the same exact current-state validity checks as the normal `liquidate` entrypoint.
4. The protocol MUST NOT require that a keeper discover all currently liquidatable accounts before it may process a useful subset.
5. Because `settle_account`, `liquidate`, `reclaim_empty_account`, and `force_close_resolved` are permissionless, reset progress and dead-account recycling MUST remain possible without any mandatory on-chain scan order.
6. `max_revalidations` counts normal exact current-state revalidation attempts on materialized accounts. A missing-account skip does not count.
7. Inside `keeper_crank`, the per-candidate local exact-touch helper MUST be economically equivalent to `touch_account_live_local(i, ctx)` on the already-accrued instruction state.
8. The only mandatory on-chain ordering constraints are:
   - a single initial accrual,
   - candidate processing in keeper-supplied order,
   - stop further candidate processing once a pending reset is scheduled.

---

## 11. Required test properties

An implementation MUST include tests covering at least the following.

1. `V >= C_tot + I` always.
2. Positive `set_pnl` increases raise `R_i` by the same delta and do not immediately increase `PNL_matured_pos_tot`.
3. Fresh unwarmed manipulated PnL cannot satisfy withdrawal checks or principal conversion.
4. Aggregate positive PnL admitted through `g` is bounded by `Residual`.
5. `Eq_trade_open_raw_i` exactly neutralizes the candidate trade’s own positive slippage.
6. A trade that only passes because of its own positive slippage is rejected.
7. Fee-debt sweep leaves `Eq_maint_raw_i` unchanged.
8. Pure warmup release does not reduce `Eq_maint_raw_i`.
9. Pure warmup release does not increase `Eq_trade_raw_i`.
10. Pure warmup release can increase `Eq_withdraw_raw_i`.
11. Fresh reserve never inherits elapsed time from an older scheduled bucket.
12. Adding new reserve does not reset or alter the older scheduled bucket’s `sched_start_slot`, `sched_horizon`, `sched_anchor_q`, or already accrued progress.
13. The pending bucket never matures while pending.
14. When promoted, the pending bucket starts fresh at `current_slot` with zero scheduled release.
15. Reserve-loss ordering is newest-first: pending bucket before scheduled bucket.
16. Repeated small reserve additions can only affect the newest pending bucket; they cannot relock the older scheduled bucket.
17. Whole-only automatic flat conversion works only at `h = 1`.
18. No permissionless lossy flat conversion occurs under `h < 1`.
19. `convert_released_pnl` consumes only `ReleasedPos_i` and leaves reserve state unchanged.
20. Flat explicit conversion rejects if the requested amount exceeds `max_safe_flat_conversion_released`.
21. Same-epoch local settlement is prefix-independent.
22. Repeated same-epoch touches without explicit position mutation do not compound quantity-flooring loss.
23. Phantom-dust bounds conservatively cover same-epoch zeroing, basis replacements, and ADL multiplier truncation.
24. Dust-clear scheduling and reset initiation happen only at end of top-level instructions.
25. Epoch gaps larger than one are rejected as corruption.
26. If `A_candidate == 0` with `OI_post > 0`, the engine force-drains both sides instead of reverting.
27. If ADL `delta_K_abs` is non-representable or `K_opp + delta_K_exact` overflows, quantity socialization still proceeds and the remainder routes through `record_uninsured_protocol_loss`.
28. `enqueue_adl` spends insurance down to `I_floor` before any remaining bankruptcy loss is socialized or left as junior undercollateralization.
29. The exact ADL dust-bound increment matches §5.6 step 10 and the unilateral and bilateral dust-clear conditions match §5.7 exactly.
30. Each funding sub-step applies the same exact `fund_num_step` to both sides’ `F_side_num` updates with opposite signs.
31. A flat account with negative `PNL_i` resolves through `absorb_protocol_loss` only in the allowed already-authoritative flat-account paths.
32. Reset finalization reopens a side once `ResetPending` preconditions are fully satisfied.
33. `deposit` settles realized losses before fee sweep.
34. A missing account cannot be materialized by a deposit smaller than `MIN_INITIAL_DEPOSIT`.
35. The strict risk-reducing trade exemption uses exact widened raw maintenance buffers and exact widened raw maintenance shortfall.
36. The strict risk-reducing trade exemption adds back `fee_equity_impact_i`, not nominal fee.
37. Any side-count increment — including a sign flip — enforces `MAX_ACTIVE_POSITIONS_PER_SIDE`.
38. A flat trade cannot bypass ADL by leaving negative `PNL_i` behind.
39. Live flat dust accounts can be reclaimed safely.
40. Missing-account safety: ordinary live and resolved paths do not auto-materialize missing accounts.
41. `keeper_crank` accrues the market exactly once per instruction.
42. The per-candidate keeper touch is economically equivalent to `touch_account_live_local`.
43. `max_revalidations` counts only normal exact revalidation attempts on materialized accounts.
44. `deposit_fee_credits` applies only `min(amount, FeeDebt_i)` and never makes `fee_credits_i` positive.
45. `charge_account_fee` mutates only capital, fee debt, and insurance through canonical helpers.
46. Trade-opening health and withdrawal health are distinct lanes.
47. Once resolved, all remaining positive PnL is globally treated as matured.
48. `prepare_account_for_resolved_touch(i)` clears local reserve state without a second global aggregate change.
49. No positive resolved payout occurs until stale-account reconciliation is complete across both sides and the shared payout snapshot is locked.
50. A resolved account with `PNL_i <= 0` can close immediately after local reconciliation, even while unrelated positive claims are still waiting for the shared snapshot.
51. Every positive terminal resolved close uses the same captured resolved payout snapshot.
52. Live instructions reject invalid `H_lock` and invalid `funding_rate_e9_per_slot`.
53. `deposit`, `deposit_fee_credits`, `top_up_insurance_fund`, and `charge_account_fee` do not draw insurance.
54. `settle_flat_negative_pnl` is a live-only permissionless cleanup path that does not mutate side state.
55. `resolve_market` fails unless `slot_last == current_slot == now_slot`, so the zero-funding settlement transition cannot erase elapsed live funding.
56. `resolve_market` rejects settlement prices outside the immutable band around `P_last`.
57. Under open-interest symmetry, end-of-instruction reset scheduling preserves `OI_eff_long == OI_eff_short`.
58. The simplified two-bucket warmup design never accelerates release relative to the sampled bucket horizons.
59. Positive resolved payouts do not begin until the market is positive-payout ready per §6.7, or an exact equivalent readiness predicate is true.
60. `neg_pnl_account_count` exactly matches iteration over materialized accounts with `PNL_i < 0` after every path that mutates `PNL_i`.
61. The touched-account set cannot silently drop an account; if capacity would be exceeded, the instruction fails conservatively.
62. Whole-only automatic flat conversion in §6.6 uses the exact helper sequence `consume_released_pnl` then `set_capital`.
63. `force_close_resolved` exposes an explicit progress-versus-close outcome; a zero payout is never the sole encoding of “not yet closeable.”
60. The positive resolved-close path fails conservatively, not permissively, if a snapshot is marked ready with a zero payout denominator while some account still has `PNL_i > 0`.

---

## 12. Wrapper obligations (deployment layer, not engine-checked)

The following are deployment-wrapper obligations.

1. **Do not expose caller-controlled live policy inputs.**  
   `H_lock` and `funding_rate_e9_per_slot` are wrapper-owned internal inputs. Public or permissionless wrappers MUST derive them internally and MUST NOT accept arbitrary caller-chosen values.
2. **Authority-gate market resolution.**  
   `resolve_market` is a privileged deployment-owned transition. A compliant wrapper MUST source `resolved_price` from the deployment’s trusted settlement source or policy.
3. **Synchronize live accrual before resolution.**  
   Because `resolve_market` requires `slot_last == current_slot == now_slot`, a compliant wrapper MUST first synchronize live accrual to the intended resolution slot. An empty `keeper_crank` or any equivalent accrue-capable live instruction is sufficient.
4. **Public wrappers SHOULD enforce execution-price admissibility.**  
   A sufficient rule is `abs(exec_price - oracle_price) * 10_000 <= max_trade_price_deviation_bps * oracle_price`, with `max_trade_price_deviation_bps <= 2 * trading_fee_bps`.
5. **Use oracle notional for wrapper-side exposure ranking.**
6. **Keep user-owned value-moving operations account-authorized.**  
   User-owned value-moving paths include `deposit`, `withdraw`, `execute_trade`, and `convert_released_pnl`. Intended permissionless progress paths are `settle_account`, `liquidate`, `reclaim_empty_account`, `settle_flat_negative_pnl`, `force_close_resolved`, and `keeper_crank`.
7. **Do not expose pure wrapper-owned account fees carelessly.**  
   `charge_account_fee` performs no maintenance gating of its own. A compliant wrapper SHOULD either restrict it to already-safe contexts or pair it with a live-touch health-check flow when used on accounts that may still carry live risk.
8. **Provide a post-snapshot resolved-close progress path.**  
   Because `force_close_resolved` is intentionally multi-stage, a compliant deployment SHOULD provide either a self-service retry path or a permissionless batch or incentive path that sweeps positive resolved accounts after the shared payout snapshot is ready.
9. **Refresh the live mark before resolution when policy expects it.**  
   Because the immutable settlement band is anchored to `P_last`, a deployment that expects resolution to reference the freshest live mark SHOULD refresh live market state immediately before invoking `resolve_market`.

