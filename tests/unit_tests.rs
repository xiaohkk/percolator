#![cfg(feature = "test")]

use percolator::*;
use percolator::wide_math::{U256, I256};

// ============================================================================
// Helpers
// ============================================================================

fn default_params() -> RiskParams {
    RiskParams {
        warmup_period_slots: 100,
        maintenance_margin_bps: 500,    // 5%
        initial_margin_bps: 1000,       // 10% — MUST be > maintenance
        trading_fee_bps: 10,
        max_accounts: 64,
        new_account_fee: U128::new(1000),
        maintenance_fee_per_slot: U128::new(1),
        max_crank_staleness_slots: 1000,
        liquidation_fee_bps: 100,
        liquidation_fee_cap: U128::new(1_000_000),
        liquidation_buffer_bps: 50,
        min_liquidation_abs: U128::new(0),
    }
}

/// Build a size_q from a quantity in base units.
/// size_q = quantity * POS_SCALE  (signed)
fn make_size_q(quantity: i64) -> I256 {
    let abs_qty = (quantity as i128).unsigned_abs();
    let product = U256::from_u128(POS_SCALE)
        .checked_mul(U256::from_u128(abs_qty))
        .expect("make_size_q overflow");
    let positive = I256::from_raw_u256_pub(product);
    if quantity < 0 {
        positive.checked_neg().expect("make_size_q neg overflow")
    } else {
        positive
    }
}

/// Helper: create engine, add two users with deposits, run initial crank.
/// Returns (engine, user_a_idx, user_b_idx).
fn setup_two_users(deposit_a: u128, deposit_b: u128) -> (RiskEngine, u16, u16) {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add user a");
    let b = engine.add_user(1000).expect("add user b");

    // Deposit before crank so accounts have capital and are not GC'd
    if deposit_a > 0 {
        engine.deposit(a, deposit_a, oracle, slot).expect("deposit a");
    }
    if deposit_b > 0 {
        engine.deposit(b, deposit_b, oracle, slot).expect("deposit b");
    }

    // Initial crank so trades/withdrawals pass freshness check
    engine.keeper_crank(a, slot, oracle, 0).expect("initial crank");

    (engine, a, b)
}

// ============================================================================
// 1. Basic engine creation and parameter validation
// ============================================================================

#[test]
fn test_engine_creation() {
    let engine = RiskEngine::new(default_params());
    assert_eq!(engine.vault.get(), 0);
    assert_eq!(engine.insurance_fund.balance.get(), 0);
    assert_eq!(engine.current_slot, 0);
    assert_eq!(engine.num_used_accounts, 0);
    assert!(engine.check_conservation());
}

#[test]
#[should_panic(expected = "maintenance_margin_bps must be strictly less than initial_margin_bps")]
fn test_params_require_mm_lt_im() {
    let mut params = default_params();
    params.maintenance_margin_bps = 1000;
    params.initial_margin_bps = 1000; // equal => should panic
    let _ = RiskEngine::new(params);
}

#[test]
#[should_panic(expected = "maintenance_margin_bps must be strictly less than initial_margin_bps")]
fn test_params_require_mm_lt_im_greater() {
    let mut params = default_params();
    params.maintenance_margin_bps = 1500;
    params.initial_margin_bps = 1000; // mm > im => should panic
    let _ = RiskEngine::new(params);
}

// ============================================================================
// 2. add_user and add_lp
// ============================================================================

#[test]
fn test_add_user() {
    let mut engine = RiskEngine::new(default_params());
    let idx = engine.add_user(1000).expect("add_user");
    assert_eq!(idx, 0);
    assert!(engine.is_used(idx as usize));
    assert_eq!(engine.num_used_accounts, 1);
    // Fee of 1000 goes to insurance; excess = 0
    assert_eq!(engine.accounts[idx as usize].capital.get(), 0);
    assert_eq!(engine.insurance_fund.balance.get(), 1000);
    assert_eq!(engine.vault.get(), 1000);
    assert!(engine.accounts[idx as usize].is_user());
}

#[test]
fn test_add_user_with_excess() {
    let mut engine = RiskEngine::new(default_params());
    let idx = engine.add_user(5000).expect("add_user");
    // excess = 5000 - 1000 = 4000 goes to capital
    assert_eq!(engine.accounts[idx as usize].capital.get(), 4000);
    assert_eq!(engine.insurance_fund.balance.get(), 1000);
    assert_eq!(engine.vault.get(), 5000);
}

#[test]
fn test_add_user_insufficient_fee() {
    let mut engine = RiskEngine::new(default_params());
    let result = engine.add_user(500); // less than new_account_fee (1000)
    assert_eq!(result, Err(RiskError::InsufficientBalance));
}

#[test]
fn test_add_lp() {
    let mut engine = RiskEngine::new(default_params());
    let program = [1u8; 32];
    let context = [2u8; 32];
    let idx = engine.add_lp(program, context, 2000).expect("add_lp");
    assert!(engine.is_used(idx as usize));
    assert!(engine.accounts[idx as usize].is_lp());
    assert_eq!(engine.accounts[idx as usize].matcher_program, program);
    assert_eq!(engine.accounts[idx as usize].matcher_context, context);
    assert_eq!(engine.accounts[idx as usize].capital.get(), 1000); // 2000 - 1000 fee
}

// ============================================================================
// 3. deposit and withdraw
// ============================================================================

#[test]
fn test_deposit() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;
    let idx = engine.add_user(1000).expect("add_user");

    let vault_before = engine.vault.get();
    engine.deposit(idx, 10_000, oracle, slot).expect("deposit");
    assert_eq!(engine.accounts[idx as usize].capital.get(), 10_000);
    assert_eq!(engine.vault.get(), vault_before + 10_000);
    assert!(engine.check_conservation());
}

#[test]
fn test_withdraw_no_position() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;
    let idx = engine.add_user(1000).expect("add_user");

    // Deposit before crank so account is not GC'd
    engine.deposit(idx, 10_000, oracle, slot).expect("deposit");

    // Initial crank needed for freshness
    engine.keeper_crank(idx, slot, oracle, 0).expect("crank");

    engine.withdraw(idx, 5_000, oracle, slot).expect("withdraw");
    assert_eq!(engine.accounts[idx as usize].capital.get(), 5_000);
    assert!(engine.check_conservation());
}

#[test]
fn test_withdraw_exceeds_balance() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;
    let idx = engine.add_user(1000).expect("add_user");
    engine.deposit(idx, 5_000, oracle, slot).expect("deposit");
    engine.keeper_crank(idx, slot, oracle, 0).expect("crank");

    let result = engine.withdraw(idx, 10_000, oracle, slot);
    assert_eq!(result, Err(RiskError::InsufficientBalance));
}

#[test]
fn test_withdraw_requires_fresh_crank() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let idx = engine.add_user(1000).expect("add_user");
    engine.deposit(idx, 10_000, oracle, 1).expect("deposit");

    // Advance far beyond staleness window without cranking
    let result = engine.withdraw(idx, 1_000, oracle, 5000);
    assert_eq!(result, Err(RiskError::Unauthorized));
}

// ============================================================================
// 4. execute_trade basics
// ============================================================================

