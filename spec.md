# Risk Engine Spec (Source of Truth) — v12.15.0

**Combined Single-Document Native 128-bit Revision  
(Wrapper-Driven Warmup Horizon / Wrapper-Owned Account-Fee Policy / Wrapper-Supplied High-Precision Funding Side-Index Input / Exact Reserve-Cohort Warmup With Bounded Exact Queue + Fixed-Horizon Overflow / Exact Candidate-Trade Neutralization / Price-Bounded Resolved-Market Settlement / Whole-Only Automatic Flat Conversion / Full-Local-PnL Maintenance / Immutable Configuration / Unencumbered-Flat Deposit Sweep / Mandatory Post-Partial Local Health Check Edition)**

**Design:** Protected Principal + Junior Profit Claims + Lazy A/K Side Indices (Native 128-bit Base-10 Scaling)  
**Status:** implementation source-of-truth (normative language: MUST / MUST NOT / SHOULD / MAY)  
**Scope:** perpetual DEX risk engine for a single quote-token vault  
**Goal:** preserve conservation, bounded insolvency handling, oracle-manipulation resistance, deterministic exact warmup-queue behavior, and liveness while supporting lazy ADL across the opposing open-interest side without global scans, canonical-order dependencies, or sequential prefix requirements for user settlement.

This revision supersedes v12.14.0 and keeps the wrapper-driven core split while fixing the remaining non-minor issues found in adversarial review.

The engine core keeps only:

- the exact reserve-cohort warmup queue and `PNL_matured_pos_tot`,
- the global trade haircut `g`,
- the matured-profit haircut `h`,
- the exact trade-open counterfactual approval metric `Eq_trade_open_raw_i`,
- capital / fee-debt / insurance accounting,
- lazy A/K settlement,
- liquidation and reset mechanics,
- resolved-market settlement and terminal close.

The following policy inputs become wrapper-owned and are **not** computed by the engine core:

- the warmup horizon chosen for a live accrued instruction that may create new reserve,
- any optional wrapper-owned per-account fee policy beyond engine-native trading and liquidation fees,
- the funding rate applied to the elapsed live interval,
- any public execution-price admissibility policy,
- any mark-EWMA or premium-funding model.

The engine validates bounds on those wrapper inputs where applicable, but it does not derive them.

## Change summary from v12.14.0

1. **Overflow segments now use one fixed conservative horizon.**  
   Once exact reserve capacity is exhausted, any newly created `overflow_older_i` or `overflow_newest_i` segment uses the immutable overflow horizon `H_overflow = H_max`. Wrapper-supplied `H_lock` applies only to exact cohorts. This closes both the pending pre-seeding bypass and the post-saturation horizon-extension grief surface.

2. **Funding is now carried in an exact high-precision side index rather than rounded per call.**  
   The engine adds cumulative side funding numerators `F_long_num` and `F_short_num` plus per-account snapshots `f_snap_i`. Wrapper-supplied `funding_rate_e9_per_slot` is applied into those numerator indices without per-call floor division, eliminating the positive-zero / negative-minus-one quantization asymmetry while keeping new entrants from inheriting prior fractional funding.

3. **The v12.14 fixes are retained unchanged.**  
   The exact-queue plus overflow design, unconditional ADL dust-bound accrual on every `A_side` decay, active-position side-cap enforcement, the capped flat-conversion helper, whole-only automatic flat conversion, explicit flat-account released-PnL conversion, aggregate-consistent withdrawal simulation, and price-bounded resolved settlement remain part of this revision.
---

## 0. Security goals (normative)

The engine MUST provide the following properties.

1. **Protected principal for flat accounts:** an account with effective position `0` MUST NOT have its protected principal directly reduced by another account's insolvency.
2. **Explicit open-position ADL eligibility:** accounts with open positions MAY be subject to deterministic protocol ADL if they are on the eligible opposing side of a bankrupt liquidation. ADL MUST operate through explicit protocol state, not hidden execution.
3. **Oracle-manipulation safety for extraction:** profits created by short-lived oracle distortion MUST NOT immediately dilute the matured-profit haircut denominator `h`, immediately become withdrawable principal, or immediately satisfy withdrawal / principal-conversion approval checks.
4. **Bounded trade reuse of positive PnL:** fresh positive PnL MAY support the generating account's own risk-increasing trades only through the global trade haircut `g`. Aggregate positive PnL admitted through `g` MUST NOT exceed current `Residual`.
5. **No same-trade bootstrap from positive slippage:** a candidate trade's own positive execution-slippage PnL MUST NOT be allowed to make that same trade pass a risk-increasing initial-margin check.
6. **Bounded wrapper-chosen horizon for exact cohorts:** when a live instruction creates new reserve and exact cohort capacity is available, the engine MUST apply the wrapper-supplied instruction-shared `H_lock` exactly to the newly created exact cohort. The engine MUST reject out-of-range `H_lock`.
7. **Fixed conservative horizon for overflow:** when exact cohort capacity is exhausted, any newly created or activated overflow segment MUST use the immutable overflow horizon `H_overflow = H_max`. Wrapper-supplied `H_lock` MUST NOT shorten, extend, or otherwise mutate any overflow segment horizon.
8. **No practical warmup dust-griefing:** an attacker MUST NOT be able to destroy materially accrued maturity progress of a victim's older exact reserve or older preserved overflow reserve through dust-sized or otherwise tiny positive-PnL additions. If exact cohort capacity is exhausted, any conservative bounded-storage approximation MUST be confined only to the newest pending overflow segment while the currently scheduled overflow segment keeps its prior law.
9. **Profit-first haircuts:** when the system is undercollateralized, haircuts MUST apply to junior profit claims before any protected principal of flat accounts is impacted.
10. **Conservation:** the engine MUST NOT create withdrawable claims exceeding vault tokens, except for explicitly bounded rounding slack.
11. **Liveness:** the engine MUST NOT require `OI == 0`, manual admin recovery, a global scan, or reconciliation of an unrelated prefix of accounts before a user can safely settle, deposit, withdraw, trade, liquidate, repay fee debt, reclaim, or resolved-close. Market resolution itself may be privileged by deployment policy.
12. **No zombie poisoning of the withdrawal haircut:** non-interacting accounts MUST NOT indefinitely pin the matured-profit haircut denominator `h` with fresh unwarmed PnL. Touched accounts MUST make warmup progress.
13. **Funding / mark / ADL exactness under laziness:** any economic quantity whose correct value depends on the position held over an interval MUST be represented through the A/K side-index mechanism or a formally equivalent event-segmented method. In this revision, mark and ADL use integer `K_side`, while funding uses the high-precision cumulative numerator index `F_side_num`. Integer rounding at settlement MUST NOT mint positive aggregate claims.
14. **No hidden protocol MM:** the protocol MUST NOT secretly internalize user flow against an undisclosed residual inventory.
15. **Defined recovery from precision stress:** the engine MUST define deterministic recovery when side precision is exhausted. It MUST NOT rely on assertion failure, silent overflow, or permanent `DrainOnly` states.
16. **No sequential quantity dependency:** same-epoch account settlement MUST be fully local. It MAY depend on the account's own stored basis and current global side state, but MUST NOT require a canonical-order prefix or global carry cursor.
17. **Protocol-fee neutrality:** explicit protocol fees MUST either be collected into `I` immediately or tracked as account-local fee debt up to the account's collectible capital-plus-fee-debt limit. Any explicit fee amount beyond that collectible limit MUST be dropped rather than socialized through `h`, through `g`, or inflated into bankruptcy deficit `D`.
18. **Synthetic liquidation price integrity:** a synthetic liquidation close MUST execute at the current oracle mark with zero execution-price slippage. Any liquidation penalty MUST be represented only by explicit fee state.
19. **Loss seniority over engine-native protocol fees:** when a trade or a non-bankruptcy liquidation realizes trading losses for an account, those losses are senior to engine-native trade and liquidation fee collection from that same local capital state.
20. **Deterministic overflow handling:** any arithmetic condition that is not proven unreachable by the spec's numeric bounds MUST have a deterministic fail-safe or bounded fallback path. Silent wrap, unchecked panic, and undefined truncation are forbidden.
21. **Finite-capacity liveness:** because account capacity is finite, the engine MUST provide permissionless dead-account reclamation or equivalent slot reuse so abandoned empty accounts and flat dust accounts below the live-balance floor cannot permanently exhaust capacity.
22. **Permissionless off-chain keeper compatibility:** candidate discovery MAY be performed entirely off chain. The engine MUST expose exact current-state shortlist processing and targeted per-account settle / liquidate / reclaim / resolved-close paths so any permissionless keeper can make liquidation and reset progress without any required on-chain phase-1 scan.
23. **No pure-capital insurance draw without accrual:** a pure capital-only instruction that does not call `accrue_market_to` MUST NOT decrement `I` or record uninsured protocol loss.
24. **Configuration immutability within a market instance:** the warmup bounds, trade-fee, margin, liquidation, insurance-floor, and live-balance-floor parameters that define a market instance MUST remain fixed for the lifetime of that instance unless a future revision defines an explicit safe update procedure.
25. **Exact warmup horizon semantics:** the locked warmup horizon for a reserve cohort MUST mean what it says up to integer flooring of claim units; tiny reserve cohorts MUST NOT release materially faster than their sampled horizon solely because of an approximate slope representation.
26. **Resolved-market close exactness:** resolved-market force close MUST be defined through canonical helpers. It MUST NOT rely on direct zero-writes that bypass `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, reserve-cohort state, or reset counters.
27. **Path-independent touched-account finalization:** flat auto-conversion and fee-debt sweep on live touched accounts MUST depend only on the post-live touched state and the shared conversion snapshot, not on whether the instruction was single-touch or multi-touch.
28. **No resolved payout race:** resolved accounts with positive claims MUST NOT be terminally paid out until stale-account reconciliation is complete across both sides. Early closers MUST NOT be able to outrun later negative final settlements.
29. **Path-independent resolved terminal payouts:** once stale-account reconciliation is complete and terminal payout becomes unlocked, all positive resolved payouts MUST use one shared resolved-payout snapshot so caller order cannot improve the payout ratio.

30. **Bounded resolved settlement price:** the resolved settlement price used in `resolve_market` MUST remain within an immutable deviation band of the last live effective mark `P_last`.
31. **No permissionless haircut realization of flat released profit:** automatic flat conversion in live instructions MUST occur only at a whole snapshot (`h = 1`). Any lossy conversion of released profit under `h < 1` MUST be an explicit user action.

**Atomic execution model:** every top-level external instruction defined in §10 MUST be atomic. If any required precondition, checked-arithmetic guard, or conservative-failure condition fails, the instruction MUST roll back all state mutations performed since that instruction began.

---

## 1. Types, units, scaling, and arithmetic requirements

### 1.1 Amounts

- `u128` unsigned amounts are denominated in quote-token atomic units, positive-PnL aggregates, OI, fixed-point position magnitudes, and bounded fee amounts.
- `i128` signed amounts represent realized PnL, K-space liabilities, and fee-credit balances.
- `wide_signed` means any transient exact signed intermediate domain wider than `i128` (for example `i256`) or an equivalent exact comparison-preserving construction.
- All persistent state MUST fit natively into 128-bit boundaries. Emulated wide integers are permitted only within transient intermediate math steps.

### 1.2 Prices and internal positions

- `POS_SCALE = 1_000_000` (6 decimal places of position precision).
- `price: u64` is quote-token atomic units per `1` base. There is no separate price scale.
- All external price inputs, including `oracle_price`, `exec_price`, `resolved_price`, and any stored funding-price sample, MUST satisfy `0 < price <= MAX_ORACLE_PRICE`.
- Internally the engine stores position bases as signed fixed-point base quantities:
  - `basis_pos_q_i: i128`, units `(base * POS_SCALE)`.
- Effective oracle notional is:
  - `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`.
- Trade fees MUST use executed trade size, not account notional:
  - `trade_notional = mul_div_floor_u128(size_q, exec_price, POS_SCALE)`.

### 1.3 A/K and funding-index scale

- `ADL_ONE = 1_000_000` (6 decimal places of fractional decay accuracy).
- `A_side` is dimensionless and scaled by `ADL_ONE`.
- `K_side` has units `(ADL scale) * (quote atomic units per 1 base)` and carries whole-unit mark / ADL index motion.
- `FUNDING_DEN = 1_000_000_000`.
- `F_side_num` has units `(ADL scale) * (quote atomic units per 1 base) * FUNDING_DEN` and carries exact cumulative funding numerator motion.


### 1.4 Concrete normative bounds

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
- `MAX_PNL_POS_TOT = MAX_MATERIALIZED_ACCOUNTS * MAX_ACCOUNT_POSITIVE_PNL = 100_000_000_000_000_000_000_000_000_000_000_000_000`
- `MIN_A_SIDE = 1_000`
- `MAX_WARMUP_SLOTS = 18_446_744_073_709_551_615`
- `MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT = 62`
- `MAX_OVERFLOW_RESERVE_SEGMENTS_PER_ACCOUNT = 2`
- `MAX_RESERVE_SEGMENTS_PER_ACCOUNT = MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT + MAX_OVERFLOW_RESERVE_SEGMENTS_PER_ACCOUNT = 64`
- `MAX_RESOLVE_PRICE_DEVIATION_BPS = 10_000`
- `0 <= I_floor <= MAX_VAULT_TVL`
- `0 <= min_liquidation_abs <= liquidation_fee_cap <= MAX_PROTOCOL_FEE_ABS`

Configured values MUST satisfy:

- `0 < MIN_INITIAL_DEPOSIT <= MAX_VAULT_TVL`
- `0 < MIN_NONZERO_MM_REQ < MIN_NONZERO_IM_REQ <= MIN_INITIAL_DEPOSIT`
- deployment configuration of `MIN_INITIAL_DEPOSIT`, `MIN_NONZERO_MM_REQ`, and `MIN_NONZERO_IM_REQ` MUST be economically non-trivial for the quote token and MUST NOT be set below the deployment's tolerated slot-pinning dust threshold
- `0 <= maintenance_bps <= initial_bps <= MAX_INITIAL_BPS`
- `0 <= H_min <= H_max <= MAX_WARMUP_SLOTS`
- `0 <= resolve_price_deviation_bps <= MAX_RESOLVE_PRICE_DEVIATION_BPS`
- `A_side > 0` whenever `OI_eff_side > 0` and the side is still representable

If the deployment also defines a stale-market resolution delay `permissionless_resolve_stale_slots` after which ordinary live-market settlement no longer occurs, then market initialization MUST additionally require:

- `H_max <= permissionless_resolve_stale_slots`

Dust accounting interpretation:

- `stored_pos_count_side` MAY be used as a q-unit conservative term in phantom-dust accounting because each live stored position can contribute at most one additional q-unit from threshold crossing whenever `A_side` changes under ADL quantity socialization, even if the global ratio divides evenly.

### 1.5 Trusted time / oracle requirements

- `now_slot` in all top-level instructions MUST come from trusted runtime slot metadata or a formally equivalent trusted source. Production entrypoints MUST NOT accept an arbitrary user-specified substitute.
- `oracle_price` MUST come from a validated configured oracle feed. Stale, invalid, or out-of-range oracle reads MUST fail conservatively before state mutation.
- Any helper or instruction that accepts `now_slot` MUST require `now_slot >= current_slot`.
- Any call to `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` MUST require `now_slot >= slot_last`.
- `current_slot` and `slot_last` MUST be monotonically nondecreasing.

### 1.6 Arithmetic requirements

The engine MUST satisfy all of the following.

1. All products involving `A_side`, `K_side`, `F_side_num`, `k_snap_i`, `f_snap_i`, `basis_pos_q_i`, `effective_pos_q(i)`, `price`, the raw funding numerator `fund_px_0 * funding_rate_e9_per_slot * dt_sub`, trade-haircut numerators, trade-open counterfactual positive-aggregate numerators, reserve-cohort release numerators, or ADL deltas MUST use checked arithmetic.
2. When `funding_rate_e9_per_slot != 0` and the accrual interval `dt > 0`, `accrue_market_to` MUST split `dt` into consecutive sub-steps each of length `dt_sub <= MAX_FUNDING_DT`, with any shorter remainder last. Mark-to-market MUST be applied once before the funding sub-step loop.
3. The conservation check `V >= C_tot + I` and any `Residual` computation MUST use checked addition for `C_tot + I`. Overflow is an invariant violation.
4. Signed division with positive denominator MUST use exact conservative floor division.
5. Exact multiply-divide helpers MUST return the exact quotient even when the exact product exceeds native `u128`, provided the exact final quotient fits.
6. `PendingWarmupTot = PNL_pos_tot - PNL_matured_pos_tot` MUST use checked subtraction.
7. Haircut paths `floor(ReleasedPos_i * h_num / h_den)`, `floor(PosPNL_i * g_num / g_den)`, and the exact candidate-open trade-haircut path of §3.5 MUST use exact multiply-divide helpers.
8. `max_safe_flat_conversion_released` MUST use an exact capped multiply-divide or an equivalent exact wide comparison. If the uncapped mathematical quotient exceeds either `x_cap` or `u128::MAX`, the helper MUST return `x_cap` rather than revert.
9. Funding sub-steps MUST use the same exact `fund_num_step = fund_px_0 * funding_rate_e9_per_slot * dt_sub` value for both sides' `F_side_num` deltas. The engine MUST NOT floor-divide `fund_num_step` inside `accrue_market_to`.
10. `K_side` and `F_side_num` are cumulative across epochs. Implementations MUST use checked arithmetic and revert on persistent `i128` overflow.
11. Same-epoch or epoch-mismatch settlement MUST combine `K_side` and `F_side_num` through the exact helper `wide_signed_mul_div_floor_from_kf_pair`, evaluating the exact signed numerator `((K_diff * FUNDING_DEN) + F_diff_num)` in a transient wide signed type before division by `(a_basis * POS_SCALE * FUNDING_DEN)`.
12. The ADL quote-deficit path MUST compute `delta_K_abs = ceil(D_rem * A_old * POS_SCALE / OI)` using exact wide arithmetic.
13. If a K-space K-index delta is representable as a magnitude but the signed addition `K_opp + delta_K_exact` overflows `i128`, the engine MUST route `D_rem` through `record_uninsured_protocol_loss` while still continuing quantity socialization.
14. `PNL_i` MUST be maintained in `[i128::MIN + 1, i128::MAX]`, and `fee_credits_i` MUST be maintained in `[i128::MIN + 1, 0]`. `i128::MIN` is forbidden.
15. Every decrement of `stored_pos_count_*`, `stale_account_count_*`, or `phantom_dust_bound_*_q` MUST use checked subtraction. Underflow indicates corruption and MUST fail conservatively.
16. Every increment of `stored_pos_count_*`, `phantom_dust_bound_*_q`, `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, `V`, or `I` MUST use checked addition and MUST enforce the relevant configured bound.
17. `trade_notional <= MAX_ACCOUNT_NOTIONAL` MUST be enforced before charging trade fees.
18. Any out-of-bound external price input, any invalid oracle read, any out-of-range wrapper-supplied `H_lock`, any out-of-range wrapper-supplied `funding_rate_e9_per_slot`, or any non-monotonic slot input MUST fail conservatively before state mutation.
19. `charge_fee_to_insurance` MUST cap its applied fee at the account's exact collectible capital-plus-fee-debt headroom. It MUST never set `fee_credits_i < -(i128::MAX)`.
20. Any direct fee-credit repayment path MUST cap its applied amount at the exact current `FeeDebt_i`. It MUST never set `fee_credits_i > 0`.
21. Any direct insurance top-up or direct fee-credit repayment path that increases `V` or `I` MUST use checked addition and MUST enforce `MAX_VAULT_TVL`.
22. Any reserve-cohort mutation MUST preserve the invariants of §2.1 and MUST use checked arithmetic.
23. The exact counterfactual trade-open computation MUST recompute the account's positive-PnL contribution and the global positive-PnL aggregate with the candidate trade's own positive slippage gain removed. Subtracting the raw gain from already haircutted trade equity is non-compliant.
24. Any wrapper-owned fee amount routed through the canonical helper MUST satisfy `fee_abs <= MAX_PROTOCOL_FEE_ABS`.
25. `append_or_route_new_reserve` MUST preserve `len(exact_reserve_cohorts_i) <= MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`, at most one `overflow_older_i`, at most one `overflow_newest_i`, and total stored reserve segments per account `<= MAX_RESERVE_SEGMENTS_PER_ACCOUNT`. When exact capacity is exhausted, any conservative bounded-storage approximation MUST be routed only into overflow segments whose `horizon_slots` are fixed to `H_overflow = H_max`; older exact cohorts and `overflow_older_i` MUST remain unchanged.
26. If `reserve_mode` does not create new reserve (`ImmediateRelease` or `UseHLock(0)`), `PNL_matured_pos_tot` MUST increase only by the true newly released increment; pre-existing reserve MUST NOT be double-counted.
27. Funding exactness MUST NOT depend on cross-call quotient carry that can be inherited by newly attached positions. Any retained fractional funding precision across calls MUST be represented through snapshot-attached state such as `F_side_num` / `f_snap_i`, not through a bare global remainder with no per-account snapshot.

