# Risk Engine Spec (Source of Truth) — v12.18.5

**Combined Single-Document Native 128-bit Revision  
(Wrapper-Owned Two-Point Warmup Admission / Touch-Time Reserve Re-Admission / Wrapper-Owned Account-Fee Policy / Per-Account Recurring-Fee Checkpoint / Wrapper-Supplied High-Precision Funding Side-Index Input / Simplified Scheduled-Plus-Pending Warmup / Exact Candidate-Trade Neutralization / Self-Synchronizing Terminal-K-Delta Resolved Settlement / Whole-Only Automatic Flat Conversion / Full-Local-PnL Maintenance / Immutable Configuration / Unencumbered-Flat Deposit Sweep / Mandatory Post-Partial Local Health Check Edition)**

**Design:** Protected principal + junior profit claims + lazy A/K/F side indices (native 128-bit base-10 scaling)  
**Status:** implementation source of truth (normative language: MUST / MUST NOT / SHOULD / MAY)  
**Scope:** perpetual DEX risk engine for a single quote-token vault

This revision supersedes v12.18.4. It keeps the two-bucket warmup design, keeps resolved settlement terminal-delta based, and closes the remaining spec-level gaps around explicit resolution-mode selection, recurring-fee sync overflow semantics, and phantom-dust accounting.

The main deltas from v12.18.4 are:

1. preserve the wrapper-supplied two-point admission pair `(admit_h_min, admit_h_max)`,
2. preserve sticky `admit_h_max` within one instruction so fresh reserve cannot be under-admitted,
3. preserve touch-time outstanding-reserve re-admission,
4. restore an explicit `resolve_mode ∈ {Ordinary, Degenerate}` selector for `resolve_market`; value-detected branch selection is forbidden,
5. preserve the funding envelope (`cfg_max_accrual_dt_slots`, `cfg_max_abs_funding_e9_per_slot`) and the privileged degenerate recovery resolution branch,
6. preserve `last_fee_slot_i` as a persistent per-account checkpoint for wrapper-owned recurring fees,
7. define a canonical fee-sync helper that charges exactly once over `[last_fee_slot_i, fee_slot_anchor]`, advances `last_fee_slot_i`, and uses explicit saturating-to-`MAX_PROTOCOL_FEE_ABS` overflow semantics,
8. require new accounts to anchor `last_fee_slot_i` at their materialization slot so they do not inherit pre-creation fees,
9. require resolved-market recurring fee sync to anchor at `resolved_slot`, never after it,
10. make the same-epoch phantom-dust rules explicit: basis-replacement orphan remainder and same-epoch decay-to-zero each increment the relevant bound by exactly `1` q-unit,
11. make the scheduled-bucket warmup release rule explicit when the bucket empties, so no stale `sched_release_q` cursor survives on a non-empty bucket.

The engine core still keeps only:

- one **scheduled** reserve bucket plus one **pending** reserve bucket per live account,
- `PNL_matured_pos_tot`,
- the global trade haircut `g`,
- the matured-profit haircut `h`,
- the exact trade-open counterfactual approval metric `Eq_trade_open_raw_i`,
- capital, fee-debt, insurance, and recurring-fee-checkpoint accounting,
- lazy A/K/F settlement,
- liquidation and reset mechanics,
- resolved-market local reconciliation, shared positive-payout snapshot capture, and terminal close.

The following policy inputs remain wrapper-owned and are **not** derived by the engine core:

- the live accrued instruction admission pair `(admit_h_min, admit_h_max)`,
- any optional wrapper-owned recurring account-fee rate or equivalent fee function,
- the funding rate applied to the elapsed live interval,
- any public execution-price admissibility policy,
- any mark-EWMA or premium-funding model.