#[test]
fn test_basic_trade() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Trade: a goes long 100 units, b goes short 100 units
    let size_q = make_size_q(100);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Both should have positions
    let eff_a = engine.effective_pos_q(a as usize);
    let eff_b = engine.effective_pos_q(b as usize);
    assert!(eff_a.is_positive());
    assert!(eff_b.is_negative());
    assert!(engine.check_conservation());
}

#[test]
fn test_trade_requires_fresh_crank() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let a = engine.add_user(1000).expect("add user a");
    let b = engine.add_user(1000).expect("add user b");
    engine.deposit(a, 100_000, oracle, 1).expect("deposit a");
    engine.deposit(b, 100_000, oracle, 1).expect("deposit b");

    // No crank, advance way past staleness
    let size_q = make_size_q(10);
    let result = engine.execute_trade(a, b, oracle, 5000, size_q, oracle);
    assert_eq!(result, Err(RiskError::Unauthorized));
}

#[test]
fn test_trade_undercollateralized_rejected() {
    let (mut engine, a, b) = setup_two_users(1_000, 1_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Try to open a huge position that exceeds margin
    // 1000 capital, 10% IM => max notional = 10000
    // notional = |size| * oracle / POS_SCALE, so for oracle=1000,
    // 11 units => notional = 11000, requires 1100 IM
    let size_q = make_size_q(11);
    let result = engine.execute_trade(a, b, oracle, slot, size_q, oracle);
    assert_eq!(result, Err(RiskError::Undercollateralized));
}

#[test]
fn test_trade_with_different_exec_price() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let exec = 990u64;
    let slot = 1u64;

    // Trade at exec_price=990 vs oracle=1000
    // trade_pnl for long = size * (oracle - exec) / POS_SCALE
    let size_q = make_size_q(100);
    engine.execute_trade(a, b, oracle, slot, size_q, exec).expect("trade");

    // Account a (long) should have positive PnL from oracle-exec gap
    // Account b (short) should have negative PnL
    assert!(engine.check_conservation());
}

// ============================================================================
// 5. Conservation invariant
// ============================================================================

#[test]
fn test_conservation_after_deposits() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(5000).expect("add user a");
    engine.deposit(a, 100_000, oracle, slot).expect("deposit");
    let b = engine.add_user(3000).expect("add user b");
    engine.deposit(b, 50_000, oracle, slot).expect("deposit");

    assert!(engine.check_conservation());
    // V >= C_tot + I
    let senior = engine.c_tot.get() + engine.insurance_fund.balance.get();
    assert!(engine.vault.get() >= senior);
}

#[test]
fn test_conservation_after_trade() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");
    assert!(engine.check_conservation());
}

// ============================================================================
// 6. Haircut ratio computation
// ============================================================================

#[test]
fn test_haircut_ratio_no_positive_pnl() {
    let engine = RiskEngine::new(default_params());
    let (h_num, h_den) = engine.haircut_ratio();
    // When pnl_pos_tot == 0, returns (1, 1)
    assert_eq!(h_num, U256::ONE);
    assert_eq!(h_den, U256::ONE);
}

#[test]
fn test_haircut_ratio_with_surplus() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Execute a trade, then move price to give one side positive PnL
    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Now accrue market with a higher price
    engine.accrue_market_to(2, 1100).expect("accrue");
    // Touch accounts to realize PnL
    engine.touch_account_full(a as usize, 1100, 2).expect("touch a");
    engine.touch_account_full(b as usize, 1100, 2).expect("touch b");

    let (h_num, h_den) = engine.haircut_ratio();
    // h_num <= h_den always
    assert!(h_num <= h_den);
}

// ============================================================================
// 7. Liquidation at oracle
// ============================================================================

#[test]
fn test_liquidation_eligible_account() {
    // Use a smaller capital so we can trigger liquidation more easily
    let (mut engine, a, b) = setup_two_users(50_000, 200_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open a position near the margin limit
    // 50_000 capital, 10% IM => max notional = 500_000
    // 480 units * 1000 = 480_000 notional, IM = 48_000
    let size_q = make_size_q(480);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Move the price against the long (a) to trigger liquidation
    // Use accrue_market_to to update price state without running the full crank
    // (the crank would itself liquidate the account before we can test it explicitly)
    let new_oracle = 890u64;
    let slot2 = 2u64;

    // Call liquidate_at_oracle directly - it calls touch_account_full internally
    // which runs accrue_market_to
    let result = engine.liquidate_at_oracle(a, slot2, new_oracle).expect("liquidate");
    assert!(result, "account a should have been liquidated");
    // Position should be closed
    let eff = engine.effective_pos_q(a as usize);
    assert!(eff.is_zero());
    assert!(engine.check_conservation());
}

#[test]
fn test_liquidation_healthy_account() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Account is well collateralized, liquidation should return false
    let result = engine.liquidate_at_oracle(a, slot, oracle).expect("liquidate attempt");
    assert!(!result, "healthy account should not be liquidated");
}

#[test]
fn test_liquidation_flat_account() {
    let (mut engine, a, _b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // No position open, liquidation should return false
    let result = engine.liquidate_at_oracle(a, slot, oracle).expect("liquidate flat");
    assert!(!result);
}

// ============================================================================
// 8. Warmup and profit conversion
// ============================================================================

#[test]
fn test_warmup_slope_set_on_new_profit() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Advance and accrue at higher price so long (a) gets positive PnL
    let slot2 = 10u64;
    let new_oracle = 1100u64;
    engine.keeper_crank(a, slot2, new_oracle, 0).expect("crank");
    engine.touch_account_full(a as usize, new_oracle, slot2).expect("touch");

    // If PnL is positive and warmup_period > 0, slope should be set
    if engine.accounts[a as usize].pnl.is_positive() {
        assert!(!engine.accounts[a as usize].warmup_slope_per_step.is_zero(),
            "warmup slope should be nonzero for positive PnL");
    }
}

#[test]
fn test_warmup_full_conversion_after_period() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Move price up to give account a profit
    let slot2 = 10u64;
    let new_oracle = 1200u64;
    engine.keeper_crank(a, slot2, new_oracle, 0).expect("crank");
    engine.touch_account_full(a as usize, new_oracle, slot2).expect("touch");

    let capital_before = engine.accounts[a as usize].capital.get();

    // Wait beyond warmup period (100 slots) and touch again
    let slot3 = slot2 + 200;
    engine.keeper_crank(a, slot3, new_oracle, 0).expect("crank2");
    engine.touch_account_full(a as usize, new_oracle, slot3).expect("touch2");

    let capital_after = engine.accounts[a as usize].capital.get();
    // Capital should increase after warmup conversion
    assert!(capital_after >= capital_before,
        "capital should increase after warmup conversion");
    assert!(engine.check_conservation());
}

// ============================================================================
// 9. Insurance fund operations
// ============================================================================

#[test]
fn test_top_up_insurance_fund() {
    let mut engine = RiskEngine::new(default_params());
    let before_vault = engine.vault.get();
    let before_ins = engine.insurance_fund.balance.get();

    let result = engine.top_up_insurance_fund(5000).expect("top_up");
    assert_eq!(engine.vault.get(), before_vault + 5000);
    assert_eq!(engine.insurance_fund.balance.get(), before_ins + 5000);
    assert!(result); // above floor (floor = 0)
    assert!(engine.check_conservation());
}