### 1.7 Reference numeric envelope

The always-wide paths in this revision are:

1. exact `pnl_delta`
2. exact matured-haircut and trade-haircut multiply-divides
3. exact counterfactual trade-open positive-aggregate and haircut computation
4. exact ADL `delta_K_abs`
5. exact combined `K_side` / `F_side_num` settlement via `wide_signed_mul_div_floor_from_kf_pair`

All other arithmetic MAY still use wider temporaries whenever convenient.

---

## 2. State model

### 2.1 Account state

For each materialized account `i`, the engine stores at least:

- `C_i: u128` — protected principal
- `PNL_i: i128` — realized PnL claim
- `R_i: u128` — aggregate reserved positive PnL, with `0 <= R_i <= max(PNL_i, 0)`
- `basis_pos_q_i: i128` — signed fixed-point base basis at the last explicit position mutation or forced zeroing
- `a_basis_i: u128` — side multiplier in effect when `basis_pos_q_i` was last explicitly attached
- `k_snap_i: i128` — last realized whole-unit `K_side` snapshot
- `f_snap_i: i128` — last realized high-precision cumulative funding numerator snapshot
- `epoch_snap_i: u64` — side epoch in which the basis is defined
- `fee_credits_i: i128`

Each account additionally stores:

- an **exact reserve-cohort queue** `exact_reserve_cohorts_i[]`, ordered from oldest cohort to newest cohort
- one optional **older preserved overflow cohort** `overflow_older_i ∈ {None, Some(Cohort)}`
- one optional **newest pending overflow segment** `overflow_newest_i ∈ {None, Some(Cohort)}`

Each exact cohort and the preserved overflow cohort store:

- `remaining_q: u128` — still-reserved amount from this cohort
- `anchor_q: u128` — cohort size at creation or exact same-slot same-horizon merge time
- `start_slot: u64` — slot at which this scheduled cohort was created
- `horizon_slots: u64` — locked warmup horizon for this scheduled cohort
- `sched_release_q: u128` — cumulative time-scheduled release already accounted for from this scheduled cohort

The newest pending overflow segment reuses the same fields with the following special semantics while pending:

- `remaining_q: u128` — still-reserved pending amount
- `anchor_q: u128` — cumulative pending amount added since this pending segment was created
- `start_slot: u64` — inert metadata while pending; implementations SHOULD set it to the slot of the most recent pending mutation
- `horizon_slots: u64` — MUST equal the immutable overflow horizon `H_overflow = H_max` while pending and after later activation
- `sched_release_q: u128` — MUST remain `0` while pending; pending reserve does not mature until activated

Storage bounds:

- `len(exact_reserve_cohorts_i) <= MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`
- at most one `overflow_older_i` may be present
- at most one `overflow_newest_i` may be present
- define `has_overflow_older_i = 1` if `overflow_older_i` is present and `0` otherwise
- define `has_overflow_newest_i = 1` if `overflow_newest_i` is present and `0` otherwise
- total stored reserve segments per account MUST satisfy `len(exact_reserve_cohorts_i) + has_overflow_older_i + has_overflow_newest_i <= MAX_RESERVE_SEGMENTS_PER_ACCOUNT`

Derived local quantities on a touched state:

- `PosPNL_i = max(PNL_i, 0)`
- if `market_mode == Live`, `ReleasedPos_i = PosPNL_i - R_i`
- if `market_mode == Resolved`, `ReleasedPos_i = PosPNL_i`
- `FeeDebt_i = fee_debt_u128_checked(fee_credits_i)`

Reserve-segment invariants:

- define `H_overflow = H_max`
- if `market_mode == Live`, `R_i = Σ exact_cohort.remaining_q + overflow_older.remaining_q_if_present + overflow_newest.remaining_q_if_present`
- if `market_mode == Live`, every exact cohort satisfies:
  - `0 < cohort.anchor_q`
  - `H_min <= cohort.horizon_slots <= H_max`
  - `0 <= cohort.sched_release_q <= cohort.anchor_q`
  - `0 < cohort.remaining_q <= cohort.anchor_q`
- if `market_mode == Live` and `overflow_older_i` is present:
  - `0 < overflow_older_i.anchor_q`
  - `overflow_older_i.horizon_slots == H_overflow`
  - `0 <= overflow_older_i.sched_release_q <= overflow_older_i.anchor_q`
  - `0 < overflow_older_i.remaining_q <= overflow_older_i.anchor_q`
- if `market_mode == Live` and `overflow_newest_i` is present:
  - `0 < overflow_newest_i.anchor_q`
  - `overflow_newest_i.horizon_slots == H_overflow`
  - `overflow_newest_i.sched_release_q == 0`
  - `0 < overflow_newest_i.remaining_q <= overflow_newest_i.anchor_q`
- if `R_i == 0`, the exact reserve queue MUST be empty and both overflow segments MUST be absent
- exact cohort order is chronological by `start_slot`; for equal `start_slot`, insertion order is preserved
- if `overflow_older_i` is present, it is economically newer than every exact cohort
- if `overflow_newest_i` is present, it is economically newer than every exact cohort and, if `overflow_older_i` is present, newer than `overflow_older_i`
- when exact capacity is exhausted, new reserve MAY mutate `overflow_newest_i` but MUST NOT mutate older exact cohorts or `overflow_older_i`
- while pending, `overflow_newest_i` does not auto-mature and does not consume schedule progress; when activated into a scheduled cohort, the activated cohort MUST start at `current_slot` with `anchor_q = remaining_q`, `sched_release_q = 0`, and `horizon_slots = H_overflow`
- if `market_mode == Resolved`, reserve storage is economically inert because all reserve is globally treated as mature; any resolved-account touch that will mutate `PNL_i` MUST first clear the exact reserve queue, `overflow_older_i`, and `overflow_newest_i` via `prepare_account_for_resolved_touch(i)`

Fee-credit bounds:

- `fee_credits_i` MUST be initialized to `0`
- the engine MUST maintain `-(i128::MAX) <= fee_credits_i <= 0` at all times
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
- `F_long_num: i128` — cumulative high-precision funding numerator index for the long side, in `FUNDING_DEN` units
- `F_short_num: i128` — cumulative high-precision funding numerator index for the short side, in `FUNDING_DEN` units
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

- `C_tot: u128 = Σ C_i`
- `PNL_pos_tot: u128 = Σ max(PNL_i, 0)`
- `PNL_matured_pos_tot: u128 = Σ ReleasedPos_i`

Market-resolution state:

- `market_mode ∈ {Live, Resolved}`
- `resolved_price: u64`
- `resolved_slot: u64`
- `resolved_payout_snapshot_ready: bool`
- `resolved_payout_h_num: u128`
- `resolved_payout_h_den: u128`

Derived global quantities:

- `PendingWarmupTot = checked_sub_u128(PNL_pos_tot, PNL_matured_pos_tot)`

Global invariants:

- `PNL_matured_pos_tot <= PNL_pos_tot <= MAX_PNL_POS_TOT`
- `C_tot <= V <= MAX_VAULT_TVL`
- `I <= V`
- `F_long_num` and `F_short_num` MUST remain representable as `i128`
- if `market_mode == Live`, `resolved_price` MAY be `0`
- if `market_mode == Resolved`, then `resolved_price > 0` and `resolved_slot <= current_slot`
- if `resolved_payout_snapshot_ready == false`, then `resolved_payout_h_num == 0` and `resolved_payout_h_den == 0`
- if `resolved_payout_snapshot_ready == true`, then `market_mode == Resolved` and `resolved_payout_h_num <= resolved_payout_h_den`

### 2.2.1 Instruction context (ephemeral, non-persistent)

Every top-level live instruction that uses the standard lifecycle MUST initialize a fresh ephemeral context `ctx` that stores at least:

- `pending_reset_long: bool`
- `pending_reset_short: bool`
- `H_lock_shared: u64`
- `touched_accounts[]` — a deduplicated instruction-local list of account identifiers touched by `touch_account_live_local`

`ctx` is not persistent market state.

### 2.2.2 Configuration immutability

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

### 2.3 Materialized-account capacity

The engine MUST track the number of currently materialized account slots. That count MUST NOT exceed `MAX_MATERIALIZED_ACCOUNTS`.

A missing account is one whose slot is not currently materialized. Missing accounts MUST NOT be auto-materialized by `settle_account`, `withdraw`, `execute_trade`, `liquidate`, `resolve_market`, `force_close_resolved`, or `keeper_crank`.

Only the following path MAY materialize a missing account in this specification:

- `deposit(i, amount, now_slot)` with `amount >= MIN_INITIAL_DEPOSIT`

### 2.4 Canonical zero-position defaults

The canonical zero-position account defaults are:

- `basis_pos_q_i = 0`
- `a_basis_i = ADL_ONE`
- `k_snap_i = 0`
- `f_snap_i = 0`
- `epoch_snap_i = 0`

### 2.5 Account materialization

`materialize_account(i, slot_anchor)` MAY succeed only if the account is currently missing and materialized-account capacity remains below `MAX_MATERIALIZED_ACCOUNTS`.

On success, it MUST increment the materialized-account count and set:

- `C_i = 0`
- `PNL_i = 0`
- `R_i = 0`
- canonical zero-position defaults from §2.4
- `fee_credits_i = 0`
- exact reserve queue empty and both overflow cohorts absent

### 2.6 Permissionless empty- or flat-dust-account reclamation

The engine MUST provide a permissionless reclamation path `reclaim_empty_account(i, now_slot)`.

It MAY begin only if all of the following hold on the pre-reclaim state:

- account `i` is materialized
- trusted `now_slot >= current_slot`
- `PNL_i == 0`
- `R_i == 0`
- exact reserve queue is empty and both overflow cohorts are absent
- `basis_pos_q_i == 0`
- `fee_credits_i <= 0`

The path MUST then require final reclaim eligibility:

- `0 <= C_i < MIN_INITIAL_DEPOSIT`
- `PNL_i == 0`
- `R_i == 0`
- exact reserve queue is empty and both overflow cohorts are absent
- `basis_pos_q_i == 0`
- `fee_credits_i <= 0`

On success, it MUST:

- if `C_i > 0`:
  - let `dust = C_i`
  - `set_capital(i, 0)`
  - `I = checked_add_u128(I, dust)`
- forgive any negative `fee_credits_i` by setting `fee_credits_i = 0`
- reset all local fields to canonical zero
- mark the slot missing / reusable
- decrement the materialized-account count

### 2.7 Initial market state

Market initialization MUST take, at minimum, `init_slot`, `init_oracle_price`, and configured fee / margin / insurance / materialization parameters.

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
- `market_mode = Live`
- `resolved_price = 0`
- `resolved_slot = init_slot`
- `resolved_payout_snapshot_ready = false`
- `resolved_payout_h_num = 0`
- `resolved_payout_h_den = 0`

### 2.8 Side modes and reset lifecycle

A side may be in one of three modes:

- `Normal`: ordinary operation
- `DrainOnly`: the side is live but has decayed below the safe precision threshold; OI on that side may decrease but MUST NOT increase
- `ResetPending`: the side has been fully drained and its prior epoch is awaiting stale-account reconciliation; no operation may increase OI on that side

`begin_full_drain_reset(side)` MAY succeed only if `OI_eff_side == 0`. It MUST:

1. set `K_epoch_start_side = K_side`
2. set `F_epoch_start_side_num = F_side_num`
3. increment `epoch_side` by exactly `1`
4. set `A_side = ADL_ONE`
5. set `stale_account_count_side = stored_pos_count_side`
6. set `phantom_dust_bound_side_q = 0`
7. set `mode_side = ResetPending`

`finalize_side_reset(side)` MAY succeed only if all of the following hold:

- `mode_side == ResetPending`
- `OI_eff_side == 0`
- `stale_account_count_side == 0`
- `stored_pos_count_side == 0`

On success, it MUST set `mode_side = Normal`.

`maybe_finalize_ready_reset_sides_before_oi_increase()` MUST check each side independently and, if the `finalize_side_reset(side)` preconditions already hold, immediately finalize that side. It MUST NOT begin a new reset or mutate OI.

### 2.8.1 Epoch-gap invariant

For every materialized account with `basis_pos_q_i != 0` on side `s`, the engine MUST maintain exactly one of the following states:

- **current attachment:** `epoch_snap_i == epoch_s`
- **stale one-epoch lag:** `mode_s == ResetPending` and `epoch_snap_i + 1 == epoch_s`

Epoch gaps larger than `1` are forbidden.

---

## 3. Solvency, haircuts, and live equity

### 3.1 Residual backing available to junior positive PnL

Define:

- `senior_sum = checked_add_u128(C_tot, I)`
- `Residual = max(0, V - senior_sum)`

Invariant: the engine MUST maintain `V >= senior_sum` at all times.

### 3.2 Positive-PnL aggregates

Define:

- `PosPNL_i = max(PNL_i, 0)`
- if `market_mode == Live`, `ReleasedPos_i = PosPNL_i - R_i`
- if `market_mode == Resolved`, `ReleasedPos_i = PosPNL_i`
- `PendingWarmupTot = PNL_pos_tot - PNL_matured_pos_tot = Σ R_i` on live markets

Reserved fresh positive PnL MUST increase `PNL_pos_tot` immediately but MUST NOT increase `PNL_matured_pos_tot` until released through warmup. On a resolved market, all remaining positive PnL is treated as matured for haircut purposes.

### 3.3 Matured withdrawal / conversion haircut `h`

This haircut governs only:

- withdrawal-style approval
- principal conversion
- matured-profit extraction semantics

Let:

- if `PNL_matured_pos_tot == 0`, define `h = 1`
- else:
  - `h_num = min(Residual, PNL_matured_pos_tot)`
  - `h_den = PNL_matured_pos_tot`

For account `i` on a touched state:

- if `PNL_matured_pos_tot == 0`, `PNL_eff_matured_i = ReleasedPos_i`
- else `PNL_eff_matured_i = mul_div_floor_u128(ReleasedPos_i, h_num, h_den)`

Because each account is floored independently:

- `Σ PNL_eff_matured_i <= h_num <= Residual`

### 3.4 Trade-collateral haircut `g`

This haircut governs only risk-increasing trade approval.

It intentionally spans **all** positive PnL, matured or unmatured.

Let:

- if `PNL_pos_tot == 0`, define `g = 1`
- else:
  - `g_num = min(Residual, PNL_pos_tot)`
  - `g_den = PNL_pos_tot`

For account `i` on a touched state:

- if `PNL_pos_tot == 0`, `PNL_eff_trade_i = PosPNL_i`
- else `PNL_eff_trade_i = mul_div_floor_u128(PosPNL_i, g_num, g_den)`

Aggregate bound:

- `Σ PNL_eff_trade_i <= g_num <= Residual`

### 3.5 Live equity used by maintenance, trading, withdrawal, and trade-open approval

For account `i` on a touched state, define:

- `Eq_withdraw_base_i = (C_i as wide_signed) + min(PNL_i, 0) + (PNL_eff_matured_i as wide_signed)`
- `Eq_withdraw_raw_i = Eq_withdraw_base_i - (FeeDebt_i as wide_signed)`
- `Eq_withdraw_net_i = max(0, Eq_withdraw_raw_i)`

- `Eq_trade_base_i = (C_i as wide_signed) + min(PNL_i, 0) + (PNL_eff_trade_i as wide_signed)`
- `Eq_trade_raw_i = Eq_trade_base_i - (FeeDebt_i as wide_signed)`

- `Eq_maint_raw_i = (C_i as wide_signed) + (PNL_i as wide_signed) - (FeeDebt_i as wide_signed)`
- `Eq_net_i = max(0, Eq_maint_raw_i)`

For **candidate trade approval only**, define the transient non-persistent quantities for account `i`:

- `candidate_trade_pnl_i` = the signed execution-slippage PnL created for account `i` by the candidate trade currently under evaluation
- `TradeGain_i_candidate = max(candidate_trade_pnl_i, 0) as u128`
- `PNL_trade_open_i = PNL_i - (TradeGain_i_candidate as i128)`
- `PosPNL_trade_open_i = max(PNL_trade_open_i, 0)`

Let the current post-candidate state's positive contribution of account `i` be `PosPNL_i = max(PNL_i, 0)`. Then define the exact counterfactual global positive aggregate with the candidate trade's own positive slippage gain removed:

- `PNL_pos_tot_trade_open_i = checked_add_u128(checked_sub_u128(PNL_pos_tot, PosPNL_i), PosPNL_trade_open_i)`

Now define the exact counterfactual trade haircut applied to the counterfactual positive state:

- if `PNL_pos_tot_trade_open_i == 0`, `PNL_eff_trade_open_i = PosPNL_trade_open_i`
- else:
  - `g_open_num_i = min(Residual, PNL_pos_tot_trade_open_i)`
  - `g_open_den_i = PNL_pos_tot_trade_open_i`
  - `PNL_eff_trade_open_i = mul_div_floor_u128(PosPNL_trade_open_i, g_open_num_i, g_open_den_i)`

Then define the exact risk-increasing trade approval metric:

- `Eq_trade_open_base_i = (C_i as wide_signed) + min(PNL_trade_open_i, 0) + (PNL_eff_trade_open_i as wide_signed)`
- `Eq_trade_open_raw_i = Eq_trade_open_base_i - (FeeDebt_i as wide_signed)`

Outside a candidate trade approval check, implementations MUST treat `candidate_trade_pnl_i = 0`, so `Eq_trade_open_raw_i = Eq_trade_raw_i`.

Interpretation:

- `Eq_withdraw_raw_i` is the conservative extraction lane
- `Eq_trade_raw_i` is the pre-neutralization trade lane
- `Eq_trade_open_raw_i` is the only compliant risk-increasing trade approval metric
- `Eq_maint_raw_i` is the maintenance lane
- strict risk-reducing buffer comparisons MUST use `Eq_maint_raw_i`, not a clamped net quantity

Important consequences:

- pure warmup release on unchanged `C_i`, `PNL_i`, and `fee_credits_i` can increase `Eq_withdraw_raw_i`
- pure warmup release on unchanged `C_i`, `PNL_i`, and `fee_credits_i` does not by itself change `Eq_trade_raw_i`, `Eq_trade_open_raw_i` (with `candidate_trade_pnl_i = 0`), or `Eq_maint_raw_i`
- a candidate trade's own positive execution-slippage PnL can never increase `Eq_trade_open_raw_i`

### 3.6 Conservatism under pending A/K side effects and warmup

Because `h` uses only matured positive PnL:

- pending positive side effects MUST NOT become withdrawable or principal-convertible until touched, reserved, and later released through warmup
- on the touched generating account, full local `PNL_i` MAY support maintenance
- on the touched generating account, positive local `PNL_i` MAY support risk-increasing trades only through `g`
- reserved fresh positive PnL MUST NOT enter the matured-profit haircut denominator `h` before warmup release
- pending lazy ADL obligations MUST NOT be counted as backing in `Residual`

---

## 4. Canonical helpers

### 4.1 Checked scalar helpers

`checked_add_u128`, `checked_sub_u128`, `checked_add_i128`, `checked_sub_i128`, `checked_mul_u128`, `checked_mul_i128`, `checked_cast_i128`, and any equivalent low-level helper MUST either return the exact value or fail conservatively on overflow or underflow.

### 4.2 `set_capital(i, new_C)`

When changing `C_i` from `old_C` to `new_C`, the engine MUST update `C_tot` by the signed delta in checked arithmetic and then set `C_i = new_C`.

### 4.3 `set_position_basis_q(i, new_basis_pos_q)`

When changing stored `basis_pos_q_i` from `old` to `new`, the engine MUST update `stored_pos_count_long` and `stored_pos_count_short` exactly once using the sign flags of `old` and `new`, then write `basis_pos_q_i = new`.

Normative implementation law:

1. if `old > 0` and `new_basis_pos_q <= 0`, decrement `stored_pos_count_long` using checked subtraction
2. if `old < 0` and `new_basis_pos_q >= 0`, decrement `stored_pos_count_short` using checked subtraction
3. if `new_basis_pos_q > 0` and `old <= 0`:
   - increment `stored_pos_count_long` using checked addition
   - require resulting `stored_pos_count_long <= MAX_ACTIVE_POSITIONS_PER_SIDE`
4. if `new_basis_pos_q < 0` and `old >= 0`:
   - increment `stored_pos_count_short` using checked addition
   - require resulting `stored_pos_count_short <= MAX_ACTIVE_POSITIONS_PER_SIDE`
5. write `basis_pos_q_i = new_basis_pos_q`

For a single logical position change, `set_position_basis_q` MUST be called exactly once with the final target. Passing through an intermediate zero value is not permitted.

### 4.4 Reserve-cohort helper rules

This revision keeps exact account-local reserve cohorts up to a fixed exact-capacity bound, then uses at most two bounded overflow segments:

- `overflow_older_i`, a preserved **scheduled** overflow cohort whose already accrued maturity progress is never reset by newer additions, and
- `overflow_newest_i`, an **unscheduled pending** overflow segment that absorbs any further post-saturation reserve conservatively without mutating older exact cohorts or `overflow_older_i`.

The engine does **not** compute the exact-cohort horizon. It receives `H_lock` from the wrapper, validates it, and stores it only on newly created exact cohorts. Overflow segments always use `H_overflow = H_max`.

#### 4.4.1 `append_or_route_new_reserve(i, reserve_add, now_slot, H_lock)`

Preconditions:

- `reserve_add > 0`
- `H_min <= H_lock <= H_max`
- `market_mode == Live`

Effects:

Define `H_overflow = H_max`.

1. if `overflow_older_i` is present and `len(exact_reserve_cohorts_i) < MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`:
   - promote `overflow_older_i` into the exact reserve queue as the newest exact cohort
   - clear `overflow_older_i`
2. if `overflow_older_i` is absent and `overflow_newest_i` is present:
   - let `pending_q = overflow_newest_i.remaining_q`
   - clear `overflow_newest_i`
   - if `len(exact_reserve_cohorts_i) < MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`:
     - append one new exact cohort with:
       - `remaining_q = pending_q`
       - `anchor_q = pending_q`
       - `start_slot = now_slot`
       - `horizon_slots = H_overflow`
       - `sched_release_q = 0`
   - else:
     - set `overflow_older_i` to one new scheduled cohort with:
       - `remaining_q = pending_q`
       - `anchor_q = pending_q`
       - `start_slot = now_slot`
       - `horizon_slots = H_overflow`
       - `sched_release_q = 0`
3. if `overflow_older_i` is absent and `overflow_newest_i` is absent and the newest exact cohort exists and all of the following hold:
   - `newest.start_slot == now_slot`
   - `newest.horizon_slots == H_lock`
   - `newest.sched_release_q == 0`
   then exact merge is permitted:
   - `newest.remaining_q = checked_add_u128(newest.remaining_q, reserve_add)`
   - `newest.anchor_q = checked_add_u128(newest.anchor_q, reserve_add)`
4. else if `overflow_older_i` is present and `overflow_newest_i` is absent and all of the following hold:
   - `overflow_older_i.start_slot == now_slot`
   - `overflow_older_i.sched_release_q == 0`
   then exact same-slot merge into `overflow_older_i` is permitted:
   - `overflow_older_i.remaining_q = checked_add_u128(overflow_older_i.remaining_q, reserve_add)`
   - `overflow_older_i.anchor_q = checked_add_u128(overflow_older_i.anchor_q, reserve_add)`
5. else if `len(exact_reserve_cohorts_i) < MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT` and `overflow_older_i` is absent and `overflow_newest_i` is absent:
   - append one new exact cohort with:
     - `remaining_q = reserve_add`
     - `anchor_q = reserve_add`
     - `start_slot = now_slot`
     - `horizon_slots = H_lock`
     - `sched_release_q = 0`
6. else if `overflow_older_i` is absent and `overflow_newest_i` is absent:
   - create one new `overflow_older_i` scheduled cohort with:
     - `remaining_q = reserve_add`
     - `anchor_q = reserve_add`
     - `start_slot = now_slot`
     - `horizon_slots = H_overflow`
     - `sched_release_q = 0`
7. else if `overflow_older_i` is present and `overflow_newest_i` is absent:
   - create one new `overflow_newest_i` pending segment with:
     - `remaining_q = reserve_add`
     - `anchor_q = reserve_add`
     - `start_slot = now_slot`
     - `horizon_slots = H_overflow`
     - `sched_release_q = 0`
8. else:
   - `overflow_newest_i.remaining_q = checked_add_u128(overflow_newest_i.remaining_q, reserve_add)`
   - `overflow_newest_i.anchor_q = checked_add_u128(overflow_newest_i.anchor_q, reserve_add)`
   - `overflow_newest_i.start_slot = now_slot`
   - `overflow_newest_i.horizon_slots` MUST remain equal to `H_overflow`
   - `overflow_newest_i.sched_release_q = 0`
9. set `R_i = checked_add_u128(R_i, reserve_add)`

Normative consequences:

- exact same-slot same-horizon merges remain exact
- older exact cohorts are never compacted, restarted, or merged away merely because exact capacity is exhausted
- `overflow_older_i` never has its schedule reset or extended by newer post-saturation additions
- any conservative bounded-storage approximation is confined to overflow segments whose horizon is fixed to `H_overflow = H_max`
- wrapper-supplied `H_lock` never mutates any overflow segment horizon
- whenever present, `overflow_newest_i` always remains the economically newest reserve segment for LIFO-loss purposes

#### 4.4.2 `apply_reserve_loss_lifo(i, reserve_loss)`

This helper consumes reserve-first losses from newest reserve to oldest reserve.

Preconditions:

- `reserve_loss > 0`
- `reserve_loss <= R_i`
- `market_mode == Live`

Effects:

1. `remaining = reserve_loss`
2. if `overflow_newest_i` is present and `remaining > 0`:
   - `take = min(remaining, overflow_newest_i.remaining_q)`
   - `overflow_newest_i.remaining_q = overflow_newest_i.remaining_q - take`
   - `R_i = checked_sub_u128(R_i, take)`
   - `remaining = remaining - take`
   - if `overflow_newest_i.remaining_q == 0`, clear `overflow_newest_i`
3. if `overflow_older_i` is present and `remaining > 0`:
   - `take = min(remaining, overflow_older_i.remaining_q)`
   - `overflow_older_i.remaining_q = overflow_older_i.remaining_q - take`
   - `R_i = checked_sub_u128(R_i, take)`
   - `remaining = remaining - take`
   - if `overflow_older_i.remaining_q == 0`, clear `overflow_older_i`
4. iterate exact reserve cohorts from newest exact to oldest exact while `remaining > 0`:
   - `take = min(remaining, cohort.remaining_q)`
   - `cohort.remaining_q = cohort.remaining_q - take`
   - `R_i = checked_sub_u128(R_i, take)`
   - `remaining = remaining - take`
5. require `remaining == 0`
6. remove all exact cohorts with `remaining_q == 0`
7. if `overflow_older_i` is absent and `overflow_newest_i` is present:
   - let `pending_q = overflow_newest_i.remaining_q`
   - let `pending_h = overflow_newest_i.horizon_slots`
   - clear `overflow_newest_i`
   - if `len(exact_reserve_cohorts_i) < MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`:
     - append one new exact cohort with:
       - `remaining_q = pending_q`
       - `anchor_q = pending_q`
       - `start_slot = current_slot`
       - `horizon_slots = pending_h`
       - `sched_release_q = 0`
   - else:
     - set `overflow_older_i` to one new scheduled cohort with:
       - `remaining_q = pending_q`
       - `anchor_q = pending_q`
       - `start_slot = current_slot`
       - `horizon_slots = pending_h`
       - `sched_release_q = 0`

Normative consequences:

- `overflow_newest_i` is always consumed before `overflow_older_i` and before any exact cohort
- `overflow_older_i`, if present, is always consumed before any exact cohort
- exact cohorts retain exact newest-to-oldest LIFO loss ordering even when storage saturation occurs
#### 4.4.3 `prepare_account_for_resolved_touch(i)`

This helper makes a resolved account locally consistent with the resolved-market global invariant that all remaining positive PnL is already matured.

Preconditions:

- `market_mode == Resolved`

Effects:

1. if `R_i == 0`:
   - require exact reserve queue is empty and both overflow cohorts are absent
   - return
2. empty the exact reserve queue
3. clear `overflow_older_i`
4. clear `overflow_newest_i`
5. set `R_i = 0`
6. do **not** mutate `PNL_matured_pos_tot`

Normative consequence:

- after `resolve_market`, all reserve is globally treated as mature by setting `PNL_matured_pos_tot = PNL_pos_tot`
- per-account resolved touches therefore clear local reserve bookkeeping without a second global aggregate change

### 4.5 `set_pnl(i, new_PNL, reserve_mode)`