The engine validates bounds and exactness requirements where applicable, but it does not derive those policies.

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
16. **Economically negligible ADL truncation before `DrainOnly`:** under the configured `ADL_ONE` and `MIN_A_SIDE`, same-epoch A-decay dust deferred into `phantom_dust_bound_*_q` MUST remain economically negligible before a side can remain live in `DrainOnly`.
17. **No hidden protocol MM:** the protocol MUST NOT secretly internalize user flow against an undisclosed residual inventory.
18. **Defined recovery from precision stress:** the engine MUST define deterministic recovery when side precision is exhausted. It MUST NOT rely on assertion failure, silent overflow, or permanent `DrainOnly` states.
19. **No sequential quantity dependency:** same-epoch account settlement MUST be fully local. It MAY depend on the account’s own stored basis and current global side state, but MUST NOT require a canonical-order prefix or global carry cursor.
20. **Protocol-fee neutrality:** explicit protocol fees MUST either be collected into `I` immediately or tracked as account-local fee debt up to the account’s collectible capital-plus-fee-debt limit. Any explicit fee amount beyond that collectible limit MUST be dropped rather than socialized through `h`, through `g`, or inflated into bankruptcy deficit `D`.
21. **Strict risk-reducing neutrality uses actual fee impact:** any “fee-neutral” strict risk-reducing comparison MUST add back the account’s **actual applied fee-equity impact**, not the nominal requested fee amount.
22. **Synthetic liquidation price integrity:** a synthetic liquidation close MUST execute at the current oracle mark with zero execution-price slippage. Any liquidation penalty MUST be represented only by explicit fee state.
23. **Loss seniority over engine-native protocol fees:** when a trade or a non-bankruptcy liquidation realizes trading losses for an account, those losses are senior to engine-native trade and liquidation fee collection from that same local capital state.
24. **Deterministic overflow handling:** any arithmetic condition that is not proven unreachable by the numeric bounds MUST have a deterministic fail-safe or bounded fallback path. Silent wrap, unchecked panic, and undefined truncation are forbidden.
25. **Finite-capacity liveness:** because account capacity is finite, the engine MUST provide permissionless dead-account reclamation or equivalent slot reuse so abandoned empty accounts and flat dust accounts below the live-balance floor cannot permanently exhaust capacity.
26. **Permissionless off-chain keeper compatibility:** candidate discovery MAY be performed entirely off chain. The engine MUST expose exact current-state shortlist processing and targeted per-account settle, liquidate, reclaim, or resolved-close paths so any permissionless keeper can make liquidation and reset progress without any required on-chain phase-1 scan.
27. **No pure-capital insurance draw without accrual:** pure capital-flow instructions (`deposit`, `deposit_fee_credits`, `top_up_insurance_fund`, `charge_account_fee`) that do not call `accrue_market_to` MUST NOT decrement `I` or record uninsured protocol loss.
28. **Configuration immutability within a market instance:** warmup bounds, admission bounds, trade-fee, margin, liquidation, insurance-floor, funding envelope, and live-balance-floor parameters MUST remain fixed for the lifetime of a market instance unless a future revision defines an explicit safe update procedure.
29. **Scheduled-bucket exactness:** the active scheduled reserve bucket MUST mature according to its stored `sched_horizon` up to the required integer flooring and reserve-loss caps.
30. **Resolved-market close exactness:** resolved-market close MUST be defined through canonical helpers. It MUST NOT rely on direct zero-writes that bypass `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, reserve state, fee-checkpoint state, or reset counters.
31. **Path-independent touched-account finalization:** flat auto-conversion and fee-debt sweep on live touched accounts MUST depend only on the post-live touched state and the shared conversion snapshot, not on whether the instruction was single-touch or multi-touch.
32. **No resolved payout race:** resolved accounts with positive claims MUST NOT be terminally paid out until stale-account reconciliation is complete across both sides and the shared resolved-payout snapshot is locked.
33. **Path-independent resolved positive payouts:** once stale-account reconciliation is complete and terminal payout becomes unlocked, all positive resolved payouts MUST use one shared resolved-payout snapshot so caller order cannot improve the payout ratio.
34. **Bounded resolved settlement price on the ordinary resolution path:** when `resolve_market` uses its ordinary self-synchronizing live-sync branch, the resolved settlement price MUST remain within an immutable deviation band of the trusted live-sync price supplied for that instruction. The privileged degenerate recovery branch may bypass this band and rely entirely on trusted settlement inputs.
35. **No permissionless haircut realization of flat released profit:** automatic flat conversion in live instructions MUST occur only at a whole snapshot (`h = 1`). Any lossy conversion of released profit under `h < 1` MUST be an explicit user action.
36. **No retroactive funding erasure at ordinary resolution:** in the ordinary self-synchronizing `resolve_market` path, the zero-funding settlement shift MUST only operate on market state already accrued through the resolution slot, so the settlement transition cannot erase elapsed live funding. The privileged degenerate recovery branch may intentionally skip omitted live accrual after `slot_last` and therefore must rely entirely on trusted settlement policy.
37. **No silent touched-set or admission-state truncation:** every account touched by live local touch and every account recorded in instruction-local admission state MUST either be tracked in the instruction context or the instruction MUST fail conservatively.
38. **No valid-price sentinel overloading:** no strictly positive price value may be used as an “uninitialized” sentinel for `P_last`, `fund_px_last`, or any other economically meaningful stored price field.
39. **Self-synchronizing resolution with a privileged degenerate-recovery escape hatch:** `resolve_market` MUST ordinarily synchronize live accrual to its resolution slot inside the same top-level instruction before applying the final zero-funding settlement shift. The same privileged instruction MAY instead take the explicit degenerate recovery branch described in §9.8 when the deployment needs to avoid additional live-state shift — for example because the accrual envelope has already been exceeded or cumulative `K` or `F` headroom is tight.
40. **Bounded-cost exact arithmetic:** the specification MUST permit exact implementations of scheduled warmup release and funding accrual without runtime work proportional to elapsed slots and without relying on narrow intermediate products that can overflow before the exact quotient is taken.
41. **Runtime-aware deployment constraints:** on constrained runtimes, deployments MUST choose batch sizes, account-opening economics, funding envelopes, and wrapper composition so exact wide arithmetic, materialized-account capacity, and transaction-size limits do not create avoidable operational deadlocks.
42. **Resolution must not depend on cumulative-K absorption of the final settlement mark:** the final settlement price shift is carried as separate resolved terminal K deltas rather than added into persistent live `K_side`.
43. **Resolved reconciliation must not deadlock on live-only claim caps:** once the market is resolved, local reconciliation MAY exceed live-market positive-PnL caps so long as all persistent values remain representable and terminal payout remains snapshot-capped.
44. **No live positive-PnL bypass of admission:** every positive reserve-creating event on a live market MUST pass through the two-point admission rule; there is no unconditional live `ImmediateRelease` path.
45. **No same-instruction under-admission:** within one top-level instruction, once an account requires the slow admitted horizon `admit_h_max` for any fresh positive increment, all later fresh positive increments on that account in that instruction MUST also use `admit_h_max`. An earlier newest pending increment MAY be conservatively lifted to `admit_h_max` if it merges with a later slower-admitted increment; under-admission is forbidden.
46. **Touch-time reserve acceleration is monotone:** touching a live account may only accelerate existing reserve by removing buckets when the current state safely admits immediate release; it MUST never extend or re-lock reserve.
47. **No inherited recurring fees for new accounts:** a newly materialized account MUST anchor its recurring-fee checkpoint at its materialization slot and MUST NOT be charged for earlier time.
48. **Exact touched-account recurring-fee liveness:** if a deployment enables wrapper-owned recurring account fees, a touched account MUST be fee-syncable from `last_fee_slot_i` to the relevant slot anchor without a global scan.
49. **No post-resolution recurring-fee accrual:** recurring account fees, if enabled by the wrapper, accrue only over live time and MUST NOT be charged past `resolved_slot`.
50. **Resolved payout snapshot stability under late fee sync:** fee sync or fee forgiveness performed after the shared resolved payout snapshot is captured MUST NOT invalidate that snapshot’s correctness. The snapshot is over `Residual = V - (C_tot + I)` and pure `C -> I` reclassification must preserve it.
51. **No implicit degenerate-mode selection:** the ordinary vs degenerate `resolve_market` branch MUST be chosen only from an explicit trusted wrapper mode input. Equality of economic values such as `live_oracle_price == P_last` or `funding_rate_e9_per_slot == 0` MUST NOT by itself force the degenerate branch.

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
- Every external price input, including `oracle_price`, `exec_price`, `live_oracle_price`, `resolved_price`, and any stored funding-price sample, MUST satisfy `0 < price <= MAX_ORACLE_PRICE`.
- The engine stores position bases as signed fixed-point base quantities:
  - `basis_pos_q_i: i128`, units `(base * POS_SCALE)`.
- Oracle notional:
  - `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`.
- Trade fees use executed size:
  - `trade_notional = mul_div_floor_u128(size_q, exec_price, POS_SCALE)`.

### 1.3 A/K/F scales

- `ADL_ONE = 1_000_000_000_000_000`.
- `A_side` is dimensionless and scaled by `ADL_ONE`.
- `K_side` has units `(ADL scale) * (quote atomic units per 1 base)`.
- `FUNDING_DEN = 1_000_000_000`.
- `F_side_num` has units `(ADL scale) * (quote atomic units per 1 base) * FUNDING_DEN`.

### 1.4 Normative bounds and configuration

Global hard bounds:

- `MAX_VAULT_TVL = 10_000_000_000_000_000`
- `MAX_ORACLE_PRICE = 1_000_000_000_000`
- `MAX_POSITION_ABS_Q = 100_000_000_000_000`
- `MAX_TRADE_SIZE_Q = MAX_POSITION_ABS_Q`
- `MAX_OI_SIDE_Q = 100_000_000_000_000`
- `MAX_ACCOUNT_NOTIONAL = 100_000_000_000_000_000_000`
- `MAX_PROTOCOL_FEE_ABS = 1_000_000_000_000_000_000_000_000_000_000_000_000`
- `GLOBAL_MAX_ABS_FUNDING_E9_PER_SLOT = 1_000_000_000`
- `MAX_TRADING_FEE_BPS = 10_000`
- `MAX_INITIAL_BPS = 10_000`
- `MAX_MAINTENANCE_BPS = 10_000`
- `MAX_LIQUIDATION_FEE_BPS = 10_000`
- `MAX_MATERIALIZED_ACCOUNTS = 1_000_000`
- `MAX_ACTIVE_POSITIONS_PER_SIDE` MUST be finite and MUST NOT exceed `MAX_MATERIALIZED_ACCOUNTS`
- `MAX_ACCOUNT_POSITIVE_PNL_LIVE = 100_000_000_000_000_000_000_000_000_000_000`
- `MAX_PNL_POS_TOT_LIVE = 100_000_000_000_000_000_000_000_000_000_000_000_000`
- `MIN_A_SIDE = 100_000_000_000_000`
- `MAX_WARMUP_SLOTS = 18_446_744_073_709_551_615`
- `MAX_RESOLVE_PRICE_DEVIATION_BPS = 10_000`

Immutable per-market configuration:

- `cfg_h_min`
- `cfg_h_max`
- `cfg_maintenance_bps`
- `cfg_initial_bps`
- `cfg_trading_fee_bps`
- `cfg_liquidation_fee_bps`
- `cfg_liquidation_fee_cap`
- `cfg_min_liquidation_abs`
- `cfg_min_initial_deposit`
- `cfg_min_nonzero_mm_req`
- `cfg_min_nonzero_im_req`
- `cfg_insurance_floor`
- `cfg_resolve_price_deviation_bps`
- `cfg_max_active_positions_per_side`
- `cfg_max_accrual_dt_slots`
- `cfg_max_abs_funding_e9_per_slot`

Configured values MUST satisfy:

- `0 < cfg_min_initial_deposit <= MAX_VAULT_TVL`
- `0 < cfg_min_nonzero_mm_req < cfg_min_nonzero_im_req <= cfg_min_initial_deposit`
- `0 <= cfg_maintenance_bps <= cfg_initial_bps <= MAX_INITIAL_BPS`
- `0 <= cfg_h_min <= cfg_h_max <= MAX_WARMUP_SLOTS`
- live instruction admission pairs MUST satisfy `0 <= admit_h_min <= admit_h_max <= cfg_h_max`
- if `admit_h_min > 0`, then `admit_h_min >= cfg_h_min`
- for live instructions that may create fresh reserve, `admit_h_max > 0` and `admit_h_max >= cfg_h_min`
- `0 <= cfg_resolve_price_deviation_bps <= MAX_RESOLVE_PRICE_DEVIATION_BPS`
- `0 <= cfg_insurance_floor <= MAX_VAULT_TVL`
- `0 <= cfg_min_liquidation_abs <= cfg_liquidation_fee_cap <= MAX_PROTOCOL_FEE_ABS`
- `0 < cfg_max_active_positions_per_side <= MAX_ACTIVE_POSITIONS_PER_SIDE`
- `0 < cfg_max_accrual_dt_slots <= MAX_WARMUP_SLOTS`
- `0 <= cfg_max_abs_funding_e9_per_slot <= GLOBAL_MAX_ABS_FUNDING_E9_PER_SLOT`
- exact init-time funding-envelope validation:
  - `ADL_ONE * MAX_ORACLE_PRICE * cfg_max_abs_funding_e9_per_slot * cfg_max_accrual_dt_slots <= i128::MAX`
  - this validation MUST be performed in an exact wide signed domain of at least 256 bits, or a formally equivalent exact method

If the deployment also defines a stale-market resolution delay `permissionless_resolve_stale_slots` and expects permissionless resolution to remain callable after that delay, then initialization MUST additionally require:

- `permissionless_resolve_stale_slots <= cfg_max_accrual_dt_slots`

Deployments that rely only on privileged degenerate recovery resolution MAY omit `permissionless_resolve_stale_slots` entirely.

The bounds `MAX_ACCOUNT_POSITIVE_PNL_LIVE` and `MAX_PNL_POS_TOT_LIVE` are **live-market** safety caps. They MUST hold whenever `market_mode == Live`. After `market_mode == Resolved`, local reconciliation and payout preparation MAY exceed those live caps, provided all resulting persistent values remain representable in their stored integer types and all payout arithmetic remains exact and conservative.

### 1.5 Trusted time and oracle requirements

- `now_slot` in every top-level instruction MUST come from trusted runtime slot metadata or an equivalent trusted source.
- `oracle_price` inputs MUST come from validated configured oracle feeds or trusted privileged settlement sources, depending on the instruction’s trust boundary.
- Any helper or instruction that accepts `now_slot` MUST require `now_slot >= current_slot`.
- Any call to `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` MUST require `now_slot >= slot_last`.
- Every live accrual MUST require `dt = now_slot - slot_last <= cfg_max_accrual_dt_slots`.
- `current_slot` and `slot_last` MUST be monotonically nondecreasing.
- The engine MUST NOT overload any strictly positive price value as an uninitialized sentinel for `P_last`, `fund_px_last`, or any equivalent stored price field.
- Any recurring-fee sync anchor `fee_slot_anchor` MUST satisfy:
  - on live markets: `last_fee_slot_i <= fee_slot_anchor <= current_slot`
  - on resolved markets: `last_fee_slot_i <= fee_slot_anchor <= resolved_slot`

### 1.6 Required exact helpers

Implementations MUST provide exact checked helpers for at least:

- checked `add`, `sub`, and `mul` on `u128` and `i128`,
- checked cast helpers,
- exact conservative signed floor division,
- exact floor and ceil multiply-divide helpers,
- `fee_debt_u128_checked(fee_credits_i)`,
- `fee_credit_headroom_u128_checked(fee_credits_i)`,
- `wide_signed_mul_div_floor_from_kf_pair(abs_basis, k_then, k_now_exact, f_then, f_now_exact, den)`, where `k_then` and `f_then` are persistent i128 snapshots and `k_now_exact` and `f_now_exact` may be either persistent i128 values or exact wide signed values.

Its canonical law is:

`wide_signed_mul_div_floor_from_kf_pair(abs_basis, k_then, k_now_exact, f_then, f_now_exact, den)`  
`= floor( abs_basis * ( ((k_now_exact - k_then) * FUNDING_DEN) + (f_now_exact - f_then) ) / (den * FUNDING_DEN) )`

with floor toward negative infinity in the exact widened signed domain. The helper MUST use at least exact 256-bit signed intermediates, or a formally equivalent exact method. Implementations MUST NOT add `ΔK` and `ΔF` directly without this `FUNDING_DEN` un-scaling.

### 1.7 Arithmetic requirements

The engine MUST satisfy all of the following.

1. Every product involving `A_side`, `K_side`, `F_side_num`, `k_snap_i`, `f_snap_i`, `basis_pos_q_i`, `effective_pos_q(i)`, `price`, the raw funding numerator `fund_px_0 * funding_rate_e9_per_slot * dt`, trade-haircut numerators, trade-open counterfactual positive-aggregate numerators, scheduled-bucket release numerators, or ADL deltas MUST use checked arithmetic or an exact checked multiply-divide helper that is mathematically equivalent to the full-width product.
2. `accrue_market_to` MUST apply the exact total funding delta over the full interval `dt`. Implementations MAY use internal chunking only if it is exactly equivalent to the total-delta law and does not require an unbounded runtime loop proportional to `dt`.
3. The conservation check `V >= C_tot + I` and any `Residual` computation MUST use checked addition for `C_tot + I`.
4. Signed division with positive denominator MUST use exact conservative floor division.
5. Exact multiply-divide helpers MUST return the exact quotient even when the exact product exceeds native `u128`, provided the final quotient fits.
6. `PendingWarmupTot = PNL_pos_tot - PNL_matured_pos_tot` MUST use checked subtraction.
7. Haircut paths `floor(ReleasedPos_i * h_num / h_den)`, `floor(PosPNL_i * g_num / g_den)`, and the exact candidate-open trade-haircut path of §3.5 MUST use exact multiply-divide helpers.
8. Funding transfer MUST use the same exact total `fund_num_total = fund_px_0 * funding_rate_e9_per_slot * dt` value for both sides’ `F_side_num` deltas, with opposite signs. The engine MUST NOT introduce per-step or per-chunk rounding inside `accrue_market_to`.
9. `fund_num_total`, each `A_side * fund_num_total` product, and each live mark-to-market `A_side * (oracle_price - P_last)` product MUST be computed in an exact wide signed domain of at least 256 bits, or a formally equivalent exact method. `K_side` and `F_side_num` are cumulative across epochs. Implementations MUST use checked arithmetic and fail conservatively on persistent `i128` overflow.
10. Same-epoch or epoch-mismatch settlement MUST combine `K_side` and `F_side_num` through the exact helper `wide_signed_mul_div_floor_from_kf_pair`. The helper MUST accept exact wide signed terminal values such as `K_epoch_start_side + resolved_k_terminal_delta_side`, even when that terminal sum is not itself persisted as a live `K_side`.
11. The ADL quote-deficit path MUST compute `delta_K_abs = ceil(D_rem * A_old * POS_SCALE / OI_before)` using exact wide arithmetic.
12. If a K-index delta magnitude is representable but `K_opp + delta_K_exact` overflows `i128`, the engine MUST route `D_rem` through `record_uninsured_protocol_loss` while still continuing quantity socialization.
13. `PNL_i` MUST be maintained in `[i128::MIN + 1, i128::MAX]`, and `fee_credits_i` in `[i128::MIN + 1, 0]`.
14. Every decrement of `stored_pos_count_*`, `stale_account_count_*`, or `phantom_dust_bound_*_q` MUST use checked subtraction.
15. Every increment of `stored_pos_count_*`, `phantom_dust_bound_*_q`, `epoch_side`, `materialized_account_count`, `neg_pnl_account_count`, `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, `V`, or `I` MUST use checked addition and MUST enforce the relevant bound.
16. `trade_notional <= MAX_ACCOUNT_NOTIONAL` MUST be enforced before charging trade fees.
17. Any out-of-range price input, invalid oracle read, invalid live admission pair, invalid `funding_rate_e9_per_slot`, invalid degenerate-resolution inputs, invalid recurring-fee anchor, or non-monotonic slot input MUST fail conservatively before state mutation.
18. `charge_fee_to_insurance` MUST cap its applied fee at the account’s exact collectible capital-plus-fee-debt headroom. It MUST never set `fee_credits_i < -(i128::MAX)`.
19. Any direct fee-credit repayment path MUST cap its applied amount at the exact current `FeeDebt_i`. It MUST never set `fee_credits_i > 0`.
20. Any direct insurance top-up or direct fee-credit repayment path that increases `V` or `I` MUST use checked addition and MUST enforce `MAX_VAULT_TVL`.
21. Scheduled- and pending-bucket mutations MUST preserve the invariants of §2.1 and MUST use checked arithmetic.
22. The exact counterfactual trade-open computation MUST recompute the account’s positive-PnL contribution and the global positive-PnL aggregate with the candidate trade’s own positive slippage gain removed.
23. Any wrapper-owned fee amount routed through the canonical helper MUST satisfy `fee_abs <= MAX_PROTOCOL_FEE_ABS`.
24. Fresh reserve MUST NOT be merged into an older scheduled bucket unless that bucket was itself created in the current slot, has the same admitted horizon, and has `sched_release_q == 0`.
25. Pending-bucket horizon updates MUST be monotone nondecreasing with `pending_horizon_i = max(pending_horizon_i, admitted_h_eff)` whenever new reserve is merged into an existing pending bucket. This monotone re-horizoning is intentionally conservative for the newest pending bucket and MUST NEVER affect the scheduled bucket.
26. If a live positive increase occurs, the engine MUST admit it through `admit_fresh_reserve_h_lock`; the only path that may immediately release positive PnL without live admission is `ImmediateReleaseResolvedOnly` on resolved markets.
27. Funding exactness MUST NOT depend on a bare global remainder with no per-account snapshot. Any retained fractional precision across calls MUST be represented through `F_side_num` and `f_snap_i`.
28. Any strict risk-reducing fee-neutral comparison MUST add back `fee_equity_impact_i`, not nominal fee.
29. `max_safe_flat_conversion_released` MUST use at least 256-bit exact intermediates, or a formally equivalent exact wide comparison, whenever `E_before * h_den` would exceed native `u128`.
30. Any helper that computes bucket maturity from `elapsed / sched_horizon` MUST clamp `elapsed` at `sched_horizon` before invoking an exact multiply-divide helper whose unclamped final quotient could exceed `u128` even though the clamped economic answer is `sched_anchor_q`.
31. Any helper precondition reachable from a top-level instruction MUST fail conservatively rather than panic or assert on caller-controlled inputs or mutable market state.
32. `phantom_dust_bound_long_q` and `phantom_dust_bound_short_q` are bounded by `u128` representability; any attempted overflow is a conservative failure.
33. Even after `market_mode == Resolved`, aggregate persistent quantities stored as `u128` — including `PNL_pos_tot` and `PNL_matured_pos_tot` — MUST remain representable in `u128`; any reconciliation or terminal-close path that would overflow them MUST fail conservatively rather than wrap.
34. All touched-account and instruction-local admission-state structures in `ctx` MUST be provisioned to hold the maximum number of distinct accounts any top-level instruction in this revision can touch or admit; if capacity would be exceeded, the instruction MUST fail conservatively.
35. `last_fee_slot_i` MUST be initialized, advanced, and reset only through canonical helper paths. A new account MUST start at its materialization slot, and a freed slot MUST return to `0`.
36. Recurring-fee sync to a resolved account MUST use `fee_slot_anchor = resolved_slot`, never `current_slot` if `current_slot > resolved_slot`.
37. A late recurring-fee sync after the resolved payout snapshot is captured MUST preserve `Residual = V - (C_tot + I)` except for intentionally dropped uncollectible fee tails, which are conservatively ignored rather than socialized.
38. `sync_account_fee_to_slot` MUST interpret `fee_rate_per_slot * dt` with explicit saturating-to-`MAX_PROTOCOL_FEE_ABS` semantics. It MUST either compute the product in an exact widened domain of at least 256 bits and then cap, or use an exactly equivalent branch on `fee_rate_per_slot > floor(MAX_PROTOCOL_FEE_ABS / dt)` for `dt > 0`. The helper MUST NOT fail solely because the uncapped raw fee product exceeds native `u128`.

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
- `last_fee_slot_i: u64` — per-account recurring-fee checkpoint

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
  - `cfg_h_min <= sched_horizon_i <= cfg_h_max`
  - `0 <= sched_release_q_i <= sched_anchor_q_i`