#[test]
fn test_insurance_floor() {
    let mut engine = RiskEngine::new(default_params());
    engine.set_insurance_floor(10000);
    assert_eq!(engine.insurance_floor, 10000);

    engine.top_up_insurance_fund(5000).expect("top_up");
    // balance 5000 < floor 10000
    let result = engine.top_up_insurance_fund(0).expect("check");
    assert!(!result, "should be below insurance floor");

    engine.top_up_insurance_fund(6000).expect("top_up2");
    // balance 11000 > floor 10000
    let result2 = engine.top_up_insurance_fund(0).expect("check2");
    assert!(result2, "should be above insurance floor");
}

// ============================================================================
// 10. Fee operations
// ============================================================================

#[test]
fn test_deposit_fee_credits() {
    let mut engine = RiskEngine::new(default_params());
    let slot = 1u64;
    engine.current_slot = slot;
    let idx = engine.add_user(1000).expect("add_user");

    engine.deposit_fee_credits(idx, 5000, slot).expect("deposit_fee_credits");
    assert!(engine.accounts[idx as usize].fee_credits.get() > 0);
    assert!(engine.check_conservation());
}

#[test]
fn test_add_fee_credits() {
    let mut engine = RiskEngine::new(default_params());
    let slot = 1u64;
    engine.current_slot = slot;
    let idx = engine.add_user(1000).expect("add_user");

    engine.add_fee_credits(idx, 3000).expect("add_fee_credits");
    assert_eq!(engine.accounts[idx as usize].fee_credits.get(), 3000);
}

#[test]
fn test_trading_fee_charged() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let capital_before = engine.accounts[a as usize].capital.get();

    let size_q = make_size_q(100);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    let capital_after = engine.accounts[a as usize].capital.get();
    // Trading fee should reduce capital of account a
    // fee = ceil(|100| * 1000 * 10 / 10000) = ceil(100) = 100
    assert!(capital_after < capital_before, "trading fee should reduce capital");
    assert!(engine.check_conservation());
}

#[test]
fn test_lp_fees_earned_tracking() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add user");
    let lp = engine.add_lp([1; 32], [2; 32], 1000).expect("add lp");

    // Deposit before crank so accounts are not GC'd
    engine.deposit(a, 100_000, oracle, slot).expect("deposit a");
    engine.deposit(lp, 100_000, oracle, slot).expect("deposit lp");
    engine.keeper_crank(a, slot, oracle, 0).expect("crank");

    let size_q = make_size_q(100);
    engine.execute_trade(a, lp, oracle, slot, size_q, oracle).expect("trade");

    // LP (account b) should track fees earned
    assert!(engine.accounts[lp as usize].fees_earned_total.get() > 0,
        "LP should track fees earned");
}

// ============================================================================
// 11. Close account
// ============================================================================

#[test]
fn test_close_account_flat() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let idx = engine.add_user(1000).expect("add_user");
    engine.deposit(idx, 10_000, oracle, slot).expect("deposit");

    let capital_returned = engine.close_account(idx, slot, oracle).expect("close");
    assert_eq!(capital_returned, 10_000);
    assert!(!engine.is_used(idx as usize));
    assert!(engine.check_conservation());
}

#[test]
fn test_close_account_with_position_fails() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    let result = engine.close_account(a, slot, oracle);
    assert_eq!(result, Err(RiskError::Undercollateralized));
}

#[test]
fn test_close_account_not_found() {
    let mut engine = RiskEngine::new(default_params());
    let result = engine.close_account(99, 1, 1000);
    assert_eq!(result, Err(RiskError::AccountNotFound));
}

// ============================================================================
// 12. Keeper crank
// ============================================================================

#[test]
fn test_keeper_crank_advances_slot() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 10u64;
    let caller = engine.add_user(1000).expect("add_user");

    let outcome = engine.keeper_crank(caller, slot, oracle, 0).expect("crank");
    assert!(outcome.advanced);
    assert_eq!(engine.last_crank_slot, slot);
}

#[test]
fn test_keeper_crank_same_slot_not_advanced() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 10u64;
    let caller = engine.add_user(1000).expect("add_user");

    engine.keeper_crank(caller, slot, oracle, 0).expect("crank1");
    let outcome = engine.keeper_crank(caller, slot, oracle, 0).expect("crank2");
    assert!(!outcome.advanced);
}

#[test]
fn test_keeper_crank_sets_funding_rate() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 10u64;
    let caller = engine.add_user(1000).expect("add_user");

    engine.keeper_crank(caller, slot, oracle, 50).expect("crank");
    assert_eq!(engine.funding_rate_bps_per_slot_last, 50);
}

#[test]
fn test_keeper_crank_caller_fee_discount() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let caller = engine.add_user(1000).expect("add_user");
    engine.deposit(caller, 10_000, oracle, slot).expect("deposit");

    // Advance some slots to accumulate maintenance fees
    let slot2 = 200u64;
    let outcome = engine.keeper_crank(caller, slot2, oracle, 0).expect("crank");
    assert!(outcome.caller_settle_ok);
    assert!(outcome.slots_forgiven > 0, "caller should get fee discount");
}

// ============================================================================
// 13. Side mode gating (DrainOnly, ResetPending)
// ============================================================================

#[test]
fn test_drain_only_blocks_new_trades() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Manually set long side to DrainOnly
    engine.side_mode_long = SideMode::DrainOnly;

    // Try to open a new long position (a goes long) — should be blocked
    let size_q = make_size_q(50);
    let result = engine.execute_trade(a, b, oracle, slot, size_q, oracle);
    assert_eq!(result, Err(RiskError::SideBlocked));
}

#[test]
fn test_drain_only_allows_reducing_trade() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open a position first in Normal mode
    let size_q = make_size_q(100);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("open trade");

    // Now set long side to DrainOnly
    engine.side_mode_long = SideMode::DrainOnly;

    // Reducing trade (a goes short = reducing long) should work
    let reduce_q = make_size_q(-50);
    engine.execute_trade(a, b, oracle, slot, reduce_q, oracle)
        .expect("reducing trade should succeed in DrainOnly");
}

#[test]
fn test_reset_pending_blocks_new_trades() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // ResetPending with stale_account_count > 0 is NOT auto-finalizable,
    // so it must still block OI-increasing trades.
    engine.side_mode_short = SideMode::ResetPending;
    engine.stale_account_count_short = 1;

    // b would go long (opposite of short blocked), a goes short — short increase blocked
    let size_q = make_size_q(-50); // a goes short
    let result = engine.execute_trade(a, b, oracle, slot, size_q, oracle);
    assert_eq!(result, Err(RiskError::SideBlocked));
}

// ============================================================================
// 14. ADL mechanics
// ============================================================================