This is the canonical helper for changing `PNL_i` while preserving reserve-queue and aggregate invariants.

`reserve_mode` MUST be one of:

- `UseHLock(H_lock)`
- `ImmediateRelease`
- `NoPositiveIncreaseAllowed`

Let:

- `old_pos = max(PNL_i, 0) as u128`
- if `market_mode == Live`, `old_rel = old_pos - R_i`
- if `market_mode == Resolved`, require `R_i == 0` and set `old_rel = old_pos`
- `new_pos = max(new_PNL, 0) as u128`

Procedure:

1. require `new_PNL != i128::MIN`
2. require `new_pos <= MAX_ACCOUNT_POSITIVE_PNL`
3. update `PNL_pos_tot` by the exact delta from `old_pos` to `new_pos`
4. require resulting `PNL_pos_tot <= MAX_PNL_POS_TOT`

#### Case A — positive increase (`new_pos > old_pos`)

5. `reserve_add = new_pos - old_pos`
6. set `PNL_i = new_PNL`
7. determine behavior from `reserve_mode`:
   - `UseHLock(H_lock)` -> validate `H_min <= H_lock <= H_max`
   - `ImmediateRelease` -> no reserve cohort is created
   - `NoPositiveIncreaseAllowed` -> fail conservatively
8. if `reserve_mode == ImmediateRelease`:
   - update `PNL_matured_pos_tot` by adding exactly `reserve_add`
   - require resulting `PNL_matured_pos_tot <= PNL_pos_tot`
   - return
9. if `reserve_mode == UseHLock(H_lock)` and `H_lock == 0`:
   - update `PNL_matured_pos_tot` by adding exactly `reserve_add`
   - require resulting `PNL_matured_pos_tot <= PNL_pos_tot`
   - return
10. require `market_mode == Live`
11. append the new reserve via `append_or_route_new_reserve(i, reserve_add, current_slot, H_lock)`
12. `PNL_matured_pos_tot` MUST remain unchanged in this case
13. require resulting `PNL_matured_pos_tot <= PNL_pos_tot`
14. return

#### Case B — no positive increase (`new_pos <= old_pos`)

15. `pos_loss = old_pos - new_pos`
16. if `market_mode == Live`:
   - `reserve_loss = min(pos_loss, R_i)`
   - if `reserve_loss > 0`, call `apply_reserve_loss_lifo(i, reserve_loss)`
   - `matured_loss = pos_loss - reserve_loss`
17. if `market_mode == Resolved`:
   - require `R_i == 0`
   - `matured_loss = pos_loss`
18. if `matured_loss > 0`, update `PNL_matured_pos_tot` by subtracting `matured_loss`
19. set `PNL_i = new_PNL`
20. if `new_pos == 0` and `market_mode == Live`, require exact reserve queue is empty, both overflow cohorts are absent, and `R_i == 0`
21. require resulting `PNL_matured_pos_tot <= PNL_pos_tot`

Normative consequence:

- positive increases append new reserve cohorts rather than restarting older ones
- true market losses consume newest reserve first, then mature released positive PnL
- on resolved accounts, all positive PnL is treated as mature, so positive decreases reduce `PNL_matured_pos_tot` one-for-one

### 4.6 `consume_released_pnl(i, x)`

This helper removes only matured released positive PnL on a **live** account and MUST leave all stored reserve segments unchanged.

Preconditions:

- `market_mode == Live`
- `x > 0`
- `x <= ReleasedPos_i`

Effects:

1. let `old_pos = max(PNL_i, 0) as u128`
2. let `old_rel = old_pos - R_i`
3. let `new_pos = old_pos - x`
4. let `new_rel = old_rel - x`
5. require `new_pos >= R_i`
6. update `PNL_pos_tot` by the exact delta from `old_pos` to `new_pos`
7. update `PNL_matured_pos_tot` by the exact delta from `old_rel` to `new_rel`
8. set `PNL_i = checked_sub_i128(PNL_i, checked_cast_i128(x))`
9. require resulting `PNL_matured_pos_tot <= PNL_pos_tot`

### 4.7 `advance_profit_warmup(i)`

This helper releases reserve according to each stored **scheduled** reserve segment's own locked horizon.

Preconditions:

- `market_mode == Live`

Procedure:

1. if `R_i == 0`:
   - require exact reserve queue is empty and both overflow segments are absent
   - return
2. iterate the exact reserve queue from oldest exact cohort to newest exact cohort, then process `overflow_older_i` if present; `overflow_newest_i` is pending and MUST NOT be advanced while pending:
   - `elapsed = current_slot - cohort.start_slot`
   - if `elapsed >= cohort.horizon_slots`, set `sched_total = cohort.anchor_q`
   - else set `sched_total = mul_div_floor_u128(cohort.anchor_q, elapsed as u128, cohort.horizon_slots as u128)`
   - require `sched_total >= cohort.sched_release_q`
   - `sched_increment = sched_total - cohort.sched_release_q`
   - `release = min(cohort.remaining_q, sched_increment)`
   - if `release > 0`:
     - `cohort.remaining_q = cohort.remaining_q - release`
     - `R_i = checked_sub_u128(R_i, release)`
     - update `PNL_matured_pos_tot` by adding `release`
   - set `cohort.sched_release_q = sched_total`
3. remove all exact cohorts with `remaining_q == 0`
4. if `overflow_older_i` is present and `overflow_older_i.remaining_q == 0`, clear `overflow_older_i`
5. if `overflow_newest_i` is present and `overflow_newest_i.remaining_q == 0`, clear `overflow_newest_i`
6. if `overflow_older_i` is present and `len(exact_reserve_cohorts_i) < MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`:
   - promote `overflow_older_i` into the exact reserve queue as the newest exact cohort
   - clear `overflow_older_i`
7. if `overflow_older_i` is absent and `overflow_newest_i` is present:
   - let `pending_q = overflow_newest_i.remaining_q`
   - let `pending_h = overflow_newest_i.horizon_slots`
   - clear `overflow_newest_i`
   - if `len(exact_reserve_cohorts_i) < MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`:
     - append one new exact cohort with:
       - `remaining_q = pending_q`
       - `anchor_q = pending_q`
       - `start_slot = current_slot`
       - `horizon_slots = pending_h`
       - `sched_release_q = 0`
   - else:
     - set `overflow_older_i` to one new scheduled cohort with:
       - `remaining_q = pending_q`
       - `anchor_q = pending_q`
       - `start_slot = current_slot`
       - `horizon_slots = pending_h`
       - `sched_release_q = 0`
8. if `R_i == 0`, require exact reserve queue is empty and both overflow segments are absent
9. require resulting `PNL_matured_pos_tot <= PNL_pos_tot`

Normative consequences:

- every exact cohort keeps its own exact cohort law
- `overflow_older_i`, if present, keeps its own preserved scheduled cohort law and is promoted back into exact cohort storage as soon as exact capacity frees up
- `overflow_newest_i`, if present, is economically pending; it does not auto-mature until activated into a scheduled cohort
- repeated touches cannot release a surviving scheduled reserve segment faster than its exact stored law
### 4.8 `attach_effective_position(i, new_eff_pos_q)`

This helper MUST convert a current effective quantity into a new position basis at the current side state.

If the account currently has a nonzero same-epoch basis and this helper is about to discard that basis (by writing either `0` or a different nonzero basis), then the engine MUST first account for any orphaned unresolved same-epoch quantity remainder:

- let `s = side(basis_pos_q_i)`
- if `epoch_snap_i == epoch_s`, compute `rem = (abs(basis_pos_q_i) * A_s) mod a_basis_i` in exact arithmetic
- if `rem != 0`, invoke `inc_phantom_dust_bound(s)`

If `new_eff_pos_q == 0`, it MUST:

- `set_position_basis_q(i, 0)`
- reset snapshots to canonical zero-position defaults

If `new_eff_pos_q != 0`, it MUST:

- require `abs(new_eff_pos_q) <= MAX_POSITION_ABS_Q`
- `set_position_basis_q(i, new_eff_pos_q)`
- `a_basis_i = A_side(new_eff_pos_q)`
- `k_snap_i = K_side(new_eff_pos_q)`
- `f_snap_i = F_side_num(new_eff_pos_q)`
- `epoch_snap_i = epoch_side(new_eff_pos_q)`

### 4.9 Phantom-dust helpers

- `inc_phantom_dust_bound(side)` increments `phantom_dust_bound_side_q` by exactly `1` q-unit using checked addition.
- `inc_phantom_dust_bound_by(side, amount_q)` increments `phantom_dust_bound_side_q` by exactly `amount_q` q-units using checked addition.

### 4.10 Exact math helpers (normative)

The engine MUST use the following exact helpers.

**Signed conservative floor division**

`floor_div_signed_conservative(n, d)`:

- require `d > 0`
- `q = trunc_toward_zero(n / d)`
- `r = n % d`
- if `n < 0` and `r != 0`, return `q - 1`
- else return `q`

**Positive checked ceiling division**

`ceil_div_positive_checked(n, d)`:

- require `d > 0`
- `q = n / d`
- `r = n % d`
- if `r != 0`, return checked(`q + 1`)
- else return `q`

**Exact multiply-divide floor for nonnegative inputs**

`mul_div_floor_u128(a, b, d)`:

- require `d > 0`
- compute the exact quotient `q = floor(a * b / d)`
- this MUST be exact even if the exact product `a * b` exceeds native `u128`
- require `q <= u128::MAX`
- return `q`

**Exact capped multiply-divide floor for nonnegative inputs**

`mul_div_floor_u128_capped(a, b, d, cap)`:

- require `d > 0`
- compute the exact quotient `q = floor(a * b / d)` in a transient wide type
- return `min(q, cap)` as `u128`
- this helper MUST be exact even if the exact product `a * b` or the uncapped quotient `q` exceeds native `u128`, provided `cap <= u128::MAX`

**Exact multiply-divide ceil for nonnegative inputs**

`mul_div_ceil_u128(a, b, d)`:

- require `d > 0`
- compute the exact quotient `q = ceil(a * b / d)`
- this MUST be exact even if the exact product `a * b` exceeds native `u128`
- require `q <= u128::MAX`
- return `q`

**Exact wide signed multiply-divide floor from K/F snapshots**

`wide_signed_mul_div_floor_from_kf_pair(abs_basis_u128, k_then_i128, k_now_i128, f_then_num_i128, f_now_num_i128, den_u128)`:

- require `den_u128 > 0`
- compute the exact signed wide differences:
  - `k_diff = k_now_i128 - k_then_i128`
  - `f_diff = f_now_num_i128 - f_then_num_i128`
- compute the exact signed wide numerator component:
  - `num = (k_diff * FUNDING_DEN) + f_diff`
- compute the exact wide magnitude `p = abs_basis_u128 * abs(num)`
- let `den_total = den_u128 * FUNDING_DEN`
- let `q = floor(p / den_total)`
- let `r = p mod den_total`
- if `num >= 0`, return `q` as positive `i128` (require representable)
- if `num < 0`, return `-q` if `r == 0`, else return `-(q + 1)` to preserve mathematical floor semantics (require representable)

**Checked fee-debt conversion**

`fee_debt_u128_checked(fee_credits)`:

- require `fee_credits != i128::MIN`
- if `fee_credits >= 0`, return `0`
- else return `(-fee_credits) as u128`

**Checked fee-credit headroom**

`fee_credit_headroom_u128_checked(fee_credits)`:

- require `fee_credits != i128::MIN`
- return `(i128::MAX as u128) - fee_debt_u128_checked(fee_credits)`

**Wide ADL quotient helper**

`wide_mul_div_ceil_u128_or_over_i128max(a, b, d)`:

- require `d > 0`
- compute the exact quotient `q = ceil(a * b / d)` in a transient wide type
- if `q > i128::MAX as u128`, return the tagged result `OverI128Magnitude`
- else return `Ok(q as u128)`

### 4.11 `charge_fee_to_insurance(i, fee_abs)`

Preconditions:

- `fee_abs <= MAX_PROTOCOL_FEE_ABS`

Effects:

1. `debt_headroom = fee_credit_headroom_u128_checked(fee_credits_i)`
2. `collectible = checked_add_u128(C_i, debt_headroom)`
3. `fee_applied = min(fee_abs, collectible)`
4. `fee_paid = min(fee_applied, C_i)`
5. if `fee_paid > 0`:
   - `set_capital(i, C_i - fee_paid)`
   - `I = checked_add_u128(I, fee_paid)`
6. `fee_shortfall = fee_applied - fee_paid`
7. if `fee_shortfall > 0`:
   - `fee_credits_i = checked_sub_i128(fee_credits_i, fee_shortfall as i128)`
8. any excess `fee_abs - fee_applied` is permanently uncollectible and MUST be dropped; it MUST NOT mutate `PNL_i`, `PNL_pos_tot`, `PNL_matured_pos_tot`, reserve cohorts, any `K_side`, `D`, or `Residual`

This helper MUST NOT mutate `PNL_i`, `PNL_pos_tot`, `PNL_matured_pos_tot`, reserve cohorts, or any `K_side`.

### 4.12 Insurance-loss helpers

`use_insurance_buffer(loss_abs)`:

1. precondition: `loss_abs > 0`
2. `available_I = I.saturating_sub(I_floor)`
3. `pay_I = min(loss_abs, available_I)`
4. `I = I - pay_I`
5. return `loss_abs - pay_I`

`record_uninsured_protocol_loss(loss_abs)`:

- precondition: `loss_abs > 0`
- no additional decrement to `V` or `I` occurs
- the uncovered loss remains represented as junior undercollateralization through `Residual` and the haircut mechanisms

`absorb_protocol_loss(loss_abs)`:

1. precondition: `loss_abs > 0`
2. `loss_rem = use_insurance_buffer(loss_abs)`
3. if `loss_rem > 0`, `record_uninsured_protocol_loss(loss_rem)`

---

## 5. Unified A/K side-index mechanics

### 5.1 Eager-equivalent event law

For one side of the book, a single eager global event on absolute fixed-point position `q_q >= 0` and realized PnL `p` has the form:

- `q_q' = α q_q`
- `p' = p + β * q_q / POS_SCALE`

where:

- `α ∈ [0, 1]` is the surviving-position fraction
- `β` is quote PnL per unit pre-event base position

The cumulative side indices compose as:

- `A_new = A_old * α`
- `K_new = K_old + A_old * β`

### 5.2 `effective_pos_q(i)`

For an account `i` with nonzero basis:

- let `s = side(basis_pos_q_i)`
- if `epoch_snap_i != epoch_s`, then `effective_pos_q(i) = 0` for current-market risk purposes until the account is touched and zeroed
- else `effective_abs_pos_q(i) = mul_div_floor_u128(abs(basis_pos_q_i) as u128, A_s, a_basis_i)`
- `effective_pos_q(i) = sign(basis_pos_q_i) * effective_abs_pos_q(i)`

If `basis_pos_q_i == 0`, define `effective_pos_q(i) = 0`.

### 5.2.1 Side-OI components of a signed effective position

For any signed fixed-point position `q` in q-units:

- `OI_long_component(q) = max(q, 0) as u128`
- `OI_short_component(q) = max(-q, 0) as u128`

### 5.2.2 Exact bilateral trade side-OI after-values

For a bilateral trade with pre-trade effective positions `old_eff_pos_q_a`, `old_eff_pos_q_b` and candidate post-trade effective positions `new_eff_pos_q_a`, `new_eff_pos_q_b`, define:

- `old_long_a = OI_long_component(old_eff_pos_q_a)`
- `old_short_a = OI_short_component(old_eff_pos_q_a)`
- `old_long_b = OI_long_component(old_eff_pos_q_b)`
- `old_short_b = OI_short_component(old_eff_pos_q_b)`
- `new_long_a = OI_long_component(new_eff_pos_q_a)`
- `new_short_a = OI_short_component(new_eff_pos_q_a)`
- `new_long_b = OI_long_component(new_eff_pos_q_b)`
- `new_short_b = OI_short_component(new_eff_pos_q_b)`

Then the exact candidate side-OI after-values are:

- `OI_long_after_trade = (((OI_eff_long - old_long_a) - old_long_b) + new_long_a) + new_long_b`
- `OI_short_after_trade = (((OI_eff_short - old_short_a) - old_short_b) + new_short_a) + new_short_b`

All arithmetic above MUST use checked helpers.

### 5.3 `settle_side_effects_live(i, H_lock)`

When touching account `i` on a live market:

1. if `basis_pos_q_i == 0`, return immediately
2. let `s = side(basis_pos_q_i)`
3. let `K_s = K_side(s)`
4. let `F_s_num = F_side_num(s)`
5. let `den = checked_mul_u128(a_basis_i, POS_SCALE)`
6. if `epoch_snap_i == epoch_s`:
   - `q_eff_new = mul_div_floor_u128(abs(basis_pos_q_i) as u128, A_s, a_basis_i)`
   - `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i) as u128, k_snap_i, K_s, f_snap_i, F_s_num, den)`
   - `set_pnl(i, checked_add_i128(PNL_i, pnl_delta), UseHLock(H_lock))`
   - if `q_eff_new == 0`:
     - `inc_phantom_dust_bound(s)`
     - `set_position_basis_q(i, 0)`
     - reset snapshots to canonical zero-position defaults
   - else:
     - leave `basis_pos_q_i` and `a_basis_i` unchanged
     - set `k_snap_i = K_s`
     - set `f_snap_i = F_s_num`
     - set `epoch_snap_i = epoch_s`
7. else:
   - require `mode_s == ResetPending`
   - require `epoch_snap_i + 1 == epoch_s`
   - let `K_epoch_start_s = K_epoch_start_side(s)`
   - let `F_epoch_start_s_num = F_epoch_start_side_num(s)`
   - `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i) as u128, k_snap_i, K_epoch_start_s, f_snap_i, F_epoch_start_s_num, den)`
   - `set_pnl(i, checked_add_i128(PNL_i, pnl_delta), UseHLock(H_lock))`
   - `set_position_basis_q(i, 0)`
   - decrement `stale_account_count_s` using checked subtraction
   - reset snapshots to canonical zero-position defaults

### 5.4 `settle_side_effects_resolved(i)`

When touching account `i` on a resolved market:

1. if `basis_pos_q_i == 0`, return immediately
2. let `s = side(basis_pos_q_i)`
3. require `mode_s == ResetPending`
4. require `epoch_snap_i + 1 == epoch_s`
5. let `K_epoch_start_s = K_epoch_start_side(s)`
6. let `F_epoch_start_s_num = F_epoch_start_side_num(s)`
7. let `den = checked_mul_u128(a_basis_i, POS_SCALE)`
8. `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i) as u128, k_snap_i, K_epoch_start_s, f_snap_i, F_epoch_start_s_num, den)`
9. `set_pnl(i, checked_add_i128(PNL_i, pnl_delta), ImmediateRelease)`
10. `set_position_basis_q(i, 0)`
11. decrement `stale_account_count_s` using checked subtraction
12. reset snapshots to canonical zero-position defaults

### 5.5 `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`

Before any live operation that depends on current market state, the engine MUST call `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`.

This helper MUST:

1. require `market_mode == Live`
2. require trusted `now_slot >= slot_last`
3. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
4. require `abs(funding_rate_e9_per_slot) <= MAX_ABS_FUNDING_E9_PER_SLOT`
5. let `dt = now_slot - slot_last`
6. snapshot `OI_long_0 = OI_eff_long` and `OI_short_0 = OI_eff_short`; let `fund_px_0 = fund_px_last`
7. mark-to-market once:
   - compute signed `ΔP = (oracle_price as i128) - (P_last as i128)`
   - if `OI_long_0 > 0`, `K_long = checked_add_i128(K_long, checked_mul_i128(A_long as i128, ΔP))`
   - if `OI_short_0 > 0`, `K_short = checked_sub_i128(K_short, checked_mul_i128(A_short as i128, ΔP))`
8. funding transfer, sub-stepped into the high-precision cumulative funding numerator indices:
   - if `funding_rate_e9_per_slot != 0` and `dt > 0` and `OI_long_0 > 0` and `OI_short_0 > 0`:
     - let `remaining = dt`
     - while `remaining > 0`:
       - `dt_sub = min(remaining, MAX_FUNDING_DT)`
       - `fund_num_1 = checked_mul_i128(fund_px_0 as i128, funding_rate_e9_per_slot as i128)`
       - `fund_num_step = checked_mul_i128(fund_num_1, dt_sub as i128)`
       - `F_long_num = checked_sub_i128(F_long_num, checked_mul_i128(A_long as i128, fund_num_step))`
       - `F_short_num = checked_add_i128(F_short_num, checked_mul_i128(A_short as i128, fund_num_step))`
       - `remaining = remaining - dt_sub`
9. update `slot_last = now_slot`
10. update `P_last = oracle_price`
11. update `fund_px_last = oracle_price`

Normative timing note:

- `fund_px_0 = fund_px_last` is the start-of-call funding-price sample for the entire elapsed interval.
- Funding exactness is represented through `F_side_num`; there is no per-call floor division inside `accrue_market_to`.
- New entrants do not inherit prior fractional funding because they snapshot `f_snap_i = F_side_num` on attachment.

### 5.6 `enqueue_adl(ctx, liq_side, q_close_q, D)`

Suppose a bankrupt liquidation from side `liq_side` leaves an uncovered deficit `D >= 0` after the liquidated account's principal and realized PnL have been exhausted. `q_close_q` is the fixed-point base quantity removed from the liquidated side and MAY be zero.

Let `opp = opposite(liq_side)`.

This helper MUST perform the following in order:

1. if `q_close_q > 0`, decrement `OI_eff_liq_side` by `q_close_q` using checked subtraction
2. if `D > 0`, set `D_rem = use_insurance_buffer(D)`; else define `D_rem = 0`
3. read `OI = OI_eff_opp`
4. if `OI == 0`:
   - if `D_rem > 0`, `record_uninsured_protocol_loss(D_rem)`
   - if `OI_eff_liq_side == 0`, set `ctx.pending_reset_long = true` and `ctx.pending_reset_short = true`
   - return
5. if `OI > 0` and `stored_pos_count_opp == 0`:
   - require `q_close_q <= OI`
   - let `OI_post = OI - q_close_q`
   - if `D_rem > 0`, `record_uninsured_protocol_loss(D_rem)`
   - set `OI_eff_opp = OI_post`
   - if `OI_post == 0`, set `ctx.pending_reset_long = true` and `ctx.pending_reset_short = true`
   - return
6. otherwise:
   - require `q_close_q <= OI`
   - `A_old = A_opp`
   - `OI_post = OI - q_close_q`
7. if `D_rem > 0`:
   - let `adl_scale = checked_mul_u128(A_old, POS_SCALE)`
   - compute `delta_K_abs_result = wide_mul_div_ceil_u128_or_over_i128max(D_rem, adl_scale, OI)`
   - if `delta_K_abs_result == OverI128Magnitude`, `record_uninsured_protocol_loss(D_rem)`
   - else:
     - `delta_K_abs = unwrap(delta_K_abs_result)`
     - `delta_K_exact = -(delta_K_abs as i128)`
     - if `checked_add_i128(K_opp, delta_K_exact)` fails, `record_uninsured_protocol_loss(D_rem)`
     - else `K_opp = K_opp + delta_K_exact`
8. if `OI_post == 0`:
   - set `OI_eff_opp = 0`
   - set `ctx.pending_reset_long = true` and `ctx.pending_reset_short = true`
   - return
9. compute `A_prod_exact = checked_mul_u128(A_old, OI_post)`
10. `A_candidate = floor(A_prod_exact / OI)`
11. if `A_candidate > 0`:
   - set `A_opp = A_candidate`
   - set `OI_eff_opp = OI_post`
   - if `OI_post < OI`:
     - `N_opp = stored_pos_count_opp as u128`
     - `global_a_dust_bound = checked_add_u128(N_opp, ceil_div_positive_checked(checked_add_u128(OI, N_opp), A_old))`
     - `inc_phantom_dust_bound_by(opp, global_a_dust_bound)`
   - if `A_opp < MIN_A_SIDE`, set `mode_opp = DrainOnly`
   - return
12. if `A_candidate == 0` while `OI_post > 0`:
   - set `OI_eff_opp = 0`
   - set `OI_eff_long = 0`
   - set `OI_eff_short = 0`
   - set both pending-reset flags true

### 5.7 End-of-instruction reset handling

The engine MUST provide both:

- `schedule_end_of_instruction_resets(ctx)`
- `finalize_end_of_instruction_resets(ctx)`

`schedule_end_of_instruction_resets(ctx)` MUST be called exactly once at the end of each top-level instruction that can touch accounts, mutate side state, liquidate, or resolved-close.

It MUST perform the following in order:

1. bilateral-empty dust clearance:
   - if `stored_pos_count_long == 0` and `stored_pos_count_short == 0`:
     - `clear_bound_q = checked_add_u128(phantom_dust_bound_long_q, phantom_dust_bound_short_q)`
     - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0) or (phantom_dust_bound_short_q > 0)`
     - if `has_residual_clear_work`:
       - require `OI_eff_long == OI_eff_short`
       - if `OI_eff_long <= clear_bound_q` and `OI_eff_short <= clear_bound_q`:
         - set `OI_eff_long = 0`
         - set `OI_eff_short = 0`
         - set both pending-reset flags true
       - else fail conservatively
2. unilateral-empty dust clearance, long empty:
   - else if `stored_pos_count_long == 0` and `stored_pos_count_short > 0`:
     - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0)`
     - if `has_residual_clear_work`:
       - require `OI_eff_long == OI_eff_short`
       - if `OI_eff_long <= phantom_dust_bound_long_q`:
         - set `OI_eff_long = 0`
         - set `OI_eff_short = 0`
         - set both pending-reset flags true
       - else fail conservatively