- if `pending_present_i`:
  - `0 < pending_remaining_q_i`
  - `cfg_h_min <= pending_horizon_i <= cfg_h_max`
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

Fee-credit and fee-slot bounds:

- `fee_credits_i` MUST be initialized to `0`
- the engine MUST maintain `-(i128::MAX) <= fee_credits_i <= 0`
- `fee_credits_i == i128::MIN` is forbidden
- if `market_mode == Live`, `last_fee_slot_i <= current_slot`
- if `market_mode == Resolved`, `last_fee_slot_i <= resolved_slot`
- `last_fee_slot_i` MUST be set to the account’s materialization slot on creation
- on free-slot reset, `last_fee_slot_i` MUST be cleared to `0`

#### 2.1.1 Wrapper-owned annotation fields (non-normative)

An engine implementation MAY carry additional per-account fields used by the deployment wrapper for its own bookkeeping — typical examples include an owner pubkey, an account-kind tag (user vs LP), a matching-engine program id, and a matching-engine context id. These fields are **wrapper-owned opaque annotation**. The engine MUST:

- store and canonicalize them through its normal materialization / reset / init paths so they do not leak stale data across slot reuse;
- **never** read them to decide any spec-normative behavior (margin health, liquidation eligibility, fee routing, reserve admission, accrual, resolution, reset lifecycle, conservation, authorization, or any other property enumerated in §0);
- treat them as inert payload on every engine-level path.

Authorization (who may call `deposit`, `withdraw`, `trade`, etc. on behalf of which account) is a **wrapper responsibility**, not an engine invariant. The engine MAY expose defensive helpers (e.g., a one-time `set_owner` that refuses to overwrite a nonzero owner or to write the zero pubkey) to preserve a "zero iff unclaimed" convention, but such helpers are conveniences for wrappers and carry no spec-level semantics.

Because these fields carry no engine-level semantics, they are outside the normative scope of this document. Deployments that do not need them MAY omit them from the Account struct entirely; deployments that do need them MAY carry any finite set of such opaque annotations. The engine's spec-level behavior MUST be identical in either case.

### 2.2 Global engine state

The engine stores at least:

- `V: u128`
- `I: u128`
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
- `neg_pnl_account_count: u64`
- `C_tot: u128`
- `PNL_pos_tot: u128`
- `PNL_matured_pos_tot: u128`

Immutable per-market configuration fields from §1.4 are stored in engine state and are part of the market instance.

Resolved-market state:

- `market_mode ∈ {Live, Resolved}`
- `resolved_price: u64`
- `resolved_live_price: u64`
- `resolved_slot: u64`
- `resolved_k_long_terminal_delta: i128`
- `resolved_k_short_terminal_delta: i128`
- `resolved_payout_snapshot_ready: bool`
- `resolved_payout_h_num: u128`
- `resolved_payout_h_den: u128`

Derived global quantity:

- `PendingWarmupTot = PNL_pos_tot - PNL_matured_pos_tot`

Global invariants:

- `C_tot <= V <= MAX_VAULT_TVL`
- `I <= V`
- `0 <= neg_pnl_account_count <= materialized_account_count <= MAX_MATERIALIZED_ACCOUNTS`
- `F_long_num` and `F_short_num` MUST remain representable as `i128`
- if `market_mode == Live`:
  - `PNL_matured_pos_tot <= PNL_pos_tot <= MAX_PNL_POS_TOT_LIVE`
  - `resolved_price == 0`
  - `resolved_live_price == 0`
  - `resolved_k_long_terminal_delta == 0`
  - `resolved_k_short_terminal_delta == 0`
- if `market_mode == Resolved`:
  - `resolved_price > 0`
  - `resolved_live_price > 0`
  - `PNL_matured_pos_tot <= PNL_pos_tot`
  - `resolved_k_long_terminal_delta` and `resolved_k_short_terminal_delta` are representable as `i128`
- if `resolved_payout_snapshot_ready == false`, then `resolved_payout_h_num == 0` and `resolved_payout_h_den == 0`
- if `resolved_payout_snapshot_ready == true`, then `resolved_payout_h_num <= resolved_payout_h_den`

### 2.3 Instruction context

Every top-level live instruction that uses the standard lifecycle MUST initialize a fresh ephemeral context `ctx` with at least:

- `pending_reset_long: bool`
- `pending_reset_short: bool`
- `admit_h_min_shared: u64`
- `admit_h_max_shared: u64`
- `touched_accounts[]` — deduplicated touched storage indices
- `h_max_sticky_accounts[]` — per-instruction set of storage indices for which `admit_h_max` has already been required in the current instruction

Capacity rules:

- `ctx.touched_accounts[]` capacity MUST be at least the maximum number of distinct accounts any single top-level instruction in this revision can touch.
- `ctx.h_max_sticky_accounts[]` capacity MUST be at least the maximum number of distinct accounts any single top-level instruction in this revision can both touch and create fresh reserve for.
- Implementations MAY choose to size both structures equally, which is sufficient in this revision.
- If insertion into either structure would exceed capacity, the instruction MUST fail conservatively.

### 2.4 Configuration immutability

No external instruction in this revision may change:

- `cfg_h_min`
- `cfg_h_max`
- `cfg_maintenance_bps`
- `cfg_initial_bps`
- `cfg_trading_fee_bps`
- `cfg_liquidation_fee_bps`
- `cfg_liquidation_fee_cap`
- `cfg_min_liquidation_abs`
- `cfg_min_initial_deposit`
- `cfg_min_nonzero_mm_req`
- `cfg_min_nonzero_im_req`
- `cfg_insurance_floor`
- `cfg_resolve_price_deviation_bps`
- `cfg_max_active_positions_per_side`
- `cfg_max_accrual_dt_slots`
- `cfg_max_abs_funding_e9_per_slot`

### 2.5 Materialized-account capacity

The engine MUST track the number of currently materialized account slots. That count MUST NOT exceed `MAX_MATERIALIZED_ACCOUNTS`.

A missing account is one whose slot is not currently materialized. Missing accounts MUST NOT be auto-materialized by `settle_account`, `withdraw`, `execute_trade`, `close_account`, `liquidate`, `resolve_market`, `force_close_resolved`, or `keeper_crank`.

Only the following path MAY materialize a missing account:

- `deposit(i, amount, now_slot)` with `amount >= cfg_min_initial_deposit`

### 2.6 Canonical zero-position defaults

The canonical zero-position account defaults are:

- `basis_pos_q_i = 0`
- `a_basis_i = ADL_ONE`
- `k_snap_i = 0`
- `f_snap_i = 0`
- `epoch_snap_i = 0`

### 2.7 Account materialization

`materialize_account(i, materialize_slot)` MAY succeed only if the account is currently missing and materialized-account capacity remains below `MAX_MATERIALIZED_ACCOUNTS`.

On success, it MUST:

- increment `materialized_account_count`
- leave `neg_pnl_account_count` unchanged because the new account starts with `PNL_i = 0`
- set `C_i = 0`
- set `PNL_i = 0`
- set `R_i = 0`
- set canonical zero-position defaults
- set `fee_credits_i = 0`
- set `last_fee_slot_i = materialize_slot`
- leave both reserve buckets absent

### 2.8 Permissionless empty- or flat-dust-account reclamation

The engine MUST provide a permissionless reclamation path `reclaim_empty_account(i, now_slot)`.

It MAY succeed only if all of the following hold:

- account `i` is materialized
- trusted `now_slot >= current_slot`
- `0 <= C_i < cfg_min_initial_deposit`
- `PNL_i == 0`
- `R_i == 0`
- both reserve buckets are absent
- `basis_pos_q_i == 0`
- `fee_credits_i <= 0`

On success, it MUST:

- if `C_i > 0`:
  - `dust = C_i`
  - `set_capital(i, 0)`
  - `I = checked_add_u128(I, dust)`
- forgive any negative `fee_credits_i`
- reset local fields to canonical zero
- set `last_fee_slot_i = 0`
- mark the slot missing or reusable
- decrement `materialized_account_count`
- require `neg_pnl_account_count` is unchanged (the reclaim precondition already requires `PNL_i == 0`)

### 2.9 Initial market state

At market initialization, the engine MUST set:

- `V = 0`
- `I = 0`
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
- `resolved_live_price = 0`
- `resolved_slot = init_slot`
- `resolved_k_long_terminal_delta = 0`
- `resolved_k_short_terminal_delta = 0`
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
3. require `epoch_side != u64::MAX`, then increment `epoch_side` by exactly `1` using checked arithmetic
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
- `mode_s == ResetPending` and `epoch_snap_i + 1 == epoch_s`

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

Reserved fresh positive PnL increases `PNL_pos_tot` immediately but MUST NOT increase `PNL_matured_pos_tot` until warmup release or explicit touch-time acceleration.

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
- `Eq_trade_raw_i` is informational only in this revision
- strict risk-reducing comparisons MUST use exact widened `Eq_maint_raw_i`, never a clamped net quantity

---

## 4. Canonical helpers

### 4.1 `set_capital(i, new_C)`

When changing `C_i`, the engine MUST update `C_tot` by the exact signed delta and then set `C_i = new_C`.

### 4.2 `set_position_basis_q(i, new_basis_pos_q)`

When changing stored `basis_pos_q_i` from `old` to `new`, the engine MUST update `stored_pos_count_long` and `stored_pos_count_short` exactly once using the sign flags of `old` and `new`, then write `basis_pos_q_i = new`.

Any transition that increments a side-count — including `0 -> nonzero` and sign flips — MUST enforce `cfg_max_active_positions_per_side`.

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

### 4.4 `append_new_reserve(i, reserve_add, admitted_h_eff)`

Preconditions:

- `reserve_add > 0`
- `market_mode == Live`
- `admitted_h_eff > 0`
- `cfg_h_min <= admitted_h_eff <= cfg_h_max`
- `current_slot` is already the trusted slot anchor for the current instruction state

Effects:

1. if the scheduled bucket is absent and the pending bucket is present, call `promote_pending_to_scheduled(i)`
2. if the scheduled bucket is absent:
   - create a scheduled bucket with:
     - `sched_remaining_q = reserve_add`
     - `sched_anchor_q = reserve_add`
     - `sched_start_slot = current_slot`
     - `sched_horizon = admitted_h_eff`
     - `sched_release_q = 0`