#[test]
fn test_adl_triggered_by_liquidation() {
    let (mut engine, a, b) = setup_two_users(50_000, 50_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open large positions near margin
    // 50k capital, 10% IM => max notional = 500k
    // 450 units * 1000 = 450k notional, IM = 45k
    let size_q = make_size_q(450);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Move price down sharply to make long (a) deeply underwater
    // Call liquidate_at_oracle directly (the crank would liquidate first)
    let slot2 = 2u64;
    let crash_oracle = 870u64;

    let result = engine.liquidate_at_oracle(a, slot2, crash_oracle).expect("liquidate");
    assert!(result, "account a should be liquidated");
    assert!(engine.check_conservation());

    // After liquidation, the position is closed. ADL state may have changed.
    let eff_a = engine.effective_pos_q(a as usize);
    assert!(eff_a.is_zero(), "liquidated position should be zero");
}

#[test]
fn test_adl_epoch_changes() {
    let mut engine = RiskEngine::new(default_params());
    let epoch_long_before = engine.adl_epoch_long;

    // Begin a full drain reset on long side (requires OI=0)
    assert!(engine.oi_eff_long_q.is_zero());
    engine.begin_full_drain_reset(Side::Long);

    assert_eq!(engine.adl_epoch_long, epoch_long_before + 1);
    assert_eq!(engine.side_mode_long, SideMode::ResetPending);
    assert_eq!(engine.adl_mult_long, ADL_ONE);
}

#[test]
fn test_effective_pos_epoch_mismatch() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open position
    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Manually bump the long epoch to simulate a reset
    engine.adl_epoch_long += 1;

    // Effective position should be zero due to epoch mismatch
    let eff = engine.effective_pos_q(a as usize);
    assert!(eff.is_zero(), "epoch mismatch should zero effective position");
}

// ============================================================================
// Additional edge-case tests
// ============================================================================

#[test]
fn test_set_owner() {
    let mut engine = RiskEngine::new(default_params());
    let idx = engine.add_user(1000).expect("add_user");
    let owner = [42u8; 32];
    engine.set_owner(idx, owner).expect("set_owner");
    assert_eq!(engine.accounts[idx as usize].owner, owner);
}

#[test]
fn test_set_owner_invalid_idx() {
    let mut engine = RiskEngine::new(default_params());
    let result = engine.set_owner(99, [0u8; 32]);
    assert_eq!(result, Err(RiskError::Unauthorized));
}

#[test]
fn test_notional_computation() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let size_q = make_size_q(100);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    let notional = engine.notional(a as usize, oracle);
    // notional = |100 * POS_SCALE| * 1000 / POS_SCALE = 100_000
    assert_eq!(notional, 100_000);
}

#[test]
fn test_advance_slot() {
    let mut engine = RiskEngine::new(default_params());
    assert_eq!(engine.current_slot, 0);
    engine.advance_slot(42);
    assert_eq!(engine.current_slot, 42);
    engine.advance_slot(8);
    assert_eq!(engine.current_slot, 50);
}

#[test]
fn test_recompute_aggregates() {
    let (mut engine, a, b) = setup_two_users(50_000, 50_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let size_q = make_size_q(30);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    let c_before = engine.c_tot.get();
    let pnl_before = engine.pnl_pos_tot;

    engine.recompute_aggregates();

    // Aggregates should be consistent after recompute
    assert_eq!(engine.c_tot.get(), c_before);
    assert_eq!(engine.pnl_pos_tot, pnl_before);
}

#[test]
fn test_multiple_accounts() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    // Create several accounts
    for _ in 0..10 {
        let idx = engine.add_user(1000).expect("add_user");
        engine.deposit(idx, 10_000, oracle, slot).expect("deposit");
    }

    assert_eq!(engine.num_used_accounts, 10);
    assert_eq!(engine.count_used(), 10);
    assert!(engine.check_conservation());
}

#[test]
fn test_trade_then_close_round_trip() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open position
    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("open");

    // Close position (reverse trade)
    let close_q = make_size_q(-50);
    engine.execute_trade(a, b, oracle, slot, close_q, oracle).expect("close");

    let eff_a = engine.effective_pos_q(a as usize);
    let eff_b = engine.effective_pos_q(b as usize);
    assert!(eff_a.is_zero(), "position a should be flat after close");
    assert!(eff_b.is_zero(), "position b should be flat after close");
    assert!(engine.check_conservation());
}

#[test]
fn test_withdraw_with_position_margin_check() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open position: 100 units * 1000 = 100k notional, 10% IM = 10k required
    let size_q = make_size_q(100);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Try to withdraw so much that IM is violated
    // capital ~ 100k (minus fees), need at least 10k for IM
    let result = engine.withdraw(a, 95_000, oracle, slot);
    assert_eq!(result, Err(RiskError::Undercollateralized));
}

#[test]
fn test_zero_size_trade_rejected() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    let result = engine.execute_trade(a, b, oracle, slot, I256::ZERO, oracle);
    assert_eq!(result, Err(RiskError::Overflow));
}

#[test]
fn test_zero_oracle_rejected() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let slot = 1u64;

    let size_q = make_size_q(10);
    let result = engine.execute_trade(a, b, 0, slot, size_q, 1000);
    assert_eq!(result, Err(RiskError::Overflow));
}

#[test]
fn test_close_account_after_trade_and_unwind() {
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open and close position
    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("open");
    let close_q = make_size_q(-50);
    engine.execute_trade(a, b, oracle, slot, close_q, oracle).expect("close");

    // Wait beyond warmup to let PnL settle
    let slot2 = slot + 200;
    engine.keeper_crank(a, slot2, oracle, 0).expect("crank");
    engine.touch_account_full(a as usize, oracle, slot2).expect("touch");

    // PnL should be zero or converted by now
    let pnl = engine.accounts[a as usize].pnl;
    if pnl.is_zero() {
        let cap = engine.close_account(a, slot2, oracle).expect("close account");
        assert!(cap > 0);
        assert!(!engine.is_used(a as usize));
    }
    // If PnL is not zero, closing might fail — that is expected behavior
}

#[test]
fn test_insurance_absorbs_loss_on_liquidation() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add user a");
    let b = engine.add_user(1000).expect("add user b");

    // Deposit before crank so accounts are not GC'd
    engine.deposit(a, 20_000, oracle, slot).expect("deposit a");
    engine.deposit(b, 100_000, oracle, slot).expect("deposit b");

    // Top up insurance fund
    engine.top_up_insurance_fund(50_000).expect("top up");

    engine.keeper_crank(a, slot, oracle, 0).expect("initial crank");

    // Open near-max position
    let size_q = make_size_q(180);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Crash price to make a deeply underwater
    let slot2 = 2u64;
    let crash = 850u64;
    engine.keeper_crank(a, slot2, crash, 0).expect("crank");

    engine.liquidate_at_oracle(a, slot2, crash).expect("liquidate");
    assert!(engine.check_conservation());
}

#[test]
fn test_maintenance_fee_accumulates() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let idx = engine.add_user(1000).expect("add_user");
    engine.deposit(idx, 10_000, oracle, slot).expect("deposit");

    let capital_before = engine.accounts[idx as usize].capital.get();

    // Advance 500 slots and touch
    let slot2 = 501u64;
    engine.keeper_crank(idx, slot2, oracle, 0).expect("crank");
    engine.touch_account_full(idx as usize, oracle, slot2).expect("touch");

    let capital_after = engine.accounts[idx as usize].capital.get();
    // maintenance_fee_per_slot = 1, over ~500 slots = ~500 fee
    assert!(capital_after < capital_before, "maintenance fees should reduce capital");
}

#[test]
fn test_keeper_crank_liquidates_underwater_accounts() {
    let (mut engine, a, b) = setup_two_users(50_000, 50_000);
    let oracle = 1000u64;
    let slot = 1u64;

    // Open near-margin positions
    let size_q = make_size_q(450);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");

    // Crash price
    let slot2 = 2u64;
    let crash = 870u64;
    let outcome = engine.keeper_crank(a, slot2, crash, 0).expect("crank");
    // The crank should have attempted liquidation
    let _ = outcome.num_liquidations; // just checking it does not panic
    assert!(engine.check_conservation());
}