3. unilateral-empty dust clearance, short empty:
   - else if `stored_pos_count_short == 0` and `stored_pos_count_long > 0`:
     - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_short_q > 0)`
     - if `has_residual_clear_work`:
       - require `OI_eff_long == OI_eff_short`
       - if `OI_eff_short <= phantom_dust_bound_short_q`:
         - set `OI_eff_long = 0`
         - set `OI_eff_short = 0`
         - set both pending-reset flags true
       - else fail conservatively
4. DrainOnly zero-OI reset scheduling:
   - if `mode_long == DrainOnly and OI_eff_long == 0`, set `ctx.pending_reset_long = true`
   - if `mode_short == DrainOnly and OI_eff_short == 0`, set `ctx.pending_reset_short = true`

`finalize_end_of_instruction_resets(ctx)` MUST:

1. if `ctx.pending_reset_long` and `mode_long != ResetPending`, invoke `begin_full_drain_reset(long)`
2. if `ctx.pending_reset_short` and `mode_short != ResetPending`, invoke `begin_full_drain_reset(short)`
3. if `mode_long == ResetPending` and `OI_eff_long == 0` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, invoke `finalize_side_reset(long)`
4. if `mode_short == ResetPending` and `OI_eff_short == 0` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, invoke `finalize_side_reset(short)`

---

## 6. Warmup queue and matured-profit release

### 6.1 Parameters

This revision stores immutable warmup bounds only:

- `H_min = minimum wrapper-permitted horizon in slots`
- `H_max = maximum wrapper-permitted horizon in slots`

The core engine does **not** compute a dynamic horizon. The wrapper chooses one instruction-shared `H_lock` within `[H_min, H_max]` and passes it into live accrued instructions that may create new reserve.

If the instruction creates a new exact cohort, a new preserved overflow cohort, or a new pending overflow segment, that new segment stores the current instruction's `H_lock`. If reserve is instead routed into an already-existing pending overflow segment under §4.4.1 step 8, the pending segment keeps its previously stored horizon unchanged.

If `H_lock == 0`, positive PnL created during that instruction is immediately released rather than reserved.

### 6.2 Semantics of `R_i`

`R_i` is the reserved portion of positive `PNL_i` that has not yet matured through warmup.

- on live markets, `ReleasedPos_i = PosPNL_i - R_i`
- on resolved markets, `ReleasedPos_i = PosPNL_i`
- only `ReleasedPos_i` contributes to `PNL_matured_pos_tot`
- only `ReleasedPos_i` contributes to `h`
- all positive `PNL_i`, including reserved `R_i`, contributes to `g`
- `Eq_maint_raw_i` uses full local `PNL_i`
- `Eq_trade_raw_i` uses `PNL_eff_trade_i`
- `Eq_withdraw_raw_i` uses `PNL_eff_matured_i`

### 6.3 Reserve-cohort exactness

Each positive reserve increment is represented as its own exact reserve cohort unless exact same-slot same-horizon merging under §4.4.1 applies.

When exact storage is saturated, the engine may additionally use:

- `overflow_older_i`, a preserved scheduled overflow cohort whose accrued progress continues exactly under its stored law, and
- `overflow_newest_i`, a newest pending overflow segment that does not mature while pending and is activated later with a fresh scheduled law using its then-current `remaining_q` and the fixed conservative overflow horizon `H_overflow = H_max`.

For any **scheduled** reserve segment with `(anchor_q, start_slot, horizon_slots, sched_release_q)`:

- by time `t`, the segment's cumulative scheduled maturity is  
  `min(anchor_q, floor(anchor_q * (t - start_slot) / horizon_slots))`
- `advance_profit_warmup(i)` realizes only the incremental scheduled maturity since the prior touch, capped by the segment's surviving `remaining_q`
- true market losses consume reserve from newest segment to oldest segment, preserving older exact and preserved-overflow maturity progress

For the newest pending overflow segment:

- `sched_release_q` remains `0` while pending
- it does not auto-mature while pending
- its stored `horizon_slots` is always `H_overflow = H_max`
- when it is activated, the activated scheduled cohort starts at `current_slot` with `anchor_q = remaining_q`, `sched_release_q = 0`, and `horizon_slots = H_overflow = H_max`

This exact cohort law plus pending-overflow law is the authoritative anti-grief warmup design in this revision.

---

## 7. Loss settlement, profit conversion, fee-debt sweep, and touched-account finalization

### 7.1 `settle_losses_from_principal(i)`

If `PNL_i < 0`, the engine MUST immediately attempt to settle from principal:

1. require `PNL_i != i128::MIN`
2. `need = (-PNL_i) as u128`
3. `pay = min(need, C_i)`
4. apply:
   - `set_capital(i, C_i - pay)`
   - `set_pnl(i, checked_add_i128(PNL_i, pay as i128), NoPositiveIncreaseAllowed)`

Because `pay <= need`, the post-write `PNL_i_after` lies in `[PNL_i_before, 0]`. Therefore `max(PNL_i_after, 0) = 0`, no reserve can be added, and this helper MUST NOT create new positive reserve.

### 7.2 Open-position negative remainder

If after §7.1:

- `PNL_i < 0`, and
- `effective_pos_q(i) != 0`

then the account MUST remain liquidatable. It MUST NOT be silently zeroed or routed through flat-account loss absorption.

### 7.3 Flat-account negative remainder

If after §7.1:

- `PNL_i < 0`, and
- `effective_pos_q(i) == 0`

then the engine MUST:

1. call `absorb_protocol_loss((-PNL_i) as u128)`
2. `set_pnl(i, 0, NoPositiveIncreaseAllowed)`

This path is allowed only for truly flat accounts whose current-state side effects are already locally authoritative.

### 7.4 `max_safe_flat_conversion_released(i, x_cap, h_num, h_den)`

This helper returns the largest `x_safe <= x_cap` such that converting `x_safe` released profit on a **live flat** account cannot make the account's exact post-conversion raw maintenance equity negative.

Preconditions:

- `market_mode == Live`
- `basis_pos_q_i == 0`
- `x_cap <= ReleasedPos_i`
- if `x_cap > 0`, then `h_den > 0`

Let:

- `Eq_before_flat_i = Eq_maint_raw_i`
- `haircut_loss_num = h_den - h_num`

For candidate `x`, define:

- `y(x) = mul_div_floor_u128(x, h_num, h_den)`
- `Eq_maint_raw_post_flat_i(x) = (C_i as wide_signed) + (y(x) as wide_signed) + ((PNL_i as wide_signed) - (x as wide_signed)) - (FeeDebt_i as wide_signed)`

Implementation law:

1. if `x_cap == 0`, return `0`
2. if exact `Eq_before_flat_i <= 0`, return `0`
3. if `haircut_loss_num == 0`, return `x_cap`
4. require the exact positive value `Eq_before_flat_i` is representable as `u128`; under the numeric envelope of §1.4 this is guaranteed, but implementations MUST still fail conservatively if violated
5. let `E = Eq_before_flat_i as u128`
6. return `mul_div_floor_u128_capped(E, h_den, haircut_loss_num, x_cap)`

This formula is exact because for integer `x`:

- `x - floor(x * h_num / h_den) = ceil(x * (h_den - h_num) / h_den)`

So `Eq_maint_raw_post_flat_i(x) >= 0` holds iff `x <= floor(E * h_den / (h_den - h_num))`. The capped helper is therefore equivalent to `min(x_cap, floor(E * h_den / haircut_loss_num))` while avoiding liveness-blocking overflow when the uncapped mathematical quotient exceeds either `x_cap` or `u128::MAX`.

### 7.5 Profit conversion

Profit conversion removes matured released profit and converts only its haircutted backed portion into protected principal.

For live conversion with requested or capped amount `x > 0`, compute:

- `y = mul_div_floor_u128(x, h_num, h_den)`

Apply:

1. `consume_released_pnl(i, x)`
2. `set_capital(i, checked_add_u128(C_i, y))`

### 7.6 Fee-debt sweep

After any operation that increases `C_i`, or after a full current-state authoritative touch where existing capital is no longer senior-encumbered by attached trading losses, the engine MUST pay down fee debt as soon as that capital is available.

The sweep is:

1. `debt = fee_debt_u128_checked(fee_credits_i)`
2. `pay = min(debt, C_i)`
3. if `pay > 0`:
   - `set_capital(i, C_i - pay)`
   - `fee_credits_i = checked_add_i128(fee_credits_i, pay as i128)`
   - `I = checked_add_u128(I, pay)`

Normative consequence:

- fee sweep does not change `Eq_maint_raw_i`, `Eq_trade_raw_i`, or `Eq_withdraw_raw_i` because it decreases capital and fee debt one-for-one

### 7.7 `touch_account_live_local(i, ctx)`

This is the canonical local touch used inside live single-touch and live multi-touch instructions.

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. add `i` to `ctx.touched_accounts[]` if not already present
4. `advance_profit_warmup(i)`
5. `settle_side_effects_live(i, ctx.H_lock_shared)`
6. `settle_losses_from_principal(i)`
7. if `effective_pos_q(i) == 0` and `PNL_i < 0`, resolve uncovered flat loss via §7.3
8. MUST NOT auto-convert
9. MUST NOT fee-sweep

### 7.8 `finalize_touched_accounts_post_live(ctx)`

This helper is mandatory for all live instructions that use `touch_account_live_local`.

Preconditions:

- all live-OI-dependent work of the instruction is complete
- `ctx.touched_accounts[]` is the deduplicated set of accounts touched by `touch_account_live_local`

Procedure:

1. compute a single shared post-live conversion snapshot:
   - `Residual_snapshot = max(0, V - (C_tot + I))`
   - `PNL_matured_pos_tot_snapshot = PNL_matured_pos_tot`
   - if `PNL_matured_pos_tot_snapshot == 0`, define shared conversion as empty
   - else define:
     - `h_snapshot_num = min(Residual_snapshot, PNL_matured_pos_tot_snapshot)`
     - `h_snapshot_den = PNL_matured_pos_tot_snapshot`
2. iterate `ctx.touched_accounts[]` in deterministic ascending account-id order:
   - if `basis_pos_q_i == 0` and `ReleasedPos_i > 0` and `PNL_matured_pos_tot_snapshot > 0` and `h_snapshot_num == h_snapshot_den`:
     - `x_full = ReleasedPos_i`
     - `consume_released_pnl(i, x_full)`
     - `set_capital(i, checked_add_u128(C_i, x_full))`
   - call fee-debt sweep on the account's current state

Normative consequences:

- automatic live flat conversion is **whole-only** and occurs only when the shared snapshot is fully backed (`h_snapshot_num == h_snapshot_den`)
- under any haircut (`h_snapshot_num < h_snapshot_den`), touched flat accounts keep their released profit as a junior claim unless the account explicitly invokes `convert_released_pnl`
- all touched live accounts receive the same end-of-instruction fee sweep opportunity
- the same starting account state can no longer end in different persistent fee-debt states solely because it was touched via a single-touch instruction rather than a multi-touch instruction

### 7.9 `force_close_resolved_terminal(i)`

This helper performs the terminal close accounting for a resolved flat account **only after stale-account reconciliation is complete across both sides** and the shared resolved-payout snapshot has been captured.

Preconditions:

- `market_mode == Resolved`
- `basis_pos_q_i == 0`
- `stale_account_count_long == 0`
- `stale_account_count_short == 0`
- `resolved_payout_snapshot_ready == true`

Procedure:

1. call `settle_losses_from_principal(i)`
2. if `PNL_i < 0`, resolve uncovered flat loss via §7.3
3. if `max(PNL_i, 0) > 0`:
   - let `x = max(PNL_i, 0) as u128`
   - require `resolved_payout_h_den > 0`
   - `y = mul_div_floor_u128(x, resolved_payout_h_num, resolved_payout_h_den)`
   - `set_pnl(i, 0, NoPositiveIncreaseAllowed)`
   - `set_capital(i, checked_add_u128(C_i, y))`
4. fee-sweep the account
5. let `payout = C_i`
6. if `payout > 0`:
   - `set_capital(i, 0)`
   - `V = checked_sub_u128(V, payout)`
7. forgive any remaining negative `fee_credits_i` by setting `fee_credits_i = 0`
8. require `PNL_i == 0`
9. require `R_i == 0`
10. require exact reserve queue is empty and both overflow cohorts are absent
11. require `basis_pos_q_i == 0`
12. reset local fields to canonical zero and mark slot missing / reusable
13. decrement the materialized-account count

Normative consequences:

- terminal resolved close is allowed to forgive residual uncollectible fee debt after all collectible capital has been swept because the account is being permanently removed and no later reclaim abuse is possible
- positive resolved claims cannot be paid out before all stale final settlements are realized across both sides
- once captured, the shared resolved payout snapshot is reused for every later terminal close so caller order cannot improve the payout ratio

---

## 8. Fees

This revision has no engine-native recurring maintenance fee. The engine core only defines native trading fees, native liquidation fees, and the canonical helper for optional wrapper-owned account fees.

### 8.1 Trading fees

Trading fees are explicit transfers to insurance and MUST NOT be socialized through `h`, through `g`, or through `D`.

Define:

- `fee = mul_div_ceil_u128(trade_notional, trading_fee_bps, 10_000)`

with `0 <= trading_fee_bps <= MAX_TRADING_FEE_BPS`.

Rules:

- if `trading_fee_bps == 0` or `trade_notional == 0`, then `fee = 0`
- if `trading_fee_bps > 0` and `trade_notional > 0`, then `fee >= 1`

The fee MUST be charged using `charge_fee_to_insurance(i, fee)`.

### 8.2 Liquidation fees

The protocol MUST define:

- `liquidation_fee_bps` with `0 <= liquidation_fee_bps <= MAX_LIQUIDATION_FEE_BPS`
- `liquidation_fee_cap` with `0 <= liquidation_fee_cap <= MAX_PROTOCOL_FEE_ABS`
- `min_liquidation_abs` with `0 <= min_liquidation_abs <= liquidation_fee_cap`

For a liquidation that closes `q_close_q` at `oracle_price`, define:

- if `q_close_q == 0`, then `liq_fee = 0`
- else:
  - `closed_notional = mul_div_floor_u128(q_close_q, oracle_price, POS_SCALE)`
  - `liq_fee_raw = mul_div_ceil_u128(closed_notional, liquidation_fee_bps, 10_000)`
  - `liq_fee = min(max(liq_fee_raw, min_liquidation_abs), liquidation_fee_cap)`

### 8.3 Optional wrapper-owned account fees

A wrapper MAY impose arbitrary additional account fees by routing an amount `fee_abs` through `charge_fee_to_insurance(i, fee_abs)`, provided `fee_abs <= MAX_PROTOCOL_FEE_ABS`.

The engine core does not define the timing, recurrence, or formula for such wrapper-owned fees.

### 8.4 Fee debt as margin liability

`FeeDebt_i = fee_debt_u128_checked(fee_credits_i)`:

- MUST reduce `Eq_maint_raw_i`, `Eq_trade_raw_i`, `Eq_trade_open_raw_i`, and `Eq_withdraw_raw_i`
- MUST be swept whenever principal becomes available and is no longer senior-encumbered by already-realized trading losses on the same local state
- MUST NOT directly change `Residual`, `PNL_pos_tot`, or `PNL_matured_pos_tot`
- includes unpaid collectible native trading fees, native liquidation fees, and any wrapper-owned account fees routed through the canonical helper
- any explicit fee amount beyond collectible capacity is dropped rather than written into `PNL_i` or `D`

---

## 9. Margin checks and liquidation

### 9.1 Margin requirements

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
- risk-increasing trade approval healthy if exact `Eq_trade_open_raw_i >= IM_req_post_i`, where `IM_req_post_i` is the post-trade initial-margin requirement explicitly recomputed in `execute_trade`

### 9.2 Risk-increasing and strictly risk-reducing trades

A trade for account `i` is **risk-increasing** when either:

1. `abs(new_eff_pos_q_i) > abs(old_eff_pos_q_i)`, or
2. the position sign flips across zero, or
3. `old_eff_pos_q_i == 0` and `new_eff_pos_q_i != 0`

A trade is **strictly risk-reducing** when:

- `old_eff_pos_q_i != 0`
- `new_eff_pos_q_i != 0`
- `sign(new_eff_pos_q_i) == sign(old_eff_pos_q_i)`
- `abs(new_eff_pos_q_i) < abs(old_eff_pos_q_i)`

### 9.3 Liquidation eligibility

An account is liquidatable when after a full current-state authoritative live touch:

- `effective_pos_q(i) != 0`, and
- `Eq_net_i <= MM_req_i as i128`

### 9.4 Partial liquidation

A liquidation MAY be partial only if it closes a strictly positive quantity smaller than the full remaining effective position:

- `0 < q_close_q < abs(old_eff_pos_q_i)`

A successful partial liquidation MUST:

1. use the current touched state
2. let `old_eff_pos_q_i = effective_pos_q(i)` and require `old_eff_pos_q_i != 0`
3. determine `liq_side = side(old_eff_pos_q_i)`
4. define `new_eff_abs_q = checked_sub_u128(abs(old_eff_pos_q_i), q_close_q)`
5. require `new_eff_abs_q > 0`
6. define `new_eff_pos_q_i = sign(old_eff_pos_q_i) * (new_eff_abs_q as i128)`
7. close `q_close_q` synthetically at `oracle_price` with zero execution-price slippage
8. apply the resulting position using `attach_effective_position(i, new_eff_pos_q_i)`
9. settle realized losses from principal via §7.1
10. compute `liq_fee` per §8.2 on the quantity actually closed
11. charge that fee using `charge_fee_to_insurance(i, liq_fee)`
12. invoke `enqueue_adl(ctx, liq_side, q_close_q, 0)` to decrease global OI and socialize quantity reduction
13. if either pending-reset flag becomes true in `ctx`, stop any further live-OI-dependent checks or mutations; only the remaining local post-step validation of step 14 may still run before end-of-instruction reset handling
14. require the resulting nonzero position to be maintenance healthy on the current post-step-12 state

### 9.5 Full-close / bankruptcy liquidation

The engine MUST be able to perform a deterministic full-close liquidation on an already-touched liquidatable account.

It MUST:

1. use the current touched state
2. let `old_eff_pos_q_i = effective_pos_q(i)` and require `old_eff_pos_q_i != 0`
3. set `q_close_q = abs(old_eff_pos_q_i)`
4. let `liq_side = side(old_eff_pos_q_i)`
5. execute exactly at `oracle_price` with zero execution-price slippage
6. `attach_effective_position(i, 0)`
7. `OI_eff_liq_side` MUST NOT be decremented anywhere except through `enqueue_adl`
8. `settle_losses_from_principal(i)`
9. compute `liq_fee` per §8.2 and charge it via `charge_fee_to_insurance(i, liq_fee)`
10. determine the uncovered bankruptcy deficit `D`:
    - if `PNL_i < 0`, let `D = (-PNL_i) as u128`
    - else `D = 0`
11. if `q_close_q > 0` or `D > 0`, invoke `enqueue_adl(ctx, liq_side, q_close_q, D)`
12. if `D > 0`, `set_pnl(i, 0, NoPositiveIncreaseAllowed)`

### 9.6 Side-mode gating

Before any top-level instruction rejects an OI-increasing operation because a side is in `ResetPending`, it MUST first invoke `maybe_finalize_ready_reset_sides_before_oi_increase()`.

Any operation that would increase net side OI on a side whose mode is `DrainOnly` or `ResetPending` MUST be rejected.

For `execute_trade`, this prospective check MUST use the exact bilateral candidate after-values of §5.2.2 on both sides.

---

## 10. External operations

### 10.0 Standard live instruction lifecycle

The `H_lock` and `funding_rate_e9_per_slot` inputs shown in live entrypoints below are **wrapper-owned logical inputs**, not public caller-owned fields. Public or permissionless wrappers MUST derive them internally rather than accept arbitrary user-chosen values.

Unless explicitly noted otherwise, a **live** external state-mutating operation that depends on current market state executes inside the same standard lifecycle:

1. validate trusted monotonic slot inputs, validated oracle input, wrapper-supplied high-precision funding-rate bound, and wrapper-supplied `H_lock` bound
2. initialize a fresh instruction context `ctx` with `H_lock_shared = H_lock`
3. call `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` exactly once
4. set `current_slot = now_slot`
5. perform the endpoint's exact current-state inner execution
6. call `finalize_touched_accounts_post_live(ctx)` exactly once
7. call `schedule_end_of_instruction_resets(ctx)` exactly once
8. call `finalize_end_of_instruction_resets(ctx)` exactly once
9. if the instruction can mutate live side exposure, assert `OI_eff_long == OI_eff_short` at the end

### 10.1 `settle_account(i, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock)`

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx` with `H_lock_shared = H_lock`
4. `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`
5. set `current_slot = now_slot`
6. `touch_account_live_local(i, ctx)`
7. `finalize_touched_accounts_post_live(ctx)`
8. `schedule_end_of_instruction_resets(ctx)`
9. `finalize_end_of_instruction_resets(ctx)`

### 10.2 `deposit(i, amount, now_slot)`

`deposit` is a pure capital-transfer instruction. It MUST NOT call `accrue_market_to`, MUST NOT mutate side state, MUST NOT auto-touch unrelated accounts, and MUST NOT mutate reserve cohorts.

Procedure:

1. require `market_mode == Live`
2. require trusted `now_slot >= current_slot`
3. if account `i` is missing:
   - require `amount >= MIN_INITIAL_DEPOSIT`
   - `materialize_account(i, now_slot)`
4. set `current_slot = now_slot`
5. require `checked_add_u128(V, amount) <= MAX_VAULT_TVL`
6. set `V = V + amount`
7. `set_capital(i, checked_add_u128(C_i, amount))`
8. `settle_losses_from_principal(i)`
9. MUST NOT invoke §7.3 or otherwise decrement `I`
10. if `basis_pos_q_i == 0` and `PNL_i >= 0`, sweep fee debt via §7.6

### 10.2.1 `deposit_fee_credits(i, amount, now_slot)`

`deposit_fee_credits` is a direct external repayment of account-local fee debt. It is not a capital deposit and does not pass through `C_i`.

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. require trusted `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. let `debt = fee_debt_u128_checked(fee_credits_i)`
6. let `pay = min(amount, debt)`
7. if `pay == 0`, return
8. require `checked_add_u128(V, pay) <= MAX_VAULT_TVL`
9. set `V = V + pay`
10. set `I = checked_add_u128(I, pay)`
11. set `fee_credits_i = checked_add_i128(fee_credits_i, pay as i128)`
12. require `fee_credits_i <= 0`

### 10.2.2 `top_up_insurance_fund(amount, now_slot)`

Procedure:

1. require `market_mode == Live`
2. require trusted `now_slot >= current_slot`
3. set `current_slot = now_slot`
4. require `checked_add_u128(V, amount) <= MAX_VAULT_TVL`
5. set `V = V + amount`
6. set `I = checked_add_u128(I, amount)`

This instruction MUST NOT call `accrue_market_to`, MUST NOT mutate any account-local state, and MUST NOT mutate side state.

### 10.2.3 `charge_account_fee(i, fee_abs, now_slot)`

This is the optional wrapper-facing pure fee instruction.

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. require trusted `now_slot >= current_slot`
4. require `fee_abs <= MAX_PROTOCOL_FEE_ABS`
5. set `current_slot = now_slot`
6. `charge_fee_to_insurance(i, fee_abs)`

This instruction MUST NOT call `accrue_market_to`, MUST NOT mutate side state, and MUST NOT mutate `PNL_i` or reserve storage.

### 10.2.4 `settle_flat_negative_pnl(i, now_slot)`

This is a permissionless **live-only** cleanup path for an already-flat authoritative account carrying negative `PNL_i`. It exists to unblock reclaim and materialized-slot reuse without requiring market accrual.

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. require trusted `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. require `basis_pos_q_i == 0`
6. require `R_i == 0` and exact reserve queue is empty and both overflow cohorts are absent
7. if `PNL_i >= 0`, return
8. call `settle_losses_from_principal(i)`
9. if `PNL_i < 0`:
   - `absorb_protocol_loss((-PNL_i) as u128)`
   - `set_pnl(i, 0, NoPositiveIncreaseAllowed)`