3. else if the scheduled bucket is present, the pending bucket is absent, and all of the following hold:
   - `sched_start_slot == current_slot`
   - `sched_horizon == admitted_h_eff`
   - `sched_release_q == 0`
   then exact same-slot merge into the scheduled bucket is permitted:
   - `sched_remaining_q += reserve_add`
   - `sched_anchor_q += reserve_add`
4. else if the pending bucket is absent:
   - create a pending bucket with:
     - `pending_remaining_q = reserve_add`
     - `pending_horizon = admitted_h_eff`
5. else:
   - `pending_remaining_q += reserve_add`
   - `pending_horizon = max(pending_horizon, admitted_h_eff)`
6. set `R_i += reserve_add`

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

### 4.6.1 `sync_account_fee_to_slot(i, fee_slot_anchor, fee_rate_per_slot)`

This helper supports exact wrapper-owned recurring fee realization without global scans.

Preconditions:

- account `i` is materialized
- `fee_rate_per_slot >= 0`
- `fee_slot_anchor >= last_fee_slot_i`
- if `market_mode == Live`, `fee_slot_anchor <= current_slot`
- if `market_mode == Resolved`, `fee_slot_anchor <= resolved_slot`

Procedure:

1. `dt = fee_slot_anchor - last_fee_slot_i`
2. if `dt == 0`, return
3. define `fee_abs` by the exact capped-product law:
   - if `fee_rate_per_slot == 0`, set `fee_abs = 0`
   - else if the implementation computes in a widened domain, compute `fee_abs_raw = fee_rate_per_slot * dt` exactly and set `fee_abs = min(fee_abs_raw, MAX_PROTOCOL_FEE_ABS)`
   - else it MUST use the exactly equivalent branch law:
     - if `fee_rate_per_slot > floor(MAX_PROTOCOL_FEE_ABS / dt)`, set `fee_abs = MAX_PROTOCOL_FEE_ABS`
     - else set `fee_abs = fee_rate_per_slot * dt`
4. route `fee_abs` through `charge_fee_to_insurance(i, fee_abs)`
5. set `last_fee_slot_i = fee_slot_anchor`

Normative consequences:

- recurring fees are charged exactly once over `[old_last_fee_slot_i, fee_slot_anchor]`
- double-sync at the same anchor is a no-op
- zero-fee sync still advances the checkpoint to `fee_slot_anchor`
- a newly materialized account starts with `last_fee_slot_i = materialize_slot`, so it never inherits earlier recurring fees
- on resolved markets this helper syncs at most through `resolved_slot`; no recurring fee accrues after resolution
- any tail above `MAX_PROTOCOL_FEE_ABS` is intentionally dropped for liveness rather than blocking progress
- this helper MUST NOT fail solely because the uncapped raw product would exceed native `u128`

### 4.7 `admit_fresh_reserve_h_lock(i, fresh_positive_pnl_i, ctx, admit_h_min, admit_h_max) -> admitted_h_eff`

Preconditions:

- `market_mode == Live`
- account `i` is materialized
- `fresh_positive_pnl_i > 0`
- `0 <= admit_h_min <= admit_h_max <= cfg_h_max`
- `admit_h_max > 0`
- if `admit_h_min > 0`, then `admit_h_min >= cfg_h_min`
- `admit_h_max >= cfg_h_min`

Definitions:

- `senior_sum = checked_add_u128(C_tot, I)`
- `Residual_now = max(0, V - senior_sum)`
- `matured_plus_fresh = checked_add_u128(PNL_matured_pos_tot, fresh_positive_pnl_i)`

Admission law:

1. if account `i` is present in `ctx.h_max_sticky_accounts[]`, return `admit_h_max`
2. else:
   - if `matured_plus_fresh <= Residual_now`, set `admitted_h_eff = admit_h_min`
   - else set `admitted_h_eff = admit_h_max`
3. if `admitted_h_eff == admit_h_max`, insert account `i` into `ctx.h_max_sticky_accounts[]`
4. return `admitted_h_eff`

Normative consequences:

- live positive PnL cannot bypass admission
- if `admit_h_min == 0`, immediate release is allowed only when current state admits it
- if `admit_h_min > 0`, the fastest admitted live path is that positive minimum horizon
- once an account requires `admit_h_max` in one instruction, later fresh positive increments on that same account in that instruction MUST also use `admit_h_max`
- an earlier newest pending increment that was admitted at `admit_h_min` MAY later be conservatively lifted to `admit_h_max` if a later same-instruction increment on the same account requires `admit_h_max` and both share one pending bucket
- this conservative lift may only affect the newest pending bucket; it MUST never rewrite an already-scheduled bucket

### 4.8 `set_pnl(i, new_PNL, reserve_mode[, ctx])`

`reserve_mode ∈ {UseAdmissionPair(admit_h_min, admit_h_max), ImmediateReleaseResolvedOnly, NoPositiveIncreaseAllowed}`.

Every persistent mutation of `PNL_i` after materialization that may change its sign across zero MUST go through this helper. The optional `ctx` argument is required only when `reserve_mode == UseAdmissionPair(...)`; it is ignored or may be omitted on other modes. The sole direct-mutation exception in this revision is `consume_released_pnl(i, x)` in §4.10, whose preconditions guarantee that `PNL_i` remains non-negative and `neg_pnl_account_count` is unchanged.

Let:

- `old_pos = max(PNL_i, 0)`
- if `market_mode == Resolved`, require `R_i == 0`
- `new_pos = max(new_PNL, 0)`
- `old_neg = (PNL_i < 0)`
- `new_neg = (new_PNL < 0)`

Procedure:

All steps of this helper are part of one atomic top-level instruction effect under §0. If any later checked step fails, all earlier writes performed by this helper — including any mutation to `PNL_i`, `PNL_pos_tot`, `PNL_matured_pos_tot`, `neg_pnl_account_count`, `R_i`, the scheduled bucket, or the pending bucket — MUST roll back atomically with the enclosing instruction.

1. require `new_PNL != i128::MIN`
2. if `market_mode == Live`, require `new_pos <= MAX_ACCOUNT_POSITIVE_PNL_LIVE`
3. if `market_mode == Resolved`, require `new_pos <= i128::MAX as u128`
4. compute `PNL_pos_tot_after` by applying the exact delta from `old_pos` to `new_pos` in checked arithmetic
5. if `market_mode == Live`, require `PNL_pos_tot_after <= MAX_PNL_POS_TOT_LIVE`

If `new_pos > old_pos`:

6. `reserve_add = new_pos - old_pos`
7. if `reserve_mode == NoPositiveIncreaseAllowed`, fail conservatively before any persistent mutation
8. if `reserve_mode == ImmediateReleaseResolvedOnly` and `market_mode == Live`, fail conservatively before any persistent mutation
9. if `reserve_mode == ImmediateReleaseResolvedOnly`:
   - require `market_mode == Resolved`
   - set `PNL_pos_tot = PNL_pos_tot_after`
   - set `PNL_i = new_PNL` and update `neg_pnl_account_count` exactly once if sign crosses zero
   - add `reserve_add` to `PNL_matured_pos_tot`
   - require `PNL_matured_pos_tot <= PNL_pos_tot`
   - return
10. if `reserve_mode == UseAdmissionPair(admit_h_min, admit_h_max)`:
   - require `market_mode == Live`
   - `admitted_h_eff = admit_fresh_reserve_h_lock(i, reserve_add, ctx, admit_h_min, admit_h_max)`
   - set `PNL_pos_tot = PNL_pos_tot_after`
   - set `PNL_i = new_PNL` and update `neg_pnl_account_count` exactly once if sign crosses zero
   - if `admitted_h_eff == 0`:
     - add `reserve_add` to `PNL_matured_pos_tot`
   - else:
     - call `append_new_reserve(i, reserve_add, admitted_h_eff)`
   - require `R_i <= max(PNL_i, 0)` and `PNL_matured_pos_tot <= PNL_pos_tot`
   - return

If `new_pos <= old_pos`:

11. `pos_loss = old_pos - new_pos`
12. if `market_mode == Live`:
   - `reserve_loss = min(pos_loss, R_i)`
   - if `reserve_loss > 0`, call `apply_reserve_loss_newest_first(i, reserve_loss)`
   - `matured_loss = pos_loss - reserve_loss`
13. if `market_mode == Resolved`:
   - require `R_i == 0`
   - `matured_loss = pos_loss`
14. if `matured_loss > 0`, subtract `matured_loss` from `PNL_matured_pos_tot`
15. set `PNL_pos_tot = PNL_pos_tot_after`
16. set `PNL_i = new_PNL` and update `neg_pnl_account_count` exactly once if sign crosses zero
17. if `new_pos == 0` and `market_mode == Live`, require `R_i == 0` and both buckets absent
18. require `R_i <= max(PNL_i, 0)` and `PNL_matured_pos_tot <= PNL_pos_tot`

### 4.9 `admit_outstanding_reserve_on_touch(i)`

Preconditions:

- `market_mode == Live`
- account `i` is materialized

Definitions:

- `reserve_total = (sched_remaining_q_i if sched_present_i else 0) + (pending_remaining_q_i if pending_present_i else 0)`
- `senior_sum = checked_add_u128(C_tot, I)`
- `Residual_now = max(0, V - senior_sum)`
- `matured_plus_reserve = checked_add_u128(PNL_matured_pos_tot, reserve_total)`

Acceleration law:

1. if `reserve_total == 0`, return
2. if `matured_plus_reserve <= Residual_now`:
   - increase `PNL_matured_pos_tot` by `reserve_total`
   - clear both buckets
   - set `R_i = 0`
   - require `PNL_matured_pos_tot <= PNL_pos_tot`
   - require `R_i <= max(PNL_i, 0)`
   - return
3. else return

Normative consequences:

- acceleration never extends a horizon; it only removes reserve when current state safely admits immediate release
- acceleration is monotone: a bucket accelerated once cannot un-accelerate
- acceleration preserves goals 6 and 7: reserve is removed, not reset
- acceleration cannot be griefed: a third party cannot force non-acceleration, and acceleration is strictly more favorable to the user than non-acceleration

### 4.10 `consume_released_pnl(i, x)`

This helper removes only matured released positive PnL on a live account and MUST leave both reserve buckets unchanged.

Preconditions:

- `market_mode == Live`
- `0 < x <= ReleasedPos_i`

Effects:

1. decrease `PNL_i` by exactly `x`
2. decrease `PNL_pos_tot` by exactly `x`
3. decrease `PNL_matured_pos_tot` by exactly `x`
4. leave `neg_pnl_account_count` unchanged because the precondition guarantees the account remains non-negative after the write
5. leave `R_i`, the scheduled bucket, and the pending bucket unchanged
6. require `PNL_matured_pos_tot <= PNL_pos_tot`

### 4.11 `advance_profit_warmup(i)`

Preconditions:

- `market_mode == Live`

Procedure:

1. if `R_i == 0`, require both buckets absent and return
2. if the scheduled bucket is absent and the pending bucket is present, call `promote_pending_to_scheduled(i)`
3. if the scheduled bucket is still absent, return
4. let `elapsed = current_slot - sched_start_slot`
5. let `effective_elapsed = min(elapsed, sched_horizon)`
6. let `sched_total = mul_div_floor_u128(sched_anchor_q, effective_elapsed as u128, sched_horizon as u128)`
7. require `sched_total >= sched_release_q`
8. `sched_increment = sched_total - sched_release_q`
9. `release = min(sched_remaining_q, sched_increment)`
10. if `release > 0`:
   - `sched_remaining_q -= release`
   - `R_i -= release`
   - `PNL_matured_pos_tot += release`
11. if the scheduled bucket is now empty:
   - clear it completely, including `sched_release_q = 0`
   - if the pending bucket is present, call `promote_pending_to_scheduled(i)`
12. else:
   - set `sched_release_q = sched_total`
13. if `R_i == 0`, require both buckets absent
14. require `PNL_matured_pos_tot <= PNL_pos_tot`

This formulation makes explicit the intended law: if loss consumption made `release < sched_increment`, that can only happen because the scheduled bucket emptied in this call, so no persistent over-advanced `sched_release_q` remains on a non-empty bucket.

### 4.12 `attach_effective_position(i, new_eff_pos_q)`

This helper converts a current effective quantity into a new position basis at the current side state.

If discarding a same-epoch nonzero basis, it MUST first compute whether the old same-epoch effective quantity had a nonzero fractional orphan remainder. Concretely, let `old_basis = basis_pos_q_i`, `s = side(old_basis)`, `A_s_current = A_s`, and `a_basis_old = a_basis_i`. If `old_basis != 0`, `epoch_snap_i == epoch_s`, and `a_basis_old > 0`, compute `orphan_rem = (abs(old_basis) * A_s_current) mod a_basis_old` in exact wide arithmetic. If `orphan_rem != 0`, it MUST call `inc_phantom_dust_bound(s)`, i.e. increment the appropriate phantom-dust bound by exactly `1` q-unit, before overwriting the basis. This spec intentionally chooses the one-q-unit conservative bound for basis-replacement orphan remainder; implementations MUST NOT silently choose a different increment law.

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