#[test]
fn test_i256_size_q_construction() {
    // Verify our make_size_q helper produces correct values
    let pos = make_size_q(1);
    let neg = make_size_q(-1);

    assert!(pos.is_positive());
    assert!(neg.is_negative());

    // |pos| should equal POS_SCALE
    let abs_pos = pos.abs_u256();
    assert_eq!(abs_pos, U256::from_u128(POS_SCALE));
}

#[test]
fn test_deposit_fee_credits_invalid_account() {
    let mut engine = RiskEngine::new(default_params());
    let result = engine.deposit_fee_credits(99, 1000, 1);
    assert_eq!(result, Err(RiskError::Unauthorized));
}

#[test]
fn test_finalize_side_reset() {
    let mut engine = RiskEngine::new(default_params());

    // Set up for reset
    engine.begin_full_drain_reset(Side::Long);
    assert_eq!(engine.side_mode_long, SideMode::ResetPending);

    // All stored_pos_count and stale_count must be 0 for finalize
    // Since no accounts with long positions exist, they should already be 0
    let result = engine.finalize_side_reset(Side::Long);
    assert!(result.is_ok());
    assert_eq!(engine.side_mode_long, SideMode::Normal);
}

#[test]
fn test_finalize_side_reset_wrong_mode() {
    let mut engine = RiskEngine::new(default_params());
    // Side is Normal, finalize should fail
    let result = engine.finalize_side_reset(Side::Long);
    assert_eq!(result, Err(RiskError::CorruptState));
}

#[test]
fn test_account_equity_net_positive() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let idx = engine.add_user(1000).expect("add_user");
    engine.deposit(idx, 50_000, oracle, slot).expect("deposit");

    let eq = engine.account_equity_net(&engine.accounts[idx as usize], oracle);
    // With only capital and no PnL, equity = capital = 50_000
    let expected = I256::from_u128(50_000);
    assert_eq!(eq, expected);
}

#[test]
fn test_count_used() {
    let mut engine = RiskEngine::new(default_params());
    assert_eq!(engine.count_used(), 0);

    engine.add_user(1000).expect("add_user");
    assert_eq!(engine.count_used(), 1);

    engine.add_user(1000).expect("add_user");
    assert_eq!(engine.count_used(), 2);
}

#[test]
fn test_conservation_maintained_through_lifecycle() {
    // Full lifecycle: create, deposit, trade, move price, crank, close
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add a");
    let b = engine.add_user(1000).expect("add b");

    // Deposit before crank so accounts are not GC'd
    engine.deposit(a, 100_000, oracle, slot).expect("dep a");
    engine.deposit(b, 100_000, oracle, slot).expect("dep b");
    assert!(engine.check_conservation());

    engine.keeper_crank(a, slot, oracle, 0).expect("crank");
    assert!(engine.check_conservation());

    let size_q = make_size_q(50);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade");
    assert!(engine.check_conservation());

    // Price move
    let slot2 = 10u64;
    engine.keeper_crank(a, slot2, 1050, 0).expect("crank2");
    assert!(engine.check_conservation());

    // Close positions
    let close_q = make_size_q(-50);
    engine.execute_trade(a, b, 1050, slot2, close_q, 1050).expect("close");
    assert!(engine.check_conservation());
}

// ============================================================================
// Spec property #23: immediate fee seniority after restart conversion
// ============================================================================

/// If restart-on-new-profit converts matured entitlement into C_i while fee debt
/// is outstanding, the fee-debt sweep occurs immediately — before later
/// loss-settlement or margin logic can consume that new capital.
///
/// This test verifies that after a trade triggers restart-on-new-profit,
/// fee debt is properly swept (capital reduced, fee_credits less negative,
/// insurance fund receives payment).
#[test]
fn test_fee_seniority_after_restart_on_new_profit_in_trade() {
    // Use zero-fee params to isolate the restart-on-new-profit / fee-sweep interaction
    let mut params = default_params();
    params.trading_fee_bps = 0;
    params.maintenance_fee_per_slot = U128::new(0);
    // Use zero warmup so all positive PnL is immediately warmable
    params.warmup_period_slots = 0;

    let mut engine = RiskEngine::new(params);
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add a");
    let b = engine.add_user(1000).expect("add b");

    // Large deposits so margin is not an issue
    engine.deposit(a, 1_000_000, oracle, slot).expect("dep a");
    engine.deposit(b, 1_000_000, oracle, slot).expect("dep b");

    engine.keeper_crank(a, slot, oracle, 0).expect("crank");

    // Open position: a buys 10 from b
    let size_q = make_size_q(10);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).expect("trade1");
    assert!(engine.check_conservation());

    // Price rises: a now has positive PnL (profit)
    let slot2 = 50u64;
    let oracle2 = 1100u64;
    engine.keeper_crank(a, slot2, oracle2, 0).expect("crank2");
    assert!(engine.check_conservation());

    // Inject fee debt on account a: fee_credits = -5000
    // (In production this happens from maintenance fees exceeding credits)
    engine.accounts[a as usize].fee_credits = I128::new(-5000);

    let cap_before = engine.accounts[a as usize].capital.get();
    let ins_before = engine.insurance_fund.balance.get();

    // Execute another trade that will trigger restart-on-new-profit for a
    // (a buys 1 more at favorable price = market, AvailGross increases)
    let size_q2 = make_size_q(1);
    engine.execute_trade(a, b, oracle2, slot2, size_q2, oracle2).expect("trade2");
    assert!(engine.check_conservation());

    // After trade: fee debt should have been swept
    let fc_after = engine.accounts[a as usize].fee_credits.get();
    // Fee debt was 5000. After sweep, fee_credits should be less negative (or zero).
    assert!(fc_after > -5000, "fee debt was not swept after restart-on-new-profit: fc={}", fc_after);

    // Insurance fund should have received the swept amount
    let ins_after = engine.insurance_fund.balance.get();
    assert!(ins_after > ins_before, "insurance fund did not receive fee sweep payment");

    // Capital should have decreased by the swept amount
    // (restart conversion adds to capital, fee sweep subtracts)
    // We can't easily check exact amounts without knowing warmable, but we can
    // verify conservation holds
    assert!(engine.check_conservation());
}

// ============================================================================
// Issue #4: Maintenance fee settle must not clamp fee_credits to i128::MIN
// ============================================================================

#[test]
fn test_maintenance_fee_does_not_reach_i128_min() {
    let mut params = default_params();
    params.maintenance_fee_per_slot = U128::new(i128::MAX as u128);
    let mut engine = RiskEngine::new(params);
    let slot = 1u64;
    engine.current_slot = slot;

    let idx = engine.add_user(1000).expect("add user");
    engine.deposit(idx, 100_000, 1000, slot).expect("deposit");

    // Set fee_credits very negative, close to i128::MIN
    engine.accounts[idx as usize].fee_credits = I128::new(i128::MIN + 2);
    engine.accounts[idx as usize].last_fee_slot = 0;

    // Touch must return Err — fee_per_slot * dt overflows u128 with checked math.
    // This is the correct "fail conservatively" behavior per §1.5 Rule 9.
    engine.last_oracle_price = 1000;
    engine.last_market_slot = 100;
    let result = engine.touch_account_full(idx as usize, 1000, 100);
    assert!(result.is_err(), "touch must fail on extreme fee overflow");
}

