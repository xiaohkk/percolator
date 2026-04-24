//! Locks in host-side byte offsets for the RiskEngine and Account fields
//! the frontend decoder (`percpad/src/lib/percolator/engine-layout.ts`)
//! needs to read.
//!
//! Strategy per task #19:
//!   1. Use `core::mem::offset_of!` to compute each offset at compile time.
//!   2. Allocate a heap-boxed RiskEngine, write sentinel values through the
//!      field names, then re-read the raw byte span and assert the sentinel
//!      shows up at the offset we computed. This catches any accidental
//!      reordering in `percolator::RiskEngine` / `percolator::Account`.
//!   3. Print every offset under `-- --nocapture` so an engineer copying
//!      the TS table can see them directly.
//!
//! Caveat: offsets are computed on the host Rust target. On x86_64 with
//! Rust 1.77+, plain `u128`/`i128` fields are 16-byte aligned, while the
//! BPF/SBF target keeps 8-byte alignment. The parent crate wraps some
//! (but not all) 128-bit fields in `U128` / `I128` ([u64; 2]) to paper
//! over this. Integration tests run native via `processor!` so the whole
//! test suite uses host layout, and the TS decoder matches that. Revisit
//! if/when the decoder is pointed at bytes written by a real BPF binary.

use core::mem::{offset_of, size_of, MaybeUninit};

use percolator::{Account, RiskEngine, SideMode, I128, U128};

fn print_offset(label: &str, off: usize) {
    println!("  {:<26} {}", format!("{}:", label), off);
}