10. require `PNL_i == 0`

This instruction MUST NOT call `accrue_market_to` and MUST NOT mutate side state.

### 10.3 `withdraw(i, amount, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock)`

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx` with `H_lock_shared = H_lock`
4. `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`
5. set `current_slot = now_slot`
6. `touch_account_live_local(i, ctx)`
7. `finalize_touched_accounts_post_live(ctx)`
8. require `amount <= C_i`
9. require the post-withdraw capital `C_i - amount` is either `0` or `>= MIN_INITIAL_DEPOSIT`
10. if `effective_pos_q(i) != 0`, require post-withdraw withdrawal health on the hypothetical post-withdraw state where:
    - `C_i' = C_i - amount`
    - `V' = V - amount`
    - `C_tot' = C_tot - amount`
    - all other touched-state quantities are unchanged
    - equivalently, because both `V` and `C_tot` decrease by the same `amount`, `Residual` and the current live haircut `h` are unchanged by the hypothetical
    - exact `Eq_withdraw_raw_i` is recomputed from that hypothetical state and compared against `IM_req_i`
11. apply:
    - `set_capital(i, C_i - amount)`
    - `V = checked_sub_u128(V, amount)`
12. `schedule_end_of_instruction_resets(ctx)`
13. `finalize_end_of_instruction_resets(ctx)`

### 10.3.1 `convert_released_pnl(i, x_req, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock)`

Explicit voluntary conversion of matured released positive PnL for any live account, flat or open.

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx` with `H_lock_shared = H_lock`
4. `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`
5. set `current_slot = now_slot`
6. `touch_account_live_local(i, ctx)`
7. require `0 < x_req <= ReleasedPos_i`
8. compute current `h`
9. if `basis_pos_q_i == 0`:
   - `x_safe = max_safe_flat_conversion_released(i, x_req, h_num, h_den)`
   - require `x_safe == x_req`
10. `consume_released_pnl(i, x_req)`
11. `set_capital(i, checked_add_u128(C_i, mul_div_floor_u128(x_req, h_num, h_den)))`
12. sweep fee debt
13. if `effective_pos_q(i) != 0`, require the current post-step-12 state is maintenance healthy
14. `finalize_touched_accounts_post_live(ctx)`
15. `schedule_end_of_instruction_resets(ctx)`
16. `finalize_end_of_instruction_resets(ctx)`

Normative consequences:

- this is the only engine-defined path that allows a user to **voluntarily accept the current haircut** on released live profit
- on a flat account, the instruction MUST reject rather than over-convert if the requested lossy conversion would make exact flat raw maintenance equity negative
- on an open account, the post-conversion state must still satisfy current maintenance health

### 10.4 `execute_trade(a, b, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock, size_q, exec_price)`

`size_q > 0` means account `a` buys base from account `b`.

Procedure:

1. require `market_mode == Live`
2. require both accounts are materialized
3. require `a != b`
4. require trusted `now_slot >= current_slot`
5. require trusted `now_slot >= slot_last`
6. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
7. require validated `0 < exec_price <= MAX_ORACLE_PRICE`
8. require `0 < size_q <= MAX_TRADE_SIZE_Q`
9. compute `trade_notional = mul_div_floor_u128(size_q, exec_price, POS_SCALE)`
10. require `trade_notional <= MAX_ACCOUNT_NOTIONAL`
11. initialize `ctx` with `H_lock_shared = H_lock`
12. `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`
13. set `current_slot = now_slot`
14. `touch_account_live_local(a, ctx)`
15. `touch_account_live_local(b, ctx)`
16. let `old_eff_pos_q_a = effective_pos_q(a)` and `old_eff_pos_q_b = effective_pos_q(b)`
17. let `MM_req_pre_a`, `MM_req_pre_b` be maintenance requirements on the post-touch pre-trade state
18. let `Eq_maint_raw_pre_a = Eq_maint_raw_a` and `Eq_maint_raw_pre_b = Eq_maint_raw_b`
19. invoke `maybe_finalize_ready_reset_sides_before_oi_increase()`
20. define:
    - `new_eff_pos_q_a = checked_add_i128(old_eff_pos_q_a, size_q as i128)`
    - `new_eff_pos_q_b = checked_sub_i128(old_eff_pos_q_b, size_q as i128)`
21. require `abs(new_eff_pos_q_a) <= MAX_POSITION_ABS_Q` and `abs(new_eff_pos_q_b) <= MAX_POSITION_ABS_Q`
22. compute `OI_long_after_trade` and `OI_short_after_trade` exactly via §5.2.2 using `old_eff_pos_q_a`, `old_eff_pos_q_b`, `new_eff_pos_q_a`, and `new_eff_pos_q_b`
23. require `OI_long_after_trade <= MAX_OI_SIDE_Q` and `OI_short_after_trade <= MAX_OI_SIDE_Q`
24. reject if `mode_long ∈ {DrainOnly, ResetPending}` and `OI_long_after_trade > OI_eff_long`
25. reject if `mode_short ∈ {DrainOnly, ResetPending}` and `OI_short_after_trade > OI_eff_short`
26. apply immediate execution-slippage alignment PnL before fees:
    - `trade_pnl_num = checked_mul_i128(size_q as i128, (oracle_price as i128) - (exec_price as i128))`
    - `trade_pnl_a = floor_div_signed_conservative(trade_pnl_num, POS_SCALE)`
    - `trade_pnl_b = -trade_pnl_a`
    - `set_pnl(a, checked_add_i128(PNL_a, trade_pnl_a), UseHLock(H_lock))`
    - `set_pnl(b, checked_add_i128(PNL_b, trade_pnl_b), UseHLock(H_lock))`
27. apply the resulting effective positions using `attach_effective_position(a, new_eff_pos_q_a)` and `attach_effective_position(b, new_eff_pos_q_b)`
28. update side OI atomically by writing the exact candidate after-values from step 22:
    - set `OI_eff_long = OI_long_after_trade`
    - set `OI_eff_short = OI_short_after_trade`
29. settle post-trade losses from principal for both accounts via §7.1
30. if `new_eff_pos_q_a == 0`, require `PNL_a >= 0` after step 29
31. if `new_eff_pos_q_b == 0`, require `PNL_b >= 0` after step 29
32. compute `fee = mul_div_ceil_u128(trade_notional, trading_fee_bps, 10_000)`
33. charge explicit trading fees using `charge_fee_to_insurance(a, fee)` and `charge_fee_to_insurance(b, fee)`
34. compute post-trade quantities for each account on the current post-step-33 state:
    - `Notional_post_i`
    - `IM_req_post_i`
    - `MM_req_post_i`
    - `Eq_trade_open_raw_i` using the exact counterfactual definition of §3.5 with `candidate_trade_pnl_i = trade_pnl_i`
35. enforce post-trade approval for each account independently:
    - if resulting effective position is zero:
      - require exact `Eq_maint_raw_i >= 0`
    - else if the trade is risk-increasing for that account:
      - require exact `Eq_trade_open_raw_i >= IM_req_post_i`
    - else if exact `Eq_net_i > MM_req_post_i`, allow
    - else if the trade is strictly risk-reducing for that account, allow only if both hold:
      - `((Eq_maint_raw_i + fee) - MM_req_post_i) > (Eq_maint_raw_pre_i - MM_req_pre_i)`
      - `min(Eq_maint_raw_i + fee, 0) >= min(Eq_maint_raw_pre_i, 0)`
    - else reject
36. `finalize_touched_accounts_post_live(ctx)`
37. `schedule_end_of_instruction_resets(ctx)`
38. `finalize_end_of_instruction_resets(ctx)`
39. assert `OI_eff_long == OI_eff_short`

### 10.5 `liquidate(i, oracle_price, now_slot, funding_rate_e9_per_slot, H_lock, policy)`

`policy` MUST be one of:

- `FullClose`
- `ExactPartial(q_close_q)` where `0 < q_close_q < abs(old_eff_pos_q_i)` on the already-touched current state

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx` with `H_lock_shared = H_lock`
4. `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`
5. set `current_slot = now_slot`
6. `touch_account_live_local(i, ctx)`
7. require liquidation eligibility from §9.3
8. if `policy == ExactPartial(q_close_q)`, attempt that exact partial-liquidation subroutine on the already-touched current state per §9.4
9. else execute the full-close liquidation subroutine on the already-touched current state per §9.5
10. `finalize_touched_accounts_post_live(ctx)`
11. `schedule_end_of_instruction_resets(ctx)`
12. `finalize_end_of_instruction_resets(ctx)`
13. assert `OI_eff_long == OI_eff_short`

### 10.6 `keeper_crank(now_slot, oracle_price, funding_rate_e9_per_slot, H_lock, ordered_candidates[], max_revalidations)`

`keeper_crank` is the minimal on-chain permissionless shortlist processor. `ordered_candidates[]` is keeper-supplied and untrusted.

Procedure:

1. require `market_mode == Live`
2. initialize `ctx` with `H_lock_shared = H_lock`
3. require trusted `now_slot >= current_slot`
4. require trusted `now_slot >= slot_last`
5. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
6. `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` exactly once at the start
7. set `current_slot = now_slot`
8. let `attempts = 0`
9. for each candidate in keeper-supplied order:
   - if `attempts == max_revalidations`, break
   - if `ctx.pending_reset_long` or `ctx.pending_reset_short`, break
   - if candidate account is missing, continue
   - increment `attempts` by exactly `1`
   - `touch_account_live_local(candidate, ctx)`
   - if the account is liquidatable after that exact current-state touch and a current-state-valid liquidation-policy hint is present, execute liquidation on the already-touched state using the already-touched local liquidation subroutine
   - if liquidation or the exact touch schedules a pending reset, break
10. `finalize_touched_accounts_post_live(ctx)`
11. `schedule_end_of_instruction_resets(ctx)`
12. `finalize_end_of_instruction_resets(ctx)`
13. assert `OI_eff_long == OI_eff_short`

### 10.7 `resolve_market(resolved_price, now_slot)`

This instruction transitions a live market to terminal resolved mode. It is a **privileged deployment-owned transition**, not part of the permissionless user surface. Access control is outside the core arithmetic and MUST be enforced by the enclosing runtime or settlement wrapper.

Procedure:

1. require `market_mode == Live`
2. require trusted `now_slot >= current_slot`
3. require trusted `now_slot >= slot_last`
4. require validated `0 < resolved_price <= MAX_ORACLE_PRICE`
5. require exact wide-arithmetic settlement band check:
   - `abs((resolved_price as wide_signed) - (P_last as wide_signed)) * 10_000 <= (resolve_price_deviation_bps as wide_signed) * (P_last as wide_signed)`
6. `accrue_market_to(now_slot, resolved_price, 0)`
7. set `current_slot = now_slot`
8. set `market_mode = Resolved`
9. set `resolved_price = resolved_price`
10. set `resolved_slot = now_slot`
11. set `resolved_payout_snapshot_ready = false`
12. set `resolved_payout_h_num = 0`
13. set `resolved_payout_h_den = 0`
14. set `PNL_matured_pos_tot = PNL_pos_tot`
15. set `OI_eff_long = 0`
16. set `OI_eff_short = 0`
17. if `mode_long != ResetPending`, invoke `begin_full_drain_reset(long)`
18. if `mode_short != ResetPending`, invoke `begin_full_drain_reset(short)`
19. if `mode_long == ResetPending` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, invoke `finalize_side_reset(long)`
20. if `mode_short == ResetPending` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, invoke `finalize_side_reset(short)`
21. require `OI_eff_long == 0` and `OI_eff_short == 0`

Normative consequences:

- once resolved, all remaining positive PnL is globally treated as matured
- local reserve storage becomes inert and must be cleared per account on resolved touch via `prepare_account_for_resolved_touch(i)`
- `resolve_market` itself applies zero funding over the settlement transition; a deployment that wants a final nonzero live funding accrual SHOULD perform a final live accrued instruction before calling `resolve_market`
- a deployment that expects the immutable settlement band around `P_last` to reflect the freshest live mark SHOULD refresh live state immediately before invoking `resolve_market`
- after `market_mode == Resolved`, only resolved-close progress operations remain on the ordinary account surface

### 10.8 `force_close_resolved(i, now_slot)`

This instruction performs resolved-market local reconciliation and, once the stale-account phase is complete, terminal payout and close.

Procedure:

1. require `market_mode == Resolved`
2. require account `i` is materialized
3. require trusted `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. `prepare_account_for_resolved_touch(i)`
6. `settle_side_effects_resolved(i)`
7. if `PNL_i < 0`, call `settle_losses_from_principal(i)`
8. if `PNL_i < 0` and `basis_pos_q_i == 0`, resolve uncovered flat loss via §7.3
9. if `mode_long == ResetPending` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, invoke `finalize_side_reset(long)`
10. if `mode_short == ResetPending` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, invoke `finalize_side_reset(short)`
11. if `stale_account_count_long > 0` or `stale_account_count_short > 0`, return
12. if `resolved_payout_snapshot_ready == false`:
    - `Residual_snapshot = max(0, V - (C_tot + I))`
    - if `PNL_matured_pos_tot == 0`:
      - set `resolved_payout_h_num = 0`
      - set `resolved_payout_h_den = 0`
    - else:
      - set `resolved_payout_h_num = min(Residual_snapshot, PNL_matured_pos_tot)`
      - set `resolved_payout_h_den = PNL_matured_pos_tot`
    - set `resolved_payout_snapshot_ready = true`
13. `force_close_resolved_terminal(i)`

Normative consequence:

- `force_close_resolved` is intentionally a multi-stage permissionless progress path: one or more calls may be needed to reconcile stale resolved accounts before a later call reaches terminal payout after the shared resolved snapshot is captured.

### 10.9 `reclaim_empty_account(i, now_slot)`

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. require trusted `now_slot >= current_slot`
4. require pre-reclaim flat-clean preconditions of §2.6
5. set `current_slot = now_slot`
6. require final reclaim eligibility of §2.6
7. execute the reclamation effects of §2.6

---

## 11. Permissionless off-chain shortlist keeper mode

This section is the sole normative specification for the optimized keeper path. Candidate discovery, ranking, deduplication, and sequential simulation MAY be performed entirely off chain. On-chain safety derives only from exact current-state revalidation immediately before any liquidation write.

### 11.1 Core rules

1. The engine does **not** require any on-chain phase-1 search, barrier classifier, or no-false-negative scan proof.
2. `ordered_candidates[]` in §10.6 is keeper-supplied and untrusted. It MAY be stale, incomplete, duplicated, adversarially ordered, or produced by approximate heuristics.
3. Optional liquidation-policy hints are untrusted. They MUST be ignored unless they encode one of the §10.5 policies and pass the same exact current-state validity checks as the normal `liquidate` entrypoint.
4. The protocol MUST NOT require that a keeper discover all currently liquidatable accounts before it may process a useful subset.
5. Because `settle_account`, `liquidate`, `reclaim_empty_account`, and `force_close_resolved` are permissionless, reset progress and dead-account recycling MUST remain possible without any mandatory on-chain scan order.

### 11.2 Exact current-state revalidation attempts

`max_revalidations` counts normal exact current-state revalidation attempts on materialized accounts. A missing-account skip does not count. A fatal conservative failure or invariant violation is a top-level instruction failure and reverts atomically under §0.

Inside `keeper_crank`, the per-candidate local exact-touch helper MUST be economically equivalent to `touch_account_live_local(i, ctx)` on a state that has already been globally accrued once to `(now_slot, oracle_price, funding_rate_e9_per_slot)` at the start of the instruction.

### 11.3 On-chain ordering constraints

Inside `keeper_crank`, the only mandatory on-chain ordering constraints are:

1. the single initial `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` and trusted `current_slot = now_slot` anchor happen before per-candidate exact revalidation
2. materialized candidates are processed in keeper-supplied order
3. once either pending-reset flag becomes true, the instruction stops further candidate processing and proceeds directly to end-of-instruction reset handling

---

## 12. Required test properties (minimum)

An implementation MUST include tests that cover at least:

1. **Conservation:** `V >= C_tot + I` always.
2. **Fresh-profit reservation:** a positive `set_pnl` increase raises `R_i` by the same positive delta and does not immediately increase `PNL_matured_pos_tot`.
3. **Withdrawal-lane oracle safety:** fresh unwarmed manipulated PnL cannot dilute `h`, cannot satisfy withdrawal checks, and cannot be principal-converted before warmup.
4. **Trade-lane boundedness:** aggregate positive PnL admitted through `g` satisfies `Σ PNL_eff_trade_i <= Residual`.
5. **Exact trade-open counterfactual:** `Eq_trade_open_raw_i` equals the exact recomputation with the candidate trade's own positive slippage removed from both local signed PnL and the global positive-PnL aggregate.
6. **Same-trade bootstrap blocked:** a trade that would pass only because of the candidate trade's own positive execution-slippage PnL is rejected.
7. **Healthy-state full trade reuse:** when `Residual >= PNL_pos_tot`, `g = 1`, and fresh positive PnL counts fully for `Eq_trade_raw_i` and, outside candidate neutralization, for `Eq_trade_open_raw_i`.
8. **Maintenance unchanged by fee sweep:** fee-debt sweep leaves `Eq_maint_raw_i` unchanged.
9. **Maintenance unchanged by warmup release:** pure warmup release on unchanged `PNL_i` does not reduce `Eq_maint_raw_i`.
10. **Trade equity unchanged by warmup release:** pure warmup release on unchanged `PNL_i` does not increase `Eq_trade_raw_i`.
11. **Withdrawal equity increases with warmup release:** pure warmup release on unchanged `PNL_i` can increase `Eq_withdraw_raw_i`.
12. **Incremental reserve no-restart:** adding a new positive reserve cohort does not change any older cohort's `start_slot`, `horizon_slots`, `anchor_q`, or already accrued maturity progress.
13. **Dust-grief resistance:** repeated dust-sized positive reserve additions do not materially delay an older exact cohort's already accrued maturity progress. Once exact capacity is exhausted, any conservative bounded-storage delay is confined to overflow segments that both use `H_overflow = H_max`, while `overflow_older_i` and all exact cohorts remain unchanged.
14. **Exact cohort timing:** a scheduled reserve cohort with horizon `H_lock` does not release materially faster than `floor(anchor * elapsed / H_lock)` solely because of small-bucket rounding.
15. **Bounded reserve storage:** the exact reserve queue length never exceeds `MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT`, at most one `overflow_older_i` and at most one `overflow_newest_i` exist, and total reserve segments never exceed `MAX_RESERVE_SEGMENTS_PER_ACCOUNT`.
16. **Pending overflow locality:** when exact reserve capacity is exhausted, newer reserve beyond the preserved overflow cohort is routed only into `overflow_newest_i`, which remains pending until activated; exact cohorts and `overflow_older_i` remain unchanged.
17. **Reserve-loss ordering:** true market losses consume `overflow_newest_i` first if present, then `overflow_older_i` if present, then exact cohorts from newest exact to oldest exact, preserving older exact maturity progress.
18. **Single-touch / multi-touch equivalence:** the same starting account state touched through `settle_account` and through a multi-touch instruction ends with the same persistent fee-debt and automatic-conversion outcome when the post-live touched state is identical.
19. **Whole-only automatic flat conversion:** open-position touch does not auto-convert released profit into principal, and flat touched live accounts auto-convert only when the shared snapshot is whole (`h_snapshot_num == h_snapshot_den`).
20. **No permissionless lossy flat conversion:** under a haircutted snapshot (`h_snapshot_num < h_snapshot_den`), `finalize_touched_accounts_post_live` leaves released flat profit as a junior claim rather than crystallizing the haircut.
21. **Explicit conversion remains matured-only:** `convert_released_pnl` consumes only live `ReleasedPos_i` and leaves reserve cohorts unchanged.
22. **Explicit flat conversion safety:** on a flat account, `convert_released_pnl` rejects if the requested amount exceeds `max_safe_flat_conversion_released`.
23. **Same-epoch local settlement:** settlement of one account does not depend on any canonical-order prefix.
24. **Non-compounding quantity basis:** repeated same-epoch touches without explicit position mutation do not compound quantity-flooring loss.
25. **Dynamic dust bound:** after same-epoch zeroing events, basis replacements, and ADL multiplier truncations before a reset, authoritative OI on a side with no stored positions is bounded by that side's cumulative phantom-dust bound.
26. **Dust-clear scheduling:** dust clearance and reset initiation happen only at end of top-level instructions, never mid-instruction.
27. **Epoch-safe reset:** accounts cannot be attached to a new epoch before `begin_full_drain_reset` runs.
28. **Precision-exhaustion terminal drain:** if `A_candidate == 0` with `OI_post > 0`, the engine force-drains both sides instead of reverting.
29. **ADL representability fallback:** if `delta_K_abs` is non-representable or `K_opp + delta_K_exact` overflows, quantity socialization still proceeds and the remainder routes through `record_uninsured_protocol_loss`.
30. **Insurance-first deficit coverage:** `enqueue_adl` spends `I` down to `I_floor` before any remaining bankruptcy loss is socialized or left as junior undercollateralization.
31. **Funding transfer conservation under lazy settlement:** each funding sub-step applies the same exact `fund_num_step` to both sides' `F_side_num` updates with opposite signs, so the theoretical side-aggregate funding transfer is zero-sum before settlement rounding.
32. **Flat-account negative remainder:** a flat account with negative `PNL_i` after principal exhaustion resolves through `absorb_protocol_loss` only in the allowed already-authoritative flat-account paths.
33. **Reset finalization:** after reconciling stale accounts, the side can leave `ResetPending` and accept fresh OI again.
34. **Deposit loss seniority:** in `deposit`, realized losses are settled from newly deposited principal before any outstanding fee debt is swept.
35. **Deposit materialization threshold:** a missing account cannot be materialized by a deposit smaller than `MIN_INITIAL_DEPOSIT`.
36. **Risk-reducing trade exemption:** a strict non-flipping position reduction that improves the exact widened fee-neutral raw maintenance buffer is allowed even if the account remains below maintenance after the trade, provided the negative raw maintenance shortfall does not worsen.
37. **Risk-reducing metric specificity:** the strict risk-reducing before/after buffer comparison uses `Eq_maint_raw_i`, not `Eq_trade_raw_i` or `Eq_withdraw_raw_i`.
38. **Organic close bankruptcy guard:** a flat trade cannot bypass ADL by leaving negative `PNL_i` behind.
39. **Dead-account reclamation:** a live flat account with `0 <= C_i < MIN_INITIAL_DEPOSIT`, zero `PNL_i`, zero `R_i`, exact reserve queue empty, both overflow cohorts absent, zero basis, and nonpositive `fee_credits_i` can be reclaimed safely.
40. **Missing-account safety:** `settle_account`, `withdraw`, `execute_trade`, `liquidate`, `resolve_market`, `force_close_resolved`, and `keeper_crank` do not materialize missing accounts.
41. **Keeper single global accrual:** `keeper_crank` calls `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` exactly once per instruction and per-candidate exact revalidation does not reaccrue the market.
42. **Keeper local-touch equivalence:** the per-candidate exact local touch used inside `keeper_crank` is economically equivalent to `touch_account_live_local` on the same already-accrued state, including the wrapper-supplied shared `H_lock`.
43. **Keeper revalidation budget accounting:** `max_revalidations` bounds the number of normal exact current-state revalidation attempts on materialized accounts; missing-account skips do not count.
44. **No duplicate keeper touch before liquidation:** when `keeper_crank` liquidates a candidate, it does so from the already-touched current state and does not perform a second full touch of that same candidate inside the same attempt.
45. **Direct fee-credit repayment cap:** `deposit_fee_credits` applies only `min(amount, FeeDebt_i)`, never makes `fee_credits_i` positive, and increases `V` and `I` by exactly the applied amount.
46. **Optional account-fee purity:** `charge_account_fee` mutates only `C_i`, `fee_credits_i`, `I`, and `C_tot` through canonical helpers; it does not mutate `PNL_i`, reserve cohorts, side state, or `V`.
47. **Trade / withdraw separation:** a state may be trade-opening healthy under `Eq_trade_open_raw_i` while still failing withdrawal health under `Eq_withdraw_raw_i`.
48. **No unbacked aggregate trade collateral from positive PnL:** even with many accounts using fresh PnL for trading, the positive-PnL portion admitted by `g` remains globally bounded by `Residual`.
49. **Resolved-market reserve promotion:** `resolve_market` sets `PNL_matured_pos_tot = PNL_pos_tot` and later `prepare_account_for_resolved_touch(i)` clears local reserve bookkeeping without a second aggregate change.
50. **Resolved stale settlement immediate release:** positive PnL created by `settle_side_effects_resolved(i)` is immediately released.
51. **No resolved payout race:** `force_close_resolved` may reconcile a resolved account before terminal payout is unlocked, but it MUST NOT pay out positive claims until both stale-account counters are zero.
52. **Resolved force close aggregate safety:** terminal `force_close_resolved` uses canonical helpers and leaves no residual contribution from the closed account in `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, or reserve cohorts.
53. **Wrapper-supplied `H_lock` bound enforcement:** live instructions reject `H_lock < H_min` or `H_lock > H_max`.
54. **Wrapper-supplied funding-rate bound enforcement:** live accrual rejects any `funding_rate_e9_per_slot` whose magnitude exceeds `MAX_ABS_FUNDING_E9_PER_SLOT`.
55. **Pure-capital no-insurance-draw:** `deposit`, `deposit_fee_credits`, `top_up_insurance_fund`, and `charge_account_fee` do not call `absorb_protocol_loss`.
56. **Immediate-release aggregate correctness:** when `reserve_mode` is `ImmediateRelease` or `UseHLock(0)`, `PNL_matured_pos_tot` increases only by the true newly released increment, even if the account already has nonzero reserve cohorts.
57. **Flat negative cleanup path:** `settle_flat_negative_pnl` is a live-only permissionless cleanup path that can zero an already-flat negative `PNL_i` state without market accrual and without mutating side state.
58. **Resolved payout snapshot path independence:** once stale-account reconciliation completes, every terminal resolved close uses the same captured `resolved_payout_h_num / resolved_payout_h_den` snapshot regardless of caller order.
59. **Safe flat conversion closed form:** `max_safe_flat_conversion_released` returns the exact largest safe conversion amount using the closed-form formula, uses the capped exact helper or an equivalent exact wide comparison, and never reverts merely because the uncapped mathematical quotient exceeds `x_cap` or `u128::MAX`.
60. **Withdrawal hypothetical aggregate consistency:** an open-position withdrawal simulation decreases both `V` and `C_tot` by the candidate withdrawal amount, so `Residual` and the current live haircut `h` are unchanged by the simulation.
61. **Wrapper-owned live policy inputs:** public or permissionless wrappers do not expose arbitrary caller-chosen `H_lock` or live funding-rate inputs.
62. **Price-bounded resolution:** `resolve_market` is a privileged deployment-owned transition, uses zero funding for the settlement transition, and rejects `resolved_price` outside the immutable deviation band around `P_last`.
63. **Pending overflow activation:** when `overflow_older_i` is absent and `overflow_newest_i` is present, the next warmup-advance or reserve/loss helper that can activate it starts a new scheduled cohort at `current_slot` with `anchor_q = remaining_q`, `sched_release_q = 0`, and `H_overflow = H_max`.
64. **A-side-change dust bound:** when `enqueue_adl` performs quantity socialization with `OI_post < OI`, the conservative phantom-dust bound is added even if `A_prod_exact` divides `OI` exactly.
65. **Active-position side cap:** any 0-to-nonzero basis attachment that would push the relevant side above `MAX_ACTIVE_POSITIONS_PER_SIDE` is rejected.
66. **Fixed overflow horizon:** `overflow_older_i` and `overflow_newest_i` always use `H_overflow = H_max`; wrapper-supplied `H_lock` never shortens or extends them.
67. **High-precision funding exactness without sign bias:** a nonzero wrapper-supplied `funding_rate_e9_per_slot` smaller than 1 basis point per slot is accumulated exactly in `F_side_num` and therefore produces proportionate cumulative funding over elapsed time without positive-zero / negative-minus-one truncation asymmetry or shock-style wrapper injection.

## 13. Compatibility and upgrade notes

1. This revision keeps wrapper-owned horizon selection, bounded cohort storage, resolved payout snapshots, and flat negative cleanup, but changes live flat auto-conversion to **whole-only**.  
   To preserve the old fixed-wait behavior exactly, set `H_min = H_max = old T` and have the wrapper always pass that same `H_lock`.

2. This revision keeps `MAX_EXACT_RESERVE_COHORTS_PER_ACCOUNT` plus up to two bounded overflow segments: one preserved scheduled overflow cohort (`overflow_older_i`) and one pending overflow segment (`overflow_newest_i`). Deployments SHOULD size storage and compute budgets assuming the full total reserve-segment bound and SHOULD choose wrapper `H_lock` policies that make overflow usage uncommon in ordinary trading.

3. No new global accumulator is required for warmup demand.  
   `PendingWarmupTot` remains derived from existing aggregates:
   - `PNL_pos_tot - PNL_matured_pos_tot`

4. UI and API surfaces SHOULD distinguish three concepts:
   - maintenance equity
   - trade-opening equity
   - withdrawable / convertible equity

5. This revision intentionally does not let fresh PnL become principal or withdrawable merely because it is tradable.
   - tradable fresh PnL is still junior
   - still non-convertible before maturity
   - still excluded from `h` until warmup release

6. This revision also intentionally does not let a permissionless touch crystallize a temporary haircut on a flat account's released profit.
   - whole snapshots may still auto-convert flat released profit for convenience
   - lossy conversion under `h < 1` is explicit user action through `convert_released_pnl`

7. A deployment upgrading from v12.14.0 MUST update:
   - funding settlement to maintain `F_long_num`, `F_short_num`, `f_snap_i`, `F_epoch_start_long_num`, and `F_epoch_start_short_num`
   - `settle_side_effects_live` and `settle_side_effects_resolved` to use the combined `K_side` / `F_side_num` helper
   - overflow routing so both `overflow_older_i` and `overflow_newest_i` always use `H_overflow = H_max`
   - any implementation of `max_safe_flat_conversion_released` to use the capped exact helper or an equivalent exact wide comparison
   - live flat auto-conversion to the whole-only rule
   - `convert_released_pnl` to support flat accounts subject to the exact safe-cap rule
   - withdrawal simulation to decrease both `V` and `C_tot` in the hypothetical state
   - tests to include exact high-precision funding settlement, fixed-horizon overflow semantics, capped safe flat conversion, whole-only auto-conversion, no permissionless lossy crystallization, and aggregate-consistent withdrawal simulation
## 14. Short wrapper note (deployment obligations, not engine-checked)

The following requirements are obligations of a compliant deployment wrapper or enclosing runtime. They are **not** engine-checked arithmetic invariants except where §10 or §§1–2 explicitly say the engine validates a bound.

1. **Do not expose caller-controlled live policy inputs.**  
   The `H_lock` and `funding_rate_e9_per_slot` inputs appearing in live logical entrypoints of §10 are wrapper-owned internal policy inputs. Public or permissionless wrappers MUST derive them internally from trusted on-chain state or wrapper policy and MUST NOT accept arbitrary caller-chosen values. `H_lock` governs exact-cohort creation only; once exact reserve capacity is exhausted, overflow segments use the immutable conservative horizon `H_overflow = H_max`.

2. **Authority-gate market resolution.**  
   `resolve_market` is a privileged deployment-owned transition. A compliant public wrapper MUST NOT expose it as a permissionless user path and MUST source `resolved_price` from the deployment's trusted settlement source or settlement policy. A compliant wrapper SHOULD refresh live market state immediately before invoking `resolve_market` when the deployment expects the immutable settlement band around `P_last` to reflect the latest live mark rather than an older stale mark.

3. **Public wrappers SHOULD enforce execution-price admissibility.**  
   A sufficient rule is `abs(exec_price - oracle_price) * 10_000 <= max_trade_price_deviation_bps * oracle_price` with `max_trade_price_deviation_bps <= 2 * trading_fee_bps`; any equivalent anti-off-market or anti-self-match protection is acceptable.

4. **Use oracle notional for wrapper-side exposure ranking.**  
   Any wrapper-side risk buffer, shortlist priority, or eviction ranking keyed on exposure MUST use oracle notional, never execution-price notional.

5. **Keep user-owned value-moving operations account-authorized.**  
   A compliant public wrapper MUST require the affected account's authorization for user-owned value-moving paths such as `deposit`, `withdraw`, `execute_trade`, and `convert_released_pnl`. The intended permissionless progress paths are `settle_account`, `liquidate`, `reclaim_empty_account`, `settle_flat_negative_pnl`, `force_close_resolved`, and `keeper_crank`.

6. **Provide a post-snapshot resolved-close progress path.**  
   Because `force_close_resolved` is intentionally multi-stage, a compliant deployment SHOULD provide either (a) an owner-facing self-service path that retries terminal close after stale reconciliation completes, or (b) a permissionless batch / incentive mechanism that sweeps resolved accounts once the shared resolved-payout snapshot is ready.