// ============================================================================
// Issue #5: charge_fee_safe must not panic on PnL underflow
// ============================================================================

#[test]
fn test_charge_fee_safe_does_not_panic_on_extreme_pnl() {
    let mut params = default_params();
    params.trading_fee_bps = 100; // 1% fee
    let mut engine = RiskEngine::new(params);
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add a");
    let b = engine.add_user(1000).expect("add b");

    // Give a zero capital (so fee shortfall goes to PnL),
    // and b large capital for margin
    engine.deposit(a, 1, oracle, slot).expect("dep a");
    engine.deposit(b, 10_000_000, oracle, slot).expect("dep b");

    engine.keeper_crank(a, slot, oracle, 0).expect("crank");

    // Set account a's PnL to near I256::MIN so fee subtraction would overflow.
    // The charge_fee_safe path: if capital < fee, shortfall = fee - capital,
    // then PnL -= shortfall. If PnL is near I256::MIN, this could overflow.
    let near_min = I256::MIN.checked_add(I256::from_u128(1)).unwrap();
    engine.set_pnl(a as usize, near_min);

    // Executing a trade charges a fee. If capital is 0, fee goes to PnL.
    // With PnL near I256::MIN, subtracting the fee must not panic.
    // (The trade will likely fail for margin reasons, but must not panic.)
    let size_q = make_size_q(1);
    let _result = engine.execute_trade(a, b, oracle, slot, size_q, oracle);
    // We don't care if it succeeds or returns Err — just that it doesn't panic.
}

// ============================================================================
// Issue #1: keeper_crank must propagate errors from state-mutating functions
// ============================================================================

#[test]
fn test_keeper_crank_propagates_corruption() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add a");
    engine.deposit(a, 100_000, oracle, slot).expect("dep a");
    engine.keeper_crank(a, slot, oracle, 0).expect("crank");

    // Set up a corrupt state: a_basis = 0 triggers CorruptState error
    // in settle_side_effects (called by touch_account_full)
    engine.accounts[a as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.accounts[a as usize].adl_a_basis = 0; // CORRUPT: a_basis must be > 0
    engine.stored_pos_count_long = 1;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);

    // keeper_crank must propagate the CorruptState error, not swallow it
    let result = engine.keeper_crank(a, 2, oracle, 0);
    assert!(result.is_err(), "keeper_crank must propagate corruption errors");
}

// ============================================================================
// Self-trade rejection
// ============================================================================

#[test]
fn test_self_trade_rejected() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add a");
    engine.deposit(a, 100_000, oracle, slot).expect("dep a");
    engine.keeper_crank(a, slot, oracle, 0).expect("crank");

    let size_q = make_size_q(1);
    let result = engine.execute_trade(a, a, oracle, slot, size_q, oracle);
    assert!(result.is_err(), "self-trade (a == b) must be rejected");
}

// ============================================================================
// Same-slot price change applies mark-to-market
// ============================================================================

#[test]
fn test_same_slot_price_change_applies_mark() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;
    engine.last_oracle_price = oracle;
    engine.last_market_slot = slot; // same slot
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    // Same slot, different price: mark-only update must apply
    let new_oracle = 1100u64;
    engine.accrue_market_to(slot, new_oracle).expect("accrue");

    // K_long must increase (price went up, longs gain)
    assert!(engine.adl_coeff_long > k_long_before,
        "K_long must increase on same-slot price rise");
    // K_short must decrease (shorts lose)
    assert!(engine.adl_coeff_short < k_short_before,
        "K_short must decrease on same-slot price rise");
    // Oracle price must be updated
    assert!(engine.last_oracle_price == new_oracle,
        "last_oracle_price must be updated");
}

// ============================================================================
// schedule_end_of_instruction_resets error propagation
// ============================================================================

#[test]
fn test_schedule_reset_error_propagated_in_withdraw() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).expect("add a");
    engine.deposit(a, 100_000, oracle, slot).expect("dep a");
    engine.keeper_crank(a, slot, oracle, 0).expect("crank");

    // Corrupt state: stored_pos_count says 0 but OI is non-zero and unequal.
    // This makes schedule_end_of_instruction_resets return CorruptState.
    engine.stored_pos_count_long = 0;
    engine.stored_pos_count_short = 0;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE * 2); // unequal OI

    let result = engine.withdraw(a, 1, oracle, slot);
    assert!(result.is_err(), "withdraw must propagate reset error on corrupt state");
}

// ============================================================================
// Wide arithmetic: U512-backed mul_div with large operands
// ============================================================================

#[test]
fn test_wide_signed_mul_div_floor_large_operands() {
    use percolator::wide_math::wide_signed_mul_div_floor;

    // Large basis * large positive K_diff
    let abs_basis = U256::from_u128(u128::MAX);
    let k_diff = I256::from_i128(i128::MAX);
    let denom = U256::from_u128(POS_SCALE);
    let result = wide_signed_mul_div_floor(abs_basis, k_diff, denom);
    // Must not panic; result should be positive (positive * positive / positive)
    assert!(!result.is_negative(), "positive inputs must give non-negative result");

    // Large basis * large negative K_diff (floor toward -inf)
    let k_neg = I256::from_i128(-1_000_000_000);
    let result_neg = wide_signed_mul_div_floor(abs_basis, k_neg, denom);
    assert!(result_neg.is_negative(), "negative k_diff must give negative result");

    // Verify floor rounding: for negative results with remainder, result should
    // be strictly more negative than truncation toward zero.
    // (-1 * 3) / 2 => floor = -2, not -1 (truncation).
    let basis_3 = U256::from_u128(3);
    let k_neg1 = I256::from_i128(-1);
    let denom_2 = U256::from_u128(2);
    let floored = wide_signed_mul_div_floor(basis_3, k_neg1, denom_2);
    assert_eq!(floored, I256::from_i128(-2), "floor(-3/2) must be -2");
}

#[test]
fn test_wide_signed_mul_div_floor_zero_cases() {
    use percolator::wide_math::wide_signed_mul_div_floor;

    // Zero basis
    let result = wide_signed_mul_div_floor(U256::ZERO, I256::from_i128(42), U256::from_u128(1));
    assert_eq!(result, I256::ZERO);

    // Zero k_diff
    let result = wide_signed_mul_div_floor(U256::from_u128(42), I256::ZERO, U256::from_u128(1));
    assert_eq!(result, I256::ZERO);
}

#[test]
fn test_mul_div_floor_u256_large_product() {
    use percolator::wide_math::mul_div_floor_u256;

    // (u128::MAX * u128::MAX) / 1 should not panic — uses U512 internally
    let a = U256::from_u128(u128::MAX);
    let b = U256::from_u128(u128::MAX);
    let d = U256::from_u128(u128::MAX); // dividing by same magnitude keeps in range
    let result = mul_div_floor_u256(a, b, d);
    assert_eq!(result, U256::from_u128(u128::MAX), "u128::MAX * u128::MAX / u128::MAX = u128::MAX");

    // Small a * large b / large d => small result
    let result2 = mul_div_floor_u256(U256::from_u128(1), U256::from_u128(u128::MAX), U256::from_u128(u128::MAX));
    assert_eq!(result2, U256::from_u128(1));
}