### 4.13 Phantom-dust helpers

- `inc_phantom_dust_bound(side)` increments by exactly `1` q-unit.
- `inc_phantom_dust_bound_by(side, amount_q)` increments by exactly `amount_q`.

### 4.14 `max_safe_flat_conversion_released(i, x_cap, h_num, h_den)`

This helper returns the largest `x_safe <= x_cap` such that converting `x_safe` released profit on a live flat account cannot make the account’s exact post-conversion raw maintenance equity negative.

Implementation law:

1. if `x_cap == 0`, return `0`
2. let `E_before = Eq_maint_raw_i` on the current exact state
3. if `E_before <= 0`, return `0`
4. if `h_den == 0` or `h_num == h_den`, return `x_cap`
5. let `haircut_loss_num = h_den - h_num`
6. return `min(x_cap, floor(E_before * h_den / haircut_loss_num))` using an exact capped multiply-divide with at least 256-bit intermediates, or an equivalent exact wide comparison

### 4.15 `compute_trade_pnl(size_q, oracle_price, exec_price)`

For a bilateral trade where `size_q > 0` means account `a` buys base from account `b`, the execution-slippage PnL applied before fees MUST be:

- `trade_pnl_num = size_q * (oracle_price - exec_price)`
- `trade_pnl_a = floor_div_signed_conservative(trade_pnl_num, POS_SCALE)`
- `trade_pnl_b = -trade_pnl_a`

This helper MUST use checked signed arithmetic and exact conservative floor division.

### 4.16 `charge_fee_to_insurance(i, fee_abs) -> FeeChargeOutcome`

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

### 4.17 Insurance-loss helpers

- `use_insurance_buffer(loss_abs)` spends insurance down to `cfg_insurance_floor` and returns the remainder.
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

### 5.3 `settle_side_effects_live(i, ctx)`

When touching account `i` on a live market:

1. if `basis_pos_q_i == 0`, return
2. let `s = side(basis_pos_q_i)`
3. let `den = checked_mul_u128(a_basis_i, POS_SCALE)`
4. if `epoch_snap_i == epoch_s`:
   - `q_eff_new = mul_div_floor_u128(abs(basis_pos_q_i), A_s, a_basis_i)`
   - `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i), k_snap_i, K_s, f_snap_i, F_s_num, den)`
   - `set_pnl(i, PNL_i + pnl_delta, UseAdmissionPair(ctx.admit_h_min_shared, ctx.admit_h_max_shared), ctx)`
   - if `q_eff_new == 0`:
     - call `inc_phantom_dust_bound(s)`, i.e. increment the appropriate phantom-dust bound by exactly `1` q-unit (the remaining same-epoch quantity is strictly between `0` and `1` q-unit)
     - zero the basis
     - reset snapshots to canonical zero-position defaults
   - else:
     - update `k_snap_i`
     - update `f_snap_i`
     - update `epoch_snap_i`
5. else:
   - require `mode_s == ResetPending`
   - require `epoch_snap_i + 1 == epoch_s`
   - require `stale_account_count_s > 0`
   - `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i), k_snap_i, K_epoch_start_s, f_snap_i, F_epoch_start_s_num, den)`
   - `set_pnl(i, PNL_i + pnl_delta, UseAdmissionPair(ctx.admit_h_min_shared, ctx.admit_h_max_shared), ctx)`
   - zero the basis
   - decrement `stale_account_count_s`
   - reset snapshots

### 5.4 `settle_side_effects_resolved(i)`

When touching account `i` on a resolved market:

Preconditions:

- `market_mode == Resolved`
- `prepare_account_for_resolved_touch(i)` has already executed in the current top-level instruction, equivalently `R_i == 0` and both reserve buckets are absent

Procedure:

1. if `basis_pos_q_i == 0`, return
2. let `s = side(basis_pos_q_i)`
3. require stale one-epoch-lag conditions on its side
4. require `stale_account_count_s > 0`
5. let `den = checked_mul_u128(a_basis_i, POS_SCALE)`
6. let `resolved_k_terminal_delta_s` denote `resolved_k_long_terminal_delta` on the long side and `resolved_k_short_terminal_delta` on the short side
7. let `k_terminal_s_exact = (K_epoch_start_s as wide_signed) + (resolved_k_terminal_delta_s as wide_signed)`
8. let `f_terminal_s_exact = F_epoch_start_s_num`
9. compute `pnl_delta = wide_signed_mul_div_floor_from_kf_pair(abs(basis_pos_q_i), k_snap_i, k_terminal_s_exact, f_snap_i, f_terminal_s_exact, den)`
10. `set_pnl(i, PNL_i + pnl_delta, ImmediateReleaseResolvedOnly)`
11. zero the basis
12. decrement `stale_account_count_s`
13. reset snapshots

### 5.5 `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`

Before any live operation that depends on current market state, the engine MUST call `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)`.

This helper MUST:

1. require `market_mode == Live`
2. require trusted `now_slot >= slot_last`
3. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
4. require `abs(funding_rate_e9_per_slot) <= cfg_max_abs_funding_e9_per_slot`
5. let `dt = now_slot - slot_last`
6. require `dt <= cfg_max_accrual_dt_slots`
7. snapshot `OI_long_0 = OI_eff_long`, `OI_short_0 = OI_eff_short`, and `fund_px_0 = fund_px_last`
8. mark-to-market once:
   - `ΔP = oracle_price - P_last`
   - if `OI_long_0 > 0`, compute `delta_k_long = A_long * ΔP` in an exact wide signed domain; if the resulting persistent `K_long` would overflow `i128`, fail conservatively; else apply it
   - if `OI_short_0 > 0`, compute `delta_k_short = -A_short * ΔP` in an exact wide signed domain; if the resulting persistent `K_short` would overflow `i128`, fail conservatively; else apply it
9. funding transfer:
   - if `funding_rate_e9_per_slot != 0` and `dt > 0` and both snapped OI sides are nonzero:
     - compute `fund_num_total = fund_px_0 * funding_rate_e9_per_slot * dt` in an exact wide signed domain of at least 256 bits, or a formally equivalent exact method
     - compute each `A_side * fund_num_total` product in the same exact wide signed domain, or a formally equivalent exact method
     - if the resulting persistent `F_long_num` or `F_short_num` would overflow `i128`, fail conservatively
     - else apply both updates exactly:
       - `F_long_num -= A_long * fund_num_total`
       - `F_short_num += A_short * fund_num_total`
10. update `slot_last = now_slot`
11. update `P_last = oracle_price`
12. update `fund_px_last = oracle_price`

Because this helper is only defined as part of a top-level atomic instruction under §0, any overflow or conservative failure in a later leg of the helper or later instruction logic MUST roll back any earlier tentative `K_side`, `F_side_num`, `P_last`, or `fund_px_last` writes from the same top-level call.

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

Insurance-first ordering in this helper is intentional. Bankruptcy deficit is senior to junior PnL and therefore hits available insurance before the engine determines whether any residual quote loss can also be represented through opposing-side `K` updates. Zero-OI and zero-stored-position-count branches may therefore consume insurance and still route the remaining deficit through `record_uninsured_protocol_loss`.

`OI_eff_side` is the authoritative side-level aggregate tracker used by later global state transitions. Because account-level effective positions are individually floored, the sum of per-account same-epoch floor quantities on a side need not equal `OI_eff_side` after `A_side` decay. Any such mismatch MUST be treated only as bounded phantom dust tracked by `phantom_dust_bound_*_q` and reconciled only through §5.7 end-of-instruction dust clearance and reset rules.

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

Late fee realization from `C_i` to `I` does **not** change `Residual = V - (C_tot + I)` and therefore does not invalidate a previously captured resolved payout snapshot.

### 6.5 `touch_account_live_local(i, ctx)`

This is the canonical live local touch.

Procedure:

1. require `market_mode == Live`
2. require account `i` is materialized
3. add `i` to `ctx.touched_accounts[]` if not already present
4. `admit_outstanding_reserve_on_touch(i)`
5. `advance_profit_warmup(i)`
6. `settle_side_effects_live(i, ctx)`
7. `settle_losses_from_principal(i)`
8. if `effective_pos_q(i) == 0` and `PNL_i < 0`, resolve uncovered flat loss
9. MUST NOT auto-convert
10. MUST NOT call `fee_debt_sweep(i)`

If the deployment enables wrapper-owned recurring account fees, the wrapper MUST sync the account’s recurring fee to the relevant live slot anchor **before** relying on any health-sensitive result of this touched state.

### 6.6 `finalize_touched_accounts_post_live(ctx)`

This helper is mandatory for every live instruction that uses `touch_account_live_local`.

Procedure:

1. compute one shared post-live conversion snapshot:
   - `Residual_snapshot = max(0, V - (C_tot + I))`
   - `PNL_matured_pos_tot_snapshot = PNL_matured_pos_tot`
   - if `PNL_matured_pos_tot_snapshot == 0`, define `whole_snapshot = false`
   - else:
     - `h_snapshot_num = min(Residual_snapshot, PNL_matured_pos_tot_snapshot)`
     - `h_snapshot_den = PNL_matured_pos_tot_snapshot`
     - `whole_snapshot = (h_snapshot_num == h_snapshot_den)`
2. iterate `ctx.touched_accounts[]` in deterministic ascending storage-index order:
   - if `basis_pos_q_i == 0`, `ReleasedPos_i > 0`, and `whole_snapshot == true`:
     - `released = ReleasedPos_i`
     - `consume_released_pnl(i, released)`
     - `set_capital(i, C_i + released)`
   - call `fee_debt_sweep(i)`

### 6.7 Resolved positive-payout readiness

Positive resolved payouts MUST NOT begin until the market is terminal-ready for positive claims.

A market is **positive-payout ready** only when all of the following hold:

- `stale_account_count_long == 0`
- `stale_account_count_short == 0`
- `stored_pos_count_long == 0`
- `stored_pos_count_short == 0`
- `neg_pnl_account_count == 0`

`neg_pnl_account_count` is therefore the exact O(1) readiness aggregate for remaining negative claims.

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

This snapshot is stable under later resolved fee sync because fee sync is a pure `C -> I` reclassification with `V` unchanged; it therefore preserves `V - (C_tot + I)`.

### 6.9 `force_close_resolved_terminal_nonpositive(i) -> payout`

This helper terminally closes a resolved account whose local claim is already non-positive and returns its terminal payout.

Preconditions:

- `market_mode == Resolved`
- account `i` is materialized
- `basis_pos_q_i == 0`
- `PNL_i <= 0`

Procedure:

1. if the deployment enables wrapper-owned recurring account fees and `last_fee_slot_i < resolved_slot`, sync recurring fee to `resolved_slot`
2. if `PNL_i < 0`, resolve uncovered flat loss via §6.3
3. call `fee_debt_sweep(i)`
4. forgive any remaining negative `fee_credits_i`
5. let `payout = C_i`
6. if `payout > 0`:
   - `set_capital(i, 0)`
   - `V = V - payout`
7. require `PNL_i == 0`, `R_i == 0`, both reserve buckets absent, `basis_pos_q_i == 0`, and `last_fee_slot_i <= resolved_slot`
8. reset local fields and free the slot
9. require `V >= C_tot + I`
10. return `payout`

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

1. if the deployment enables wrapper-owned recurring account fees and `last_fee_slot_i < resolved_slot`, sync recurring fee to `resolved_slot`
2. let `x = max(PNL_i, 0)`
3. let `y = floor(x * resolved_payout_h_num / resolved_payout_h_den)`
4. `set_pnl(i, 0, NoPositiveIncreaseAllowed)`
5. `set_capital(i, C_i + y)`
6. call `fee_debt_sweep(i)`
7. forgive any remaining negative `fee_credits_i`
8. let `payout = C_i`
9. if `payout > 0`:
   - `set_capital(i, 0)`
   - `V = V - payout`
10. require `PNL_i == 0`, `R_i == 0`, both reserve buckets absent, `basis_pos_q_i == 0`, and `last_fee_slot_i <= resolved_slot`
11. reset local fields and free the slot
12. require `V >= C_tot + I`
13. return `payout`

Impossible states — for example `resolved_payout_snapshot_ready == true` with `PNL_i > 0` but `resolved_payout_h_den == 0` — MUST fail conservatively rather than falling back to `y = x`.

---

## 7. Fees

This revision still has no engine-native recurring maintenance fee. The engine core defines native trading fees, native liquidation fees, and the canonical helpers for optional wrapper-owned account fees. The new `last_fee_slot_i` checkpoint exists so wrapper-owned recurring fees can be realized exactly on touched accounts.

### 7.1 Trading fees

Define:

- `fee = mul_div_ceil_u128(trade_notional, cfg_trading_fee_bps, 10_000)`

Rules:

- if `cfg_trading_fee_bps == 0` or `trade_notional == 0`, then `fee = 0`
- if `cfg_trading_fee_bps > 0` and `trade_notional > 0`, then `fee >= 1`

### 7.2 Liquidation fees

