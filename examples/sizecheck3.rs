fn main() {
    println!("ACCOUNTS_OFF={}", core::mem::offset_of!(percolator::RiskEngine, accounts));
    println!("ADL_MULT_LONG={}", core::mem::offset_of!(percolator::RiskEngine, adl_mult_long));
    println!("ADL_MULT_SHORT={}", core::mem::offset_of!(percolator::RiskEngine, adl_mult_short));
    println!("ADL_EPOCH_LONG={}", core::mem::offset_of!(percolator::RiskEngine, adl_epoch_long));
    println!("ADL_EPOCH_SHORT={}", core::mem::offset_of!(percolator::RiskEngine, adl_epoch_short));
    println!("C_TOT={}", core::mem::offset_of!(percolator::RiskEngine, c_tot));
    println!("PNL_POS_TOT={}", core::mem::offset_of!(percolator::RiskEngine, pnl_pos_tot));
    println!("NUM_USED={}", core::mem::offset_of!(percolator::RiskEngine, num_used_accounts));
    println!("NEG_PNL_COUNT={}", core::mem::offset_of!(percolator::RiskEngine, neg_pnl_account_count));
    println!("BITMAP={}", core::mem::offset_of!(percolator::RiskEngine, used));
    println!("ENGINE_SIZE={}", std::mem::size_of::<percolator::RiskEngine>());
}