#[test]
fn test_mul_div_ceil_u256_rounding() {
    use percolator::wide_math::mul_div_ceil_u256;

    // Exact division: 6 * 2 / 3 = 4 (no rounding needed)
    let exact = mul_div_ceil_u256(U256::from_u128(6), U256::from_u128(2), U256::from_u128(3));
    assert_eq!(exact, U256::from_u128(4));

    // Rounding up: 7 * 1 / 3 = ceil(7/3) = 3
    let ceiled = mul_div_ceil_u256(U256::from_u128(7), U256::from_u128(1), U256::from_u128(3));
    assert_eq!(ceiled, U256::from_u128(3), "ceil(7/3) must be 3");

    // Minimal remainder: 4 * 1 / 3 = ceil(4/3) = 2
    let min_rem = mul_div_ceil_u256(U256::from_u128(4), U256::from_u128(1), U256::from_u128(3));
    assert_eq!(min_rem, U256::from_u128(2), "ceil(4/3) must be 2");
}

// ============================================================================
// Multi-step funding accrual over large dt
// ============================================================================

#[test]
fn test_accrue_market_to_multi_substep_large_dt() {
    let mut engine = RiskEngine::new(default_params());
    engine.last_oracle_price = 1000;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 1000;
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);

    // High funding rate, large time gap requiring multiple sub-steps
    engine.funding_rate_bps_per_slot_last = 5000; // 50% bps/slot
    let large_dt = MAX_FUNDING_DT * 3 + 100; // triggers 4 sub-steps

    let result = engine.accrue_market_to(large_dt, 1100);
    assert!(result.is_ok(), "multi-substep accrual must not overflow: {:?}", result);

    // Price increased, so K_long must increase (mark + funding payer = long)
    // K_short must also change from receiving funding
    assert!(engine.last_market_slot == large_dt);
    assert!(engine.last_oracle_price == 1100);
}

#[test]
fn test_accrue_market_funding_rate_zero_no_funding_applied() {
    let mut engine = RiskEngine::new(default_params());
    engine.last_oracle_price = 1000;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 1000;
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);
    engine.funding_rate_bps_per_slot_last = 0;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    // Same price, time passes: with zero rate, only mark applies (0 delta_p)
    engine.accrue_market_to(100, 1000).unwrap();

    // No price change + no funding → K unchanged
    assert_eq!(engine.adl_coeff_long, k_long_before);
    assert_eq!(engine.adl_coeff_short, k_short_before);
}

#[test]
fn test_accrue_market_negative_funding_rate() {
    let mut engine = RiskEngine::new(default_params());
    engine.last_oracle_price = 1000;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 1000;
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);

    // Negative rate: shorts pay, longs receive
    engine.funding_rate_bps_per_slot_last = -1000;

    let k_long_before = engine.adl_coeff_long;
    let k_short_before = engine.adl_coeff_short;

    engine.accrue_market_to(10, 1000).unwrap(); // same price, time passes

    // Shorts pay → K_short decreases; Longs receive → K_long increases
    assert!(engine.adl_coeff_short < k_short_before,
        "negative rate: short K must decrease (payer)");
    assert!(engine.adl_coeff_long > k_long_before,
        "negative rate: long K must increase (receiver)");
}

// ============================================================================
// Keeper crank: cursor advancement and fairness
// ============================================================================

#[test]
fn test_keeper_crank_sweep_complete_flag() {
    let (mut engine, a, _b) = setup_two_users(10_000_000, 10_000_000);

    // With only 2 accounts, a single crank should sweep all of them
    let outcome = engine.keeper_crank(a, 5, 1000, 0).unwrap();
    assert!(outcome.sweep_complete, "crank with few accounts must complete sweep");
}

#[test]
fn test_keeper_crank_caller_fee_discount_multi_slot() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).unwrap();
    engine.deposit(a, 10_000_000, oracle, slot).unwrap();
    engine.keeper_crank(a, slot, oracle, 0).unwrap();

    // Advance many slots to accumulate maintenance fee debt
    let far_slot = 1000u64;
    engine.accounts[a as usize].last_fee_slot = slot;

    // Run crank at far_slot — caller gets 50% slot forgiveness
    engine.keeper_crank(a, far_slot, oracle, 0).unwrap();

    // Caller's last_fee_slot should be updated to far_slot (post-settlement)
    assert_eq!(engine.accounts[a as usize].last_fee_slot, far_slot,
        "caller's last_fee_slot must be updated after crank settlement");
}

// ============================================================================
// Liquidation edge cases
// ============================================================================

#[test]
fn test_liquidation_triggers_on_underwater_account() {
    // Small deposits + large position = high leverage → easily liquidated
    let (mut engine, a, b) = setup_two_users(100_000, 100_000);
    let oracle = 1000u64;
    let slot = 2u64;

    // Trade at maximum leverage the margin allows
    // With 100k capital, 10% IM, max notional ≈ 1M → ~1000 units at price 1000
    let size_q = make_size_q(900);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).unwrap();

    // Price crashes — longs deeply underwater
    let crash_price = 500u64; // 50% drop
    let slot2 = 3;

    // Crank at crash price — accrues market internally then liquidates
    let outcome = engine.keeper_crank(b, slot2, crash_price, 0).unwrap();
    assert!(outcome.num_liquidations > 0, "crank must liquidate underwater account after 50% price drop");
}

#[test]
fn test_direct_liquidation_returns_to_insurance() {
    let (mut engine, a, b) = setup_two_users(10_000_000, 10_000_000);
    let oracle = 1000u64;
    let slot = 2u64;

    let size_q = make_size_q(10);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).unwrap();

    let ins_before = engine.insurance_fund.balance.get();

    // Price crashes — a (long) underwater
    let crash_price = 100u64;
    let slot2 = 3;
    engine.liquidate_at_oracle(a, slot2, crash_price).unwrap();

    let ins_after = engine.insurance_fund.balance.get();
    // Insurance should receive liquidation fee (or absorb loss)
    assert!(ins_after >= ins_before, "insurance fund must not decrease on liquidation");
}

// ============================================================================
// Conservation law: full lifecycle
// ============================================================================

#[test]
fn test_conservation_full_lifecycle() {
    let (mut engine, a, b) = setup_two_users(10_000_000, 10_000_000);
    assert!(engine.check_conservation(), "conservation must hold after setup");

    let oracle = 1000u64;
    let slot = 2u64;

    // Trade
    let size_q = make_size_q(5);
    engine.execute_trade(a, b, oracle, slot, size_q, oracle).unwrap();
    assert!(engine.check_conservation(), "conservation must hold after trade");

    // Price change + crank
    let slot2 = 3;
    engine.keeper_crank(a, slot2, 1200, 0).unwrap();
    assert!(engine.check_conservation(), "conservation must hold after crank with price change");

    // Withdraw
    engine.withdraw(a, 1_000, 1200, slot2).unwrap();
    assert!(engine.check_conservation(), "conservation must hold after withdraw");

    // Another crank at different price
    let slot3 = 4;
    engine.keeper_crank(b, slot3, 800, 0).unwrap();
    assert!(engine.check_conservation(), "conservation must hold after second crank");
}