For a liquidation that closes `q_close_q` at `oracle_price`:

- if `q_close_q == 0`, `liq_fee = 0`
- else:
  - `closed_notional = mul_div_floor_u128(q_close_q, oracle_price, POS_SCALE)`
  - `liq_fee_raw = mul_div_ceil_u128(closed_notional, cfg_liquidation_fee_bps, 10_000)`
  - `liq_fee = min(max(liq_fee_raw, cfg_min_liquidation_abs), cfg_liquidation_fee_cap)`

### 7.3 Optional wrapper-owned account fees

A wrapper MAY impose additional account fees by routing an amount `fee_abs` through `charge_fee_to_insurance(i, fee_abs)`, provided `fee_abs <= MAX_PROTOCOL_FEE_ABS`.

If the wrapper wants a recurring time-based fee, it SHOULD do so through `sync_account_fee_to_slot(i, fee_slot_anchor, fee_rate_per_slot)` rather than by attempting to reconstruct elapsed time externally without a per-account checkpoint.

---

## 8. Margin checks and liquidation

### 8.1 Margin requirements

After live touch reconciliation, define:

- `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`

If `effective_pos_q(i) == 0`:

- `MM_req_i = 0`
- `IM_req_i = 0`

Else:

- `MM_req_i = max(mul_div_floor_u128(Notional_i, cfg_maintenance_bps, 10_000), cfg_min_nonzero_mm_req)`
- `IM_req_i = max(mul_div_floor_u128(Notional_i, cfg_initial_bps, 10_000), cfg_min_nonzero_im_req)`

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

If the deployment enables wrapper-owned recurring account fees, that touched state MUST be fee-current for the account before liquidatability is evaluated.

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

`(admit_h_min, admit_h_max)` and `funding_rate_e9_per_slot` are wrapper-owned logical inputs, not public caller-owned fields. Public or permissionless wrappers MUST derive them internally.

If the deployment enables wrapper-owned recurring account fees, any top-level instruction that depends on current account health or reclaimability MUST sync the relevant touched account(s) to the intended fee anchor before relying on health-sensitive or reclaim-sensitive results.

Unless explicitly noted otherwise, a live external state-mutating operation that depends on current market state executes in this order:

1. validate monotonic slot, oracle input, funding-rate bound, and admission-pair bound
2. initialize fresh `ctx` with `admit_h_min_shared = admit_h_min`, `admit_h_max_shared = admit_h_max`
3. call `accrue_market_to(now_slot, oracle_price, funding_rate_e9_per_slot)` exactly once
4. set `current_slot = now_slot`
5. if recurring account fees are enabled, sync the operation’s touched account set to `current_slot` before any health-sensitive check for those accounts
6. perform the endpoint’s exact current-state inner execution
7. call `finalize_touched_accounts_post_live(ctx)` exactly once
8. call `schedule_end_of_instruction_resets(ctx)` exactly once
9. call `finalize_end_of_instruction_resets(ctx)` exactly once
10. assert `OI_eff_long == OI_eff_short` at the end of every live top-level instruction that can mutate side state or live exposure
11. require `V >= C_tot + I`

### 9.1 `settle_account(i, oracle_price, now_slot, funding_rate_e9_per_slot, admit_h_min, admit_h_max[, fee_rate_per_slot])`

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market once
5. set `current_slot`
6. if recurring fees are enabled, `sync_account_fee_to_slot(i, current_slot, fee_rate_per_slot)`
7. `touch_account_live_local(i, ctx)`
8. `finalize_touched_accounts_post_live(ctx)`
9. schedule resets
10. finalize resets
11. assert `OI_eff_long == OI_eff_short`
12. require `V >= C_tot + I`

### 9.2 `deposit(i, amount, now_slot)`

`deposit` is pure capital transfer. It MUST NOT call `accrue_market_to`, MUST NOT mutate side state, and MUST NOT mutate reserve state.

Procedure:

1. require `market_mode == Live`
2. require `now_slot >= current_slot`
3. set `current_slot = now_slot`
4. if account `i` is missing:
   - require `amount >= cfg_min_initial_deposit`
   - materialize the account with `materialize_account(i, now_slot)`
5. require `V + amount <= MAX_VAULT_TVL`
6. set `V = V + amount`
7. `set_capital(i, C_i + amount)`
8. `settle_losses_from_principal(i)`
9. MUST NOT invoke flat-loss insurance absorption
10. if `basis_pos_q_i == 0` and `PNL_i >= 0`, call `fee_debt_sweep(i)`
11. require `V >= C_tot + I`

> **Live accrual envelope (applies to §9.2.1 – §9.2.4).**
> Public Live-mode instructions that advance `current_slot` but do NOT call
> `accrue_market_to` (i.e., do not advance `last_market_slot`) MUST also
> require `now_slot <= last_market_slot + cfg_max_accrual_dt_slots`.
> Without this bound, a permissionless caller could pick any `now_slot`
> beyond the envelope, commit the `current_slot` advance, and permanently
> brick subsequent live accrual: every later `accrue_market_to(n, ..)`
> with `n >= current_slot` would fail because
> `n - last_market_slot > cfg_max_accrual_dt_slots`, and monotonicity
> forbids smaller `n`. Callers wanting to advance time beyond the
> envelope MUST go through `accrue_market_to`, which also advances
> `last_market_slot`.

### 9.2.1 `deposit_fee_credits(i, amount, now_slot)`

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
3a. require `now_slot <= last_market_slot + cfg_max_accrual_dt_slots`
4. set `current_slot = now_slot`
5. `pay = min(amount, FeeDebt_i)`
6. if `pay == 0`, return
7. require `V + pay <= MAX_VAULT_TVL`
8. set `V = V + pay`
9. set `I = I + pay`
10. add `pay` to `fee_credits_i`
11. require `fee_credits_i <= 0`
12. require `V >= C_tot + I`

### 9.2.2 `top_up_insurance_fund(amount, now_slot)`

1. require `market_mode == Live`
2. require `now_slot >= current_slot`
2a. require `now_slot <= last_market_slot + cfg_max_accrual_dt_slots`
3. set `current_slot = now_slot`
4. require `V + amount <= MAX_VAULT_TVL`
5. set `V = V + amount`
6. set `I = I + amount`
7. require `V >= C_tot + I`

### 9.2.3 `charge_account_fee(i, fee_abs, now_slot)`

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
3a. require `now_slot <= last_market_slot + cfg_max_accrual_dt_slots`
4. require `fee_abs <= MAX_PROTOCOL_FEE_ABS`
5. set `current_slot = now_slot`
6. `charge_fee_to_insurance(i, fee_abs)`
7. require `V >= C_tot + I`

### 9.2.4 `settle_flat_negative_pnl(i, now_slot[, fee_rate_per_slot])`

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
3a. require `now_slot <= last_market_slot + cfg_max_accrual_dt_slots`
4. set `current_slot = now_slot`
5. if recurring fees are enabled, `sync_account_fee_to_slot(i, current_slot, fee_rate_per_slot)`
6. require `basis_pos_q_i == 0`
7. require `R_i == 0` and both reserve buckets absent
8. if `PNL_i >= 0`, return
9. settle losses from principal
10. if `PNL_i < 0`, absorb protocol loss and set `PNL_i = 0`
11. require `PNL_i == 0`
12. require `V >= C_tot + I`

### 9.3 `withdraw(i, amount, oracle_price, now_slot, funding_rate_e9_per_slot, admit_h_min, admit_h_max[, fee_rate_per_slot])`

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market
5. set `current_slot`
6. if recurring fees are enabled, `sync_account_fee_to_slot(i, current_slot, fee_rate_per_slot)`
7. `touch_account_live_local(i, ctx)`
8. `finalize_touched_accounts_post_live(ctx)`
9. require `amount <= C_i`
10. require post-withdraw capital is either `0` or `>= cfg_min_initial_deposit`
11. if `effective_pos_q(i) != 0`, require withdrawal health on the hypothetical post-withdraw state where both `V` and `C_tot` decrease by `amount`
12. apply `set_capital(i, C_i - amount)` and `V = V - amount`
13. schedule resets
14. finalize resets
15. assert `OI_eff_long == OI_eff_short`
16. require `V >= C_tot + I`

### 9.3.1 `convert_released_pnl(i, x_req, oracle_price, now_slot, funding_rate_e9_per_slot, admit_h_min, admit_h_max[, fee_rate_per_slot])`

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market
5. set `current_slot`
6. if recurring fees are enabled, `sync_account_fee_to_slot(i, current_slot, fee_rate_per_slot)`
7. `touch_account_live_local(i, ctx)`
8. require `0 < x_req <= ReleasedPos_i`
9. compute current `h`
10. if `basis_pos_q_i == 0`, require `x_req <= max_safe_flat_conversion_released(i, x_req, h_num, h_den)`
11. `consume_released_pnl(i, x_req)`
12. `set_capital(i, C_i + floor(x_req * h_num / h_den))`
13. call `fee_debt_sweep(i)`
14. if `effective_pos_q(i) != 0`, require the post-conversion state is maintenance healthy
15. `finalize_touched_accounts_post_live(ctx)`
16. schedule resets
17. finalize resets
18. assert `OI_eff_long == OI_eff_short`
19. require `V >= C_tot + I`

### 9.4 `execute_trade(a, b, oracle_price, now_slot, funding_rate_e9_per_slot, admit_h_min, admit_h_max, size_q, exec_price[, fee_rate_per_slot_a, fee_rate_per_slot_b])`

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
10. if recurring fees are enabled, sync `a` and `b` to `current_slot`
11. touch both accounts locally
12. capture pre-trade effective positions, maintenance requirements, and exact widened raw maintenance buffers
13. finalize any already-ready reset sides before OI increase
14. compute candidate post-trade effective positions
15. require position bounds
16. compute exact bilateral candidate OI after-values
17. enforce `MAX_OI_SIDE_Q`
18. reject any trade that would increase OI on a blocked side
19. compute `trade_pnl_a` and `trade_pnl_b` via `compute_trade_pnl(size_q, oracle_price, exec_price)` and apply execution-slippage PnL before fees:
   - `set_pnl(a, PNL_a + trade_pnl_a, UseAdmissionPair(admit_h_min, admit_h_max), ctx)`
   - `set_pnl(b, PNL_b + trade_pnl_b, UseAdmissionPair(admit_h_min, admit_h_max), ctx)`
20. attach the resulting effective positions
21. write the exact candidate OI after-values
22. settle post-trade losses from principal for both accounts
23. if a resulting effective position is zero, require `PNL_i >= 0` before fees
24. compute and charge explicit trading fees, capturing `fee_equity_impact_a` and `fee_equity_impact_b`
25. compute post-trade `Notional_post_i`, `IM_req_post_i`, `MM_req_post_i`, and `Eq_trade_open_raw_i`
26. enforce post-trade approval independently for both accounts:
   - if resulting effective position is zero, require exact `min(Eq_maint_raw_post_i + fee_equity_impact_i, 0) >= min(Eq_maint_raw_pre_i, 0)`
   - else if risk-increasing, require exact `Eq_trade_open_raw_i >= IM_req_post_i`
   - else if exact maintenance health already holds, allow
   - else if strictly risk-reducing, allow only if both:
     - `((Eq_maint_raw_post_i + fee_equity_impact_i) - MM_req_post_i) > (Eq_maint_raw_pre_i - MM_req_pre_i)`
     - `min(Eq_maint_raw_post_i + fee_equity_impact_i, 0) >= min(Eq_maint_raw_pre_i, 0)`
   - else reject
27. `finalize_touched_accounts_post_live(ctx)`
28. schedule resets
29. finalize resets
30. assert `OI_eff_long == OI_eff_short`
31. require `V >= C_tot + I`

### 9.5 `close_account(i, oracle_price, now_slot, funding_rate_e9_per_slot, admit_h_min, admit_h_max[, fee_rate_per_slot]) -> payout`

Owner-facing close path for a clean live account.

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market
5. set `current_slot`
6. if recurring fees are enabled, `sync_account_fee_to_slot(i, current_slot, fee_rate_per_slot)`
7. `touch_account_live_local(i, ctx)`
8. `finalize_touched_accounts_post_live(ctx)`
9. require `basis_pos_q_i == 0`
10. require `PNL_i == 0`
11. require `R_i == 0` and both reserve buckets absent
12. require `FeeDebt_i == 0`
13. let `payout = C_i`
14. if `payout > 0`:
    - `set_capital(i, 0)`
    - `V = V - payout`
15. free the slot
16. schedule resets
17. finalize resets
18. assert `OI_eff_long == OI_eff_short`
19. require `V >= C_tot + I`
20. return `payout`

### 9.6 `liquidate(i, oracle_price, now_slot, funding_rate_e9_per_slot, admit_h_min, admit_h_max, policy[, fee_rate_per_slot])`

`policy ∈ {FullClose, ExactPartial(q_close_q)}`.