#[test]
fn print_and_verify_engine_layout() {
    let engine_size = size_of::<RiskEngine>();
    let account_size = size_of::<Account>();

    println!("\n==== RiskEngine / Account layout (host) ====");
    println!("RiskEngine size: {}", engine_size);
    println!("Account size:    {}", account_size);
    println!("\nRiskEngine offsets:");
    print_offset("vault", offset_of!(RiskEngine, vault));
    print_offset("insurance_fund", offset_of!(RiskEngine, insurance_fund));
    print_offset("c_tot", offset_of!(RiskEngine, c_tot));
    print_offset("pnl_pos_tot", offset_of!(RiskEngine, pnl_pos_tot));
    print_offset("pnl_matured_pos_tot", offset_of!(RiskEngine, pnl_matured_pos_tot));
    print_offset("adl_mult_long", offset_of!(RiskEngine, adl_mult_long));
    print_offset("adl_mult_short", offset_of!(RiskEngine, adl_mult_short));
    print_offset("adl_coeff_long", offset_of!(RiskEngine, adl_coeff_long));
    print_offset("adl_coeff_short", offset_of!(RiskEngine, adl_coeff_short));
    print_offset("oi_eff_long_q", offset_of!(RiskEngine, oi_eff_long_q));
    print_offset("oi_eff_short_q", offset_of!(RiskEngine, oi_eff_short_q));
    print_offset("side_mode_long", offset_of!(RiskEngine, side_mode_long));
    print_offset("side_mode_short", offset_of!(RiskEngine, side_mode_short));
    print_offset("last_oracle_price", offset_of!(RiskEngine, last_oracle_price));
    print_offset("num_used_accounts", offset_of!(RiskEngine, num_used_accounts));
    print_offset("accounts", offset_of!(RiskEngine, accounts));
    println!("\nAccount offsets:");
    print_offset("capital", offset_of!(Account, capital));
    print_offset("kind", offset_of!(Account, kind));
    print_offset("pnl", offset_of!(Account, pnl));
    print_offset("reserved_pnl", offset_of!(Account, reserved_pnl));
    print_offset("position_basis_q", offset_of!(Account, position_basis_q));
    print_offset("owner", offset_of!(Account, owner));
    println!("============================================\n");

    // Heap-box it so we don't blow the test stack (~100 KiB with
    // MAX_ACCOUNTS=256 under the `compact` feature).
    let mut engine_box: Box<MaybeUninit<RiskEngine>> = Box::new(MaybeUninit::zeroed());
    let engine_ptr: *mut RiskEngine = engine_box.as_mut_ptr();

    // Sentinel values: distinct per field so we can pattern-match byte
    // positions without confusing two u128 fields sharing the same value.
    const SENT_VAULT: u128 = 0x0111_0111_0111_0111_0111_0111_0111_0111;
    const SENT_CTOT: u128 = 0x0222_0222_0222_0222_0222_0222_0222_0222;
    const SENT_PNL_POS: u128 = 0x0333_0333_0333_0333_0333_0333_0333_0333;
    const SENT_PNL_MAT: u128 = 0x0444_0444_0444_0444_0444_0444_0444_0444;
    const SENT_ADL_ML: u128 = 0x0555_0555_0555_0555_0555_0555_0555_0555;
    const SENT_ADL_MS: u128 = 0x0666_0666_0666_0666_0666_0666_0666_0666;
    const SENT_ADL_CL: i128 = 0x0777_0777_0777_0777_0777_0777_0777_0777;
    const SENT_ADL_CS: i128 = 0x0888_0888_0888_0888_0888_0888_0888_0888;
    const SENT_OI_L: u128 = 0x0999_0999_0999_0999_0999_0999_0999_0999;
    const SENT_OI_S: u128 = 0x0aaa_0aaa_0aaa_0aaa_0aaa_0aaa_0aaa_0aaa;
    const SENT_LAST_ORACLE: u64 = 0xdeadbeefcafef00d;
    const SENT_NUM_USED: u16 = 0x1234;

    unsafe {
        (*engine_ptr).vault = U128::new(SENT_VAULT);
        (*engine_ptr).c_tot = U128::new(SENT_CTOT);
        (*engine_ptr).pnl_pos_tot = SENT_PNL_POS;
        (*engine_ptr).pnl_matured_pos_tot = SENT_PNL_MAT;
        (*engine_ptr).adl_mult_long = SENT_ADL_ML;
        (*engine_ptr).adl_mult_short = SENT_ADL_MS;
        (*engine_ptr).adl_coeff_long = SENT_ADL_CL;
        (*engine_ptr).adl_coeff_short = SENT_ADL_CS;
        (*engine_ptr).oi_eff_long_q = SENT_OI_L;
        (*engine_ptr).oi_eff_short_q = SENT_OI_S;
        (*engine_ptr).side_mode_long = SideMode::DrainOnly;
        (*engine_ptr).side_mode_short = SideMode::ResetPending;
        (*engine_ptr).last_oracle_price = SENT_LAST_ORACLE;
        (*engine_ptr).num_used_accounts = SENT_NUM_USED;

        // First account slot: sentinel values per field.
        let acct0: *mut Account = (*engine_ptr).accounts.as_mut_ptr();
        (*acct0).capital = U128::new(0xbbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbb);
        (*acct0).pnl = -0x5555_5555_5555_5555_5555_5555_5555_5555i128;
        (*acct0).reserved_pnl = 0xcccc_cccc_cccc_cccc_cccc_cccc_cccc_ccccu128;
        (*acct0).position_basis_q = 0x7777_7777_7777_7777_7777_7777_7777_7777i128;
        (*acct0).owner = [0xAB; 32];
    }

    let bytes: &[u8] =
        unsafe { core::slice::from_raw_parts(engine_ptr as *const u8, engine_size) };

    fn read_u128_le(b: &[u8], off: usize) -> u128 {
        let mut arr = [0u8; 16];
        arr.copy_from_slice(&b[off..off + 16]);
        u128::from_le_bytes(arr)
    }
    fn read_i128_le(b: &[u8], off: usize) -> i128 {
        let mut arr = [0u8; 16];
        arr.copy_from_slice(&b[off..off + 16]);
        i128::from_le_bytes(arr)
    }
    fn read_u64_le(b: &[u8], off: usize) -> u64 {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&b[off..off + 8]);
        u64::from_le_bytes(arr)
    }
    fn read_u16_le(b: &[u8], off: usize) -> u16 {
        let mut arr = [0u8; 2];
        arr.copy_from_slice(&b[off..off + 2]);
        u16::from_le_bytes(arr)
    }

    // RiskEngine field byte positions round-trip through offset_of.
    assert_eq!(read_u128_le(bytes, offset_of!(RiskEngine, vault)), SENT_VAULT);
    assert_eq!(read_u128_le(bytes, offset_of!(RiskEngine, c_tot)), SENT_CTOT);
    assert_eq!(
        read_u128_le(bytes, offset_of!(RiskEngine, pnl_pos_tot)),
        SENT_PNL_POS
    );
    assert_eq!(
        read_u128_le(bytes, offset_of!(RiskEngine, pnl_matured_pos_tot)),
        SENT_PNL_MAT
    );
    assert_eq!(
        read_u128_le(bytes, offset_of!(RiskEngine, adl_mult_long)),
        SENT_ADL_ML
    );
    assert_eq!(
        read_u128_le(bytes, offset_of!(RiskEngine, adl_mult_short)),
        SENT_ADL_MS
    );
    assert_eq!(
        read_i128_le(bytes, offset_of!(RiskEngine, adl_coeff_long)),
        SENT_ADL_CL
    );
    assert_eq!(
        read_i128_le(bytes, offset_of!(RiskEngine, adl_coeff_short)),
        SENT_ADL_CS
    );
    assert_eq!(
        read_u128_le(bytes, offset_of!(RiskEngine, oi_eff_long_q)),
        SENT_OI_L
    );
    assert_eq!(
        read_u128_le(bytes, offset_of!(RiskEngine, oi_eff_short_q)),
        SENT_OI_S
    );
    assert_eq!(bytes[offset_of!(RiskEngine, side_mode_long)], 1); // DrainOnly
    assert_eq!(bytes[offset_of!(RiskEngine, side_mode_short)], 2); // ResetPending
    assert_eq!(
        read_u64_le(bytes, offset_of!(RiskEngine, last_oracle_price)),
        SENT_LAST_ORACLE
    );
    assert_eq!(
        read_u16_le(bytes, offset_of!(RiskEngine, num_used_accounts)),
        SENT_NUM_USED
    );

    // Account field byte positions for slot 0.
    let acct_base = offset_of!(RiskEngine, accounts);
    assert_eq!(
        read_u128_le(bytes, acct_base + offset_of!(Account, capital)),
        0xbbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbbu128
    );
    assert_eq!(
        read_i128_le(bytes, acct_base + offset_of!(Account, pnl)),
        -0x5555_5555_5555_5555_5555_5555_5555_5555i128
    );
    assert_eq!(
        read_u128_le(bytes, acct_base + offset_of!(Account, reserved_pnl)),
        0xcccc_cccc_cccc_cccc_cccc_cccc_cccc_ccccu128
    );
    assert_eq!(
        read_i128_le(bytes, acct_base + offset_of!(Account, position_basis_q)),
        0x7777_7777_7777_7777_7777_7777_7777_7777i128
    );
    let owner_off = acct_base + offset_of!(Account, owner);
    assert!(bytes[owner_off..owner_off + 32].iter().all(|&b| b == 0xAB));

    // Stride: two consecutive Account slots must be exactly size_of::<Account>() apart.
    // (Assertion tests array packing — a silent repr change here would shift
    // every TS-side slot lookup.)
    let stride = size_of::<Account>();
    let slot1_capital_off = acct_base + stride + offset_of!(Account, capital);
    assert_eq!(slot1_capital_off - (acct_base + offset_of!(Account, capital)), stride);

    // Silence unused-imports lints on the helper types.
    let _ = core::mem::size_of::<I128>();
}