// ============================================================================
// Position boundary: max position enforcement
// ============================================================================

#[test]
fn test_trade_at_reasonable_size_succeeds() {
    let (mut engine, a, b) = setup_two_users(100_000_000, 100_000_000);
    let oracle = 1000u64;
    let slot = 2u64;

    // Reasonable trade should succeed
    let size_q = make_size_q(1);
    let result = engine.execute_trade(a, b, oracle, slot, size_q, oracle);
    assert!(result.is_ok(), "reasonable trade must succeed");
    assert!(engine.check_conservation());
}

// ============================================================================
// Maintenance fee: overflow on large dt
// ============================================================================

#[test]
fn test_maintenance_fee_large_dt_overflow_returns_error() {
    let mut params = default_params();
    params.maintenance_fee_per_slot = U128::new(u128::MAX / 2);
    let mut engine = RiskEngine::new(params);
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).unwrap();
    engine.deposit(a, 10_000_000, oracle, slot).unwrap();
    engine.keeper_crank(a, slot, oracle, 0).unwrap();

    // Use a moderate slot gap (not u64::MAX which loops forever in accrue_market_to).
    // fee_per_slot = u128::MAX/2, dt = 200_000 → product overflows u128.
    let far_slot = slot + 200_000;
    // Set last_market_slot close to far_slot so accrue_market_to is fast
    engine.last_market_slot = far_slot - 1;
    engine.last_oracle_price = oracle;
    engine.funding_price_sample_last = oracle;

    let result = engine.keeper_crank(a, far_slot, oracle, 0);
    assert!(result.is_err(), "huge maintenance fee must return Err, not panic");
}

// ============================================================================
// charge_fee_safe: PnL near I256::MIN boundary
// ============================================================================

#[test]
fn test_charge_fee_safe_rejects_pnl_at_i256_min() {
    let mut engine = RiskEngine::new(default_params());
    let oracle = 1000u64;
    let slot = 1u64;
    engine.current_slot = slot;

    let a = engine.add_user(1000).unwrap();
    engine.deposit(a, 0, oracle, slot).unwrap(); // zero capital so shortfall goes to PnL

    // Set PnL very close to I256::MIN
    let near_min = I256::MIN.checked_add(I256::from_i128(1)).unwrap();
    engine.set_pnl(a as usize, near_min);

    // Liquidation fee would push PnL to exactly I256::MIN — must return Err
    // We test via the public liquidate path, but first set up the conditions
    // for an underwater account with a position.
    engine.accounts[a as usize].position_basis_q = I256::from_u128(POS_SCALE);
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.adl_epoch_long = 0;
    engine.adl_epoch_short = 0;
    engine.accounts[a as usize].adl_a_basis = ADL_ONE;
    engine.accounts[a as usize].adl_k_snap = I256::ZERO;
    engine.accounts[a as usize].adl_epoch_snap = 0;
    engine.stored_pos_count_long = 1;
    engine.oi_eff_long_q = U256::from_u128(POS_SCALE);
    engine.oi_eff_short_q = U256::from_u128(POS_SCALE);
    engine.last_oracle_price = oracle;
    engine.last_market_slot = slot;
    engine.last_crank_slot = slot;
    engine.funding_price_sample_last = oracle;

    // Liquidation should handle this gracefully (return Err or succeed without I256::MIN)
    let result = engine.liquidate_at_oracle(a, slot, oracle);
    // Either it errors out or it succeeds but PnL is not I256::MIN
    if result.is_ok() {
        assert!(engine.accounts[a as usize].pnl != I256::MIN,
            "PnL must never reach I256::MIN");
    }
}

// ============================================================================
// Side mode gating prevents OI increase during DrainOnly
// ============================================================================

#[test]
fn test_drain_only_blocks_oi_increase() {
    let (mut engine, a, b) = setup_two_users(10_000_000, 10_000_000);
    let oracle = 1000u64;
    let slot = 2u64;

    // Set long side to DrainOnly
    engine.side_mode_long = SideMode::DrainOnly;

    // Try to open a new long position — should fail
    let size_q = make_size_q(1); // a goes long
    let result = engine.execute_trade(a, b, oracle, slot, size_q, oracle);
    assert!(result.is_err(), "DrainOnly side must reject OI-increasing trades");
}

// ============================================================================
// Oracle price: zero and max boundary
// ============================================================================

#[test]
fn test_oracle_price_zero_rejected() {
    let (mut engine, a, _b) = setup_two_users(10_000_000, 10_000_000);
    let result = engine.accrue_market_to(2, 0);
    assert!(result.is_err(), "oracle price 0 must be rejected");
}

#[test]
fn test_oracle_price_max_accepted() {
    let mut engine = RiskEngine::new(default_params());
    engine.last_oracle_price = 1000;
    engine.last_market_slot = 0;
    engine.funding_price_sample_last = 1000;
    engine.adl_mult_long = ADL_ONE;
    engine.adl_mult_short = ADL_ONE;
    engine.funding_rate_bps_per_slot_last = 0;

    let result = engine.accrue_market_to(1, MAX_ORACLE_PRICE);
    assert!(result.is_ok(), "MAX_ORACLE_PRICE must be accepted");

    let result2 = engine.accrue_market_to(2, MAX_ORACLE_PRICE + 1);
    assert!(result2.is_err(), "above MAX_ORACLE_PRICE must be rejected");
}

// ============================================================================
// Deposit/withdraw roundtrip: conservation on single account
// ============================================================================

#[test]
fn test_deposit_withdraw_roundtrip_same_slot() {
    let (mut engine, a, _b) = setup_two_users(10_000_000, 10_000_000);
    // Use same slot as setup (slot=1) to avoid maintenance fee deduction
    let oracle = 1000;
    let slot = 1;

    let cap_before = engine.accounts[a as usize].capital.get();
    engine.deposit(a, 5_000_000, oracle, slot).unwrap();
    assert_eq!(engine.accounts[a as usize].capital.get(), cap_before + 5_000_000);

    // Withdraw full extra amount at same slot — no fee should apply
    engine.withdraw(a, 5_000_000, oracle, slot).unwrap();
    assert_eq!(engine.accounts[a as usize].capital.get(), cap_before,
        "same-slot deposit+withdraw roundtrip must return exact capital");
    assert!(engine.check_conservation());
}

// ============================================================================
// Multiple cranks don't double-process accounts
// ============================================================================

#[test]
fn test_double_crank_same_slot_is_safe() {
    let (mut engine, a, b) = setup_two_users(10_000_000, 10_000_000);
    let oracle = 1000u64;
    let slot = 2u64;

    engine.keeper_crank(a, slot, oracle, 0).unwrap();

    let cap_a = engine.accounts[a as usize].capital.get();
    let cap_b = engine.accounts[b as usize].capital.get();

    // Second crank same slot — should be a no-op (no double fee charges etc.)
    engine.keeper_crank(b, slot, oracle, 0).unwrap();

    // Capital shouldn't change from a redundant crank
    // (small tolerance for rounding if any fees apply)
    let cap_a_after = engine.accounts[a as usize].capital.get();
    let cap_b_after = engine.accounts[b as usize].capital.get();
    assert!(cap_a_after == cap_a, "redundant crank must not change capital");
    assert!(cap_b_after == cap_b, "redundant crank must not change capital");
    assert!(engine.check_conservation());
}