1. require `market_mode == Live`
2. require account `i` is materialized
3. initialize `ctx`
4. accrue market
5. set `current_slot`
6. if recurring fees are enabled, `sync_account_fee_to_slot(i, current_slot, fee_rate_per_slot)`
7. touch the account locally
8. require liquidation eligibility
9. execute either exact partial liquidation or full-close liquidation on the already-touched state
10. `finalize_touched_accounts_post_live(ctx)`
11. schedule resets
12. finalize resets
13. assert `OI_eff_long == OI_eff_short`
14. require `V >= C_tot + I`

### 9.7 `keeper_crank(now_slot, oracle_price, funding_rate_e9_per_slot, admit_h_min, admit_h_max, ordered_candidates[], max_revalidations[, fee_rate_per_slot_fn])`

`ordered_candidates[]` is keeper-supplied and untrusted. It MAY be empty; an empty call is a valid “accrue-only plus finalize” instruction.

1. require `market_mode == Live`
2. initialize `ctx`
3. validate slot and oracle
4. accrue market exactly once
5. set `current_slot = now_slot`
6. iterate candidates in keeper-supplied order until budget exhausted or a pending reset is scheduled:
   - stopping at the first scheduled reset is intentional; once reset work is pending, further live-OI-dependent candidate processing belongs to a later instruction after reset finalization
   - in this loop, “a pending reset is scheduled” means `ctx.pending_reset_long || ctx.pending_reset_short`
   - missing-account skips do not count
   - touching a materialized account counts against `max_revalidations`
   - if recurring fees are enabled, sync the candidate to `current_slot`
   - `touch_account_live_local(candidate, ctx)`
   - if the account is liquidatable after touch and a current-state-valid liquidation-policy hint is present, execute liquidation on the already-touched state
   - if the account is flat, clean, empty, or dust after that touched state, the wrapper MAY instead or additionally invoke the separate reclaim path in a later instruction
   - after each candidate’s touch/liquidation attempt, if `ctx.pending_reset_long || ctx.pending_reset_short`, break before processing the next candidate
7. `finalize_touched_accounts_post_live(ctx)`
8. schedule resets
9. finalize resets
10. assert `OI_eff_long == OI_eff_short`
11. require `V >= C_tot + I`

Candidate order in this instruction is **keeper policy**, not an engine-level fairness guarantee. Different valid candidate orders can change which accounts receive faster or slower reserve admission or touch-time acceleration in that instruction. This affects only user-side warmup timing and operational UX, never solvency, conservation, or correctness. Deployments that require deterministic UX SHOULD canonicalize candidates by ascending storage index after their own off-chain risk bucketing.

### 9.8 `resolve_market(resolve_mode, resolved_price, live_oracle_price, now_slot, funding_rate_e9_per_slot)`

Privileged deployment-owned transition.

`resolve_mode ∈ {Ordinary, Degenerate}` is a trusted wrapper-controlled selector. Value-detected branch selection is forbidden.

This instruction has two privileged branches:

- **ordinary self-synchronizing resolution**, which first accrues the live market state to `now_slot` using the trusted current live oracle price and the wrapper-owned current funding rate, then stores the final settlement mark as separate resolved terminal `K` deltas; and
- **degenerate recovery resolution**, which is available only when the wrapper explicitly selects it and explicitly supplies degenerate live-sync inputs (`live_oracle_price = P_last` and `funding_rate_e9_per_slot = 0`), in which case the instruction resolves directly from the last synchronized live mark and intentionally applies no additional live accrual after `slot_last`.

Procedure:

1. require `market_mode == Live`
2. require `now_slot >= current_slot` and `now_slot >= slot_last`
3. require validated `0 < live_oracle_price <= MAX_ORACLE_PRICE`
4. require validated `0 < resolved_price <= MAX_ORACLE_PRICE`
5. if `resolve_mode == Degenerate`:
   - require `live_oracle_price == P_last`
   - require `funding_rate_e9_per_slot == 0`
   - set `current_slot = now_slot`
   - set `slot_last = now_slot`
   - set `resolved_live_price_candidate = P_last`
   - set `used_degenerate_resolution_branch = true`
6. else if `resolve_mode == Ordinary`:
   - require `now_slot - slot_last <= cfg_max_accrual_dt_slots`
   - call `accrue_market_to(now_slot, live_oracle_price, funding_rate_e9_per_slot)`
   - set `current_slot = now_slot`
   - set `resolved_live_price_candidate = live_oracle_price`
   - set `used_degenerate_resolution_branch = false`
7. value-based ambiguity is forbidden: if the wrapper wants the ordinary branch, it MUST pass `resolve_mode = Ordinary`, even when `live_oracle_price == P_last` and `funding_rate_e9_per_slot == 0`
8. if `used_degenerate_resolution_branch == false`:
   - require exact settlement-band check:
     - `abs(resolved_price - resolved_live_price_candidate) * 10_000 <= cfg_resolve_price_deviation_bps * resolved_live_price_candidate`
     - both `resolved_live_price_candidate` and `resolved_price` are privileged wrapper-trusted inputs on this path; on the ordinary branch the band is an internal consistency guard, not an independent oracle-integrity proof
9. else:
   - skip the ordinary live-sync settlement band check
   - the degenerate branch relies entirely on trusted wrapper settlement inputs and must be used only when explicitly permitted by the deployment’s settlement policy
10. compute resolved terminal mark deltas in exact checked signed arithmetic:
   - if `mode_long == ResetPending`, set `resolved_k_long_terminal_delta = 0`
   - else compute `resolved_k_long_terminal_delta = A_long * (resolved_price - resolved_live_price_candidate)` and require representable as persistent `i128`
   - if `mode_short == ResetPending`, set `resolved_k_short_terminal_delta = 0`
   - else compute `resolved_k_short_terminal_delta = -A_short * (resolved_price - resolved_live_price_candidate)` and require representable as persistent `i128`
   - these terminal deltas MUST NOT be added into persistent live `K_side`
11. set `market_mode = Resolved`
12. set `resolved_price = resolved_price`
13. set `resolved_live_price = resolved_live_price_candidate`
14. set `resolved_slot = now_slot`
15. clear resolved payout snapshot state explicitly:
   - `resolved_payout_snapshot_ready = false`
   - `resolved_payout_h_num = 0`
   - `resolved_payout_h_den = 0`
16. set `PNL_matured_pos_tot = PNL_pos_tot`
17. set `OI_eff_long = 0` and `OI_eff_short = 0`
18. for each side:
   - if `mode_side != ResetPending`, invoke `begin_full_drain_reset(side)`
   - if the resulting side state is `ResetPending` and `stale_account_count_side == 0` and `stored_pos_count_side == 0`, invoke `finalize_side_reset(side)`
19. require both open-interest sides are zero
20. require `V >= C_tot + I`

Under §0, steps 5 through 20 are one atomic transition. If any check fails — including ordinary live-sync accrual, explicit degenerate-mode validation, terminal-delta representability, or reset-finalization checks — the market remains live and all intermediate writes roll back with the enclosing instruction.

The ordinary branch is the normative path. The degenerate branch exists only to preserve privileged resolution liveness when applying additional live accrual would be impossible or undesirable under the deployment’s explicit settlement policy — for example because `dt > cfg_max_accrual_dt_slots` or cumulative live `K_side` or `F_side_num` headroom is tight. It is entered only when the wrapper explicitly passes `resolve_mode = Degenerate`.

### 9.9 `force_close_resolved(i, now_slot[, fee_rate_per_slot])`

Multi-stage resolved-market progress path.

An implementation MUST expose an explicit outcome distinguishing:

- `ProgressOnly` — local reconciliation progressed but no terminal close occurred yet
- `Closed { payout }` — the account was terminally closed and paid out `payout`

A zero payout MUST NOT be the sole encoding of “not yet closeable.”

1. require `market_mode == Resolved`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. `prepare_account_for_resolved_touch(i)`
6. if recurring fees are enabled and `last_fee_slot_i < resolved_slot`, `sync_account_fee_to_slot(i, resolved_slot, fee_rate_per_slot)`
7. `settle_side_effects_resolved(i)`
8. settle losses from principal if needed
9. resolve uncovered flat loss if needed
10. if `mode_long == ResetPending` and `OI_eff_long == 0` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, finalize the long side
11. if `mode_short == ResetPending` and `OI_eff_short == 0` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, finalize the short side
12. require `OI_eff_long == OI_eff_short`
13. if `PNL_i <= 0`, return `Closed { payout }` from `force_close_resolved_terminal_nonpositive(i)`
14. if `PNL_i > 0`:
   - if the market is not positive-payout ready:
     - require `V >= C_tot + I`
     - return `ProgressOnly` after persisting the local reconciliation
   - if the shared resolved payout snapshot is not ready, capture it
   - return `Closed { payout }` from `force_close_resolved_terminal_positive(i)`

### 9.10 `reclaim_empty_account(i, now_slot[, fee_rate_per_slot])`

1. require `market_mode == Live`
2. require account `i` is materialized
3. require `now_slot >= current_slot`
4. set `current_slot = now_slot`
5. if recurring fees are enabled, `sync_account_fee_to_slot(i, current_slot, fee_rate_per_slot)`
6. require the flat-clean reclaim preconditions of §2.8
7. require final reclaim eligibility of §2.8
8. execute the reclamation effects of §2.8
9. require `V >= C_tot + I`

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
   - a single initial accrual
   - candidate processing in keeper-supplied order
   - stop further candidate processing once a pending reset is scheduled
9. If recurring account fees are enabled, keeper processing MAY exact-touch fee-current state one candidate at a time using `last_fee_slot_i`; this is intentional and does not require a global scan.

---

## 11. Required test properties

An implementation MUST include tests covering at least the following.

1. `V >= C_tot + I` always.
2. Positive `set_pnl` increases raise `R_i` by the same delta and do not immediately increase `PNL_matured_pos_tot` unless admitted at `h_eff = 0`.
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
28. `enqueue_adl` spends insurance down to `cfg_insurance_floor` before any remaining bankruptcy loss is socialized or left as junior undercollateralization.
29. The exact ADL dust-bound increment matches §5.6 step 10 and the unilateral and bilateral dust-clear conditions match §5.7 exactly.
30. Funding accrual uses exact 256-bit-or-equivalent intermediates for both `fund_num_total` and each `A_side * fund_num_total` product, with symmetry preserved.
31. A flat account with negative `PNL_i` resolves through `absorb_protocol_loss` only in the allowed already-authoritative flat-account paths.
32. Reset finalization reopens a side once `ResetPending` preconditions are fully satisfied.
33. `deposit` settles realized losses before fee sweep.
34. A missing account cannot be materialized by a deposit smaller than `cfg_min_initial_deposit`.
35. The strict risk-reducing trade exemption uses exact widened raw maintenance buffers and exact widened raw maintenance shortfall.
36. The strict risk-reducing trade exemption adds back `fee_equity_impact_i`, not nominal fee.
37. Any side-count increment — including a sign flip — enforces `cfg_max_active_positions_per_side`.
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
52. Live instructions reject invalid admission pairs and invalid `funding_rate_e9_per_slot`.
53. `deposit`, `deposit_fee_credits`, `top_up_insurance_fund`, and `charge_account_fee` do not draw insurance.
54. `settle_flat_negative_pnl` is a live-only permissionless cleanup path that does not mutate side state.
55. On its ordinary branch, `resolve_market(Ordinary, ...)` self-synchronizes live accrual to `now_slot` and stores the final settlement mark as separate resolved terminal deltas.
56. On its ordinary branch, `resolve_market(Ordinary, ...)` rejects settlement prices outside the immutable band around the trusted live-sync price used for that instruction; on its degenerate branch, that ordinary live-sync band check is intentionally bypassed.
57. Resolved local reconciliation applies the stored `resolved_k_*_terminal_delta` exactly on sides that were still live at resolution, and applies zero terminal delta on sides that were already `ResetPending`.
58. Under open-interest symmetry, end-of-instruction reset scheduling preserves `OI_eff_long == OI_eff_short`.
59. Positive resolved payouts do not begin until the market is positive-payout ready per §6.7.
60. `neg_pnl_account_count` exactly matches iteration over materialized accounts with `PNL_i < 0` after every path that mutates `PNL_i`.
61. The touched-account set and instruction-local `h_max` sticky state cannot silently drop an account; if capacity would be exceeded, the instruction fails conservatively.
62. Whole-only automatic flat conversion in §6.6 uses the exact helper sequence `consume_released_pnl` then `set_capital`.
63. `force_close_resolved` exposes an explicit progress-versus-close outcome; a zero payout is never the sole encoding of “not yet closeable.”
64. The positive resolved-close path fails conservatively, not permissively, if a snapshot is marked ready with a zero payout denominator while some account still has `PNL_i > 0`.
65. `advance_profit_warmup` clamps `elapsed` at `sched_horizon` and therefore does not fail merely because an unclamped quotient would exceed `u128`.
66. Live positive reserve creation cannot use `ImmediateReleaseResolvedOnly`.
67. Within one instruction, once an account requires `admit_h_max`, later fresh positive increases on that account also use `admit_h_max`; an earlier newest pending increment may be conservatively lifted, but under-admission is forbidden.
68. `admit_outstanding_reserve_on_touch` either accelerates all outstanding reserve or leaves it unchanged; it never extends or resets reserve horizons.
69. A live live-accrual instruction with `dt > cfg_max_accrual_dt_slots` fails conservatively; privileged `resolve_market` may proceed only through its explicit degenerate branch.
70. Market initialization rejects any `(cfg_max_abs_funding_e9_per_slot, cfg_max_accrual_dt_slots)` pair that violates the exact funding-envelope inequality.
71. `resolve_market(Degenerate, ...)` requires `live_oracle_price = P_last` and `funding_rate_e9_per_slot = 0`; `resolve_market(Ordinary, ...)` MUST stay on the ordinary branch even when those values happen to coincide.
72. A voluntary trade that closes an account exactly to flat is not rejected solely because current-trade fees create or increase fee debt; the zero-position branch uses the same fee-neutral shortfall-comparison principle as strict risk reduction.
73. `max_safe_flat_conversion_released` uses 256-bit-or-equivalent arithmetic and does not silently overflow on `E_before * h_den`.
74. Candidate ordering in `keeper_crank` may affect warmup UX but not solvency, conservation, or correctness.
75. Resolved local reconciliation may exceed live-only caps while still failing conservatively on any `u128` aggregate overflow.
76. `close_account` cannot be used to forgive unpaid fee debt; unresolved debt must be repaid or reclaimed through the dust path.
77. After any `A_side` decay in ADL, any mismatch between authoritative `OI_eff_side` and summed per-account same-epoch floor quantities is bounded and resolved only through explicit phantom-dust rules.
78. Long-running markets with little matured-PnL extraction eventually see `Residual` become scarce relative to `PNL_matured_pos_tot`, causing fresh reserve admission to select slower horizons more often; this is operationally visible but must never break safety or correctness.
79. A newly materialized account sets `last_fee_slot_i = materialize_slot` and is never charged for earlier time.
80. `sync_account_fee_to_slot(i, t, r)` charges exactly once over `[last_fee_slot_i, t]`, advances `last_fee_slot_i` to `t`, and a second sync at the same `t` is a no-op.
81. `last_fee_slot_i <= resolved_slot` holds for all materialized accounts on resolved markets.
82. Resolved recurring-fee sync uses `resolved_slot`, not later wall-clock time.
83. Capturing the resolved payout snapshot before some accounts are fee-current does not invalidate later payouts because late fee sync is a pure `C -> I` reclassification.
84. If `advance_profit_warmup` empties the scheduled bucket in a frame where `sched_total > sched_release_q`, the bucket is cleared immediately; no non-empty bucket can persist with an over-advanced `sched_release_q`.
85. `resolve_market(Ordinary, ...)` does not silently fall into the degenerate branch when `live_oracle_price == P_last` and `funding_rate_e9_per_slot == 0`; explicit `resolve_mode` controls branch selection.
86. `sync_account_fee_to_slot(i, t, r)` caps to `MAX_PROTOCOL_FEE_ABS` and advances `last_fee_slot_i` even when the uncapped raw product `r * (t - last_fee_slot_i)` exceeds native `u128`.
87. Same-epoch basis replacement with nonzero orphan remainder increments the relevant `phantom_dust_bound_*_q` by exactly `1` q-unit.
88. Same-epoch live settlement with `q_eff_new == 0` increments the relevant `phantom_dust_bound_*_q` by exactly `1` q-unit before basis reset.

---

## 12. Wrapper obligations (deployment layer, not engine-checked)

The following are deployment-wrapper obligations.

1. **Do not expose caller-controlled live policy inputs.**  
   `(admit_h_min, admit_h_max)` and `funding_rate_e9_per_slot` are wrapper-owned internal inputs. Public or permissionless wrappers MUST derive them internally and MUST NOT accept arbitrary caller-chosen values.

2. **Authority-gate market resolution and supply trusted inputs for both ordinary and degenerate branches.**  
   `resolve_market` is a privileged deployment-owned transition. A compliant wrapper MUST source both `live_oracle_price` and `resolved_price` from the deployment’s trusted settlement sources or policy, MUST source the wrapper-owned current funding rate used for the ordinary live-sync leg inside `resolve_market`, and MUST pass an explicit trusted `resolve_mode ∈ {Ordinary, Degenerate}` selector. For normal resolution it MUST pass `resolve_mode = Ordinary`. If it intentionally uses the degenerate recovery branch, it MUST pass `resolve_mode = Degenerate`, `live_oracle_price = P_last`, and `funding_rate_e9_per_slot = 0`, and it MUST do so only when that behavior is explicitly permitted by the deployment’s settlement policy.

3. **Do not emulate resolution with a separate prior accrual transaction as the normal path.**  
   Because `resolve_market` is self-synchronizing in this revision, a compliant wrapper MUST invoke it directly with trusted live-sync inputs and `resolve_mode = Ordinary` for ordinary operation. A separate pre-accrual transaction is not required and MUST NOT be treated as the normative path, though a deployment MAY use an explicit pre-accrual or headroom-management flow as an operational recovery tool if it is trying to avoid cumulative `K` or `F` saturation before resolution. If live accrual would still be unsafe or impossible, the wrapper MAY instead use the privileged degenerate branch inside `resolve_market` by explicitly passing `resolve_mode = Degenerate`.

4. **Respect the funding envelope operationally.**  
   A compliant deployment MUST monitor `slot_last`, `cfg_max_accrual_dt_slots`, and `cfg_max_abs_funding_e9_per_slot` so the market is actively cranked or ordinarily resolved before the engine’s live accrual envelope is exceeded. If the deployment enables permissionless stale resolution, it MUST choose `permissionless_resolve_stale_slots <= cfg_max_accrual_dt_slots`. If the envelope is exceeded anyway, only the privileged degenerate branch remains available.

5. **Public wrappers SHOULD enforce execution-price admissibility.**  
   A sufficient rule is `abs(exec_price - oracle_price) * 10_000 <= max_trade_price_deviation_bps * oracle_price`, with `max_trade_price_deviation_bps <= 2 * cfg_trading_fee_bps`.

6. **Use oracle notional for wrapper-side exposure ranking.**

7. **Keep user-owned value-moving operations account-authorized.**  
   User-owned value-moving paths include `deposit`, `withdraw`, `execute_trade`, `close_account`, and `convert_released_pnl`. Intended permissionless progress paths are `settle_account`, `liquidate`, `reclaim_empty_account`, `settle_flat_negative_pnl`, `force_close_resolved`, and `keeper_crank`.

8. **Do not expose pure wrapper-owned account fees carelessly.**  
   `charge_account_fee` performs no maintenance gating of its own. A compliant public wrapper MUST either restrict it to already-safe contexts or pair it with a same-instruction live-touch health-check flow when used on accounts that may still carry live risk.

9. **If desired, tighten the dropped-fee policy above the engine.**  
   The core engine’s strict risk-reducing comparison is defined by actual `fee_equity_impact_i` only. A deployment that wishes to reject strict risk-reducing trades whenever `fee_dropped_i > 0` MAY impose that stricter wrapper rule above the engine.

10. **Provide a post-snapshot resolved-close progress path.**  
    Because `force_close_resolved` is intentionally multi-stage, a compliant deployment SHOULD provide either a self-service retry path or a permissionless batch or incentive path that sweeps positive resolved accounts after the shared payout snapshot is ready.

11. **Set account-opening economics high enough to resist slot-griefing.**  
    A compliant deployment MUST choose `cfg_min_initial_deposit` and any account-opening fee or equivalent economic barrier so that exhausting the configured materialized-account capacity is economically prohibitive relative to the deployment’s threat model.

12. **Size runtime batches to actual compute limits.**  
    On constrained runtimes, a compliant deployment MUST choose `max_revalidations`, batch-close sizes, and any wrapper-side multi-account composition so one instruction fits the runtime’s per-instruction compute budget.

13. **Plan market lifecycle before K/F headroom exhaustion.**  
    A compliant deployment SHOULD monitor cumulative `K_side` and `F_side_num` headroom and resolve or migrate the market before approaching persistent `i128` saturation.

14. **If more throughput is required than one market state can provide, shard at the deployment layer.**  
    One market instance serializes writes by design. A deployment that requires higher throughput SHOULD shard across multiple market instances rather than assuming runtime-level parallelism inside one market.

15. **If deterministic keeper UX is desired, canonicalize candidate order.**  
    The engine intentionally treats keeper candidate order as policy. A deployment that wants deterministic warmup-admission or acceleration UX across keepers SHOULD canonicalize `ordered_candidates[]`, for example by ascending storage index after off-chain risk bucketing.

16. **Surface matured-pool saturation to users.**  
    In long-running markets where users do not convert or withdraw matured profit, `PNL_matured_pos_tot` can grow close to `Residual`, causing fresh reserve admission to select slower horizons more often. Deployments SHOULD surface this state in UI and MAY prompt users to settle or extract matured claims when appropriate.

17. **Provide an operator recovery path for impossible invariant-breach orphans if the deployment requires one.**  
    The core engine intentionally fails conservatively if resolved reconciliation encounters a state that violates the epoch-gap or reset invariants. A deployment that wants an explicit operational escape hatch for such impossible states SHOULD provide a privileged migration or recovery path above the engine rather than weakening the engine’s conservative-failure rules.

18. **If the deployment enables wrapper-owned recurring account fees, sync before health-sensitive checks.**  
    A compliant wrapper MUST sync recurring fees to the relevant anchor before using an account’s touched state for:
    - live maintenance checks,
    - live liquidation eligibility,
    - reclaim eligibility,
    - resolved terminal close,
    - any user-facing action whose correctness depends on up-to-date fee debt.

19. **Use `resolved_slot` as the recurring-fee anchor on resolved markets.**  
    A compliant wrapper MUST NOT accrue recurring account fees past `resolved_slot`.

20. **Anchor new accounts correctly.**  
    A compliant wrapper MUST materialize new accounts using their actual creation slot as `materialize_slot`, so `last_fee_slot_i` starts at the right point.

---

## 13. Operational notes (non-normative)

1. **Wide exact arithmetic costs compute.** Exact 256-bit-or-equivalent multiply-divide and signed floor arithmetic are materially more expensive than native 128-bit operations. Keepers and wrappers should use bounded candidate sets and avoid oversized multi-account transactions.

2. **One market account serializes one market.** Because core instructions update shared market aggregates (`V`, `I`, `C_tot`, `PNL_pos_tot`, `A_side`, `K_side`, `F_side_num`, and so on), one market instance is throughput-serialized by design.

3. **Account-capacity griefing is economic, not mathematical.** If `cfg_min_initial_deposit` or any account-opening fee is set too low, an attacker can economically spam materialization. The engine’s reclaim path preserves eventual liveness, but the deployment must still choose parameters that make the attack unattractive and should incentivize reclaim.

4. **Resolution paths should stay thin.** Even though `resolve_market` is self-synchronizing, wrappers should keep the resolution path small in transaction size and compute. Precompute external checks off chain where possible, avoid unnecessary CPI fanout in the same transaction, and remember that the settlement band checks consistency between wrapper-trusted prices rather than supplying an independent oracle guarantee.

5. **Multi-instruction keeper progress is normal.** Because `keeper_crank` intentionally stops further live-OI-dependent processing once a reset is pending, volatile periods may require multiple successive keeper instructions.

6. **Batch positive resolved closes are recommended when practical.** The engine defines exact single-account progress and terminal-close semantics. Deployments that expect many resolved accounts should strongly consider a batched wrapper or incentive path for post-snapshot sweeping to reduce transaction overhead.

7. **Funding envelopes are an engine safety boundary, not only a wrapper preference.** The engine-enforced pair `(cfg_max_abs_funding_e9_per_slot, cfg_max_accrual_dt_slots)` is what prevents dormant-market funding accrual from overflowing persistent `F_side_num`. Wrapper policy should stay comfortably inside that envelope; if the envelope is exceeded anyway, only the privileged degenerate branch of `resolve_market` remains live.

8. **The recurring-fee checkpoint is intentionally local.** `last_fee_slot_i` is the minimal extra state needed to make touched-account recurring fees exact. It avoids a global fee scan, but it means fee freshness is per account, not globally uniform.

9. **Late resolved fee sync is harmless to payout ratios.** Once the resolved payout snapshot is captured, late fee sync only moves value from `C_i` to `I`. That preserves `Residual = V - (C_tot + I)`. Any uncollectible tail that is dropped stays as conservative unused slack; it is not socialized through payouts.

10. **Monotone pending-bucket max-horizon merge is deliberate.** Coalescing into the newest pending bucket by `max(pending_horizon_i, admitted_h_eff)` is intentionally conservative. It can delay newer-bucket maturity but it never accelerates it and never contaminates the older scheduled bucket.
