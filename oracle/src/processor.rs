//! Instruction dispatch + source-kind readers.
//!
//! Every Update is permissionless: read the source account bytes at
//! well-known offsets, compute an instantaneous price, append to the ring
//! buffer, republish `price_lamports_per_token = median(ring)`.
//!
//! Graduation: the pump.fun bonding curve carries a `complete: bool` byte at
//! offset 0xA6. When set, Graduate flips `graduated = 1` on the feed;
//! subsequent Updates against the bonding curve are rejected, and the
//! wrapper must call ConvertSource to repoint the feed at the AMM.

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    msg,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::{
    error::OracleError,
    instruction::{
        ConvertSourceArgs, InitializeFeedArgs, OracleInstruction,
    },
    state::{ring_median, Feed, SourceKind, RING_LEN},
};

pub struct Processor;

impl Processor {
    pub fn process(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        instruction_data: &[u8],
    ) -> ProgramResult {
        let ix = OracleInstruction::unpack(instruction_data)?;
        match ix {
            OracleInstruction::InitializeFeed(a) => {
                Self::process_initialize_feed(program_id, accounts, a)
            }
            OracleInstruction::Update => Self::process_update(program_id, accounts),
            OracleInstruction::Graduate => Self::process_graduate(program_id, accounts),
            OracleInstruction::ConvertSource(a) => {
                Self::process_convert_source(program_id, accounts, a)
            }
        }
    }

    fn process_initialize_feed(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: InitializeFeedArgs,
    ) -> ProgramResult {
        // Accounts:
        //   0: [writable] feed (must be program-owned and pre-allocated)
        //   1: []         source (identity + reserves validated here)
        let iter = &mut accounts.iter();
        let feed_ai = next_account_info(iter)?;
        let source_ai = next_account_info(iter)?;

        if !feed_ai.is_writable {
            return Err(OracleError::AccountNotWritable.into());
        }
        if feed_ai.owner != program_id {
            return Err(OracleError::FeedSizeMismatch.into());
        }
        if feed_ai.data_len() < Feed::LEN {
            return Err(OracleError::FeedSizeMismatch.into());
        }

        let source_kind =
            SourceKind::from_u8(args.source_kind).ok_or(OracleError::InvalidSourceKind)?;
        if source_ai.key.to_bytes() != args.source {
            return Err(OracleError::WrongSourceAccount.into());
        }

        // Existing feed must not already be initialized.
        {
            let data = feed_ai.try_borrow_data()?;
            let existing = Feed::read_from(&data[..Feed::LEN])?;
            if existing.is_initialized() {
                return Err(OracleError::FeedAlreadyInitialized.into());
            }
        }

        // Read an initial instantaneous price so bytes 0..8 are valid from
        // tick zero — otherwise the Percolator program's legacy
        // oracle-reads would see zero right after init.
        let clock = Clock::get()?;
        let init_price = read_source_price(source_kind, source_ai)?;
        if init_price == 0 {
            return Err(OracleError::StaleSource.into());
        }

        let mut feed = Feed {
            price_lamports_per_token: init_price,
            last_update_slot: clock.slot,
            mint: Pubkey::new_from_array(args.mint),
            source: Pubkey::new_from_array(args.source),
            source_kind: args.source_kind,
            graduated: 0,
            initialized: 1,
            ring_idx: 0,
            _pad: [0; 4],
            ring_buffer: [0u64; RING_LEN],
        };
        feed.ring_buffer[0] = init_price;
        feed.ring_idx = 1;

        let mut data = feed_ai.try_borrow_mut_data()?;
        feed.write_into(&mut data[..Feed::LEN])?;

        msg!(
            "InitializeFeed: feed={}, mint={}, kind={}, price={}",
            feed_ai.key,
            Pubkey::new_from_array(args.mint),
            args.source_kind,
            init_price,
        );
        Ok(())
    }

    fn process_update(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
        // Accounts:
        //   0: [writable] feed
        //   1: []         source (identity-checked against feed.source)
        let iter = &mut accounts.iter();
        let feed_ai = next_account_info(iter)?;
        let source_ai = next_account_info(iter)?;

        if !feed_ai.is_writable {
            return Err(OracleError::AccountNotWritable.into());
        }
        if feed_ai.owner != program_id {
            return Err(OracleError::FeedSizeMismatch.into());
        }
        if feed_ai.data_len() < Feed::LEN {
            return Err(OracleError::FeedSizeMismatch.into());
        }

        let mut feed = {
            let data = feed_ai.try_borrow_data()?;
            Feed::read_from(&data[..Feed::LEN])?
        };
        if !feed.is_initialized() {
            return Err(OracleError::FeedNotInitialized.into());
        }
        if feed.source != *source_ai.key {
            return Err(OracleError::WrongSourceAccount.into());
        }
        let source_kind = feed
            .source_kind_enum()
            .ok_or(OracleError::InvalidSourceKind)?;

        // Bonding-curve feeds that have graduated MUST be migrated via
        // ConvertSource; refuse further updates against the dead curve.
        if source_kind == SourceKind::PumpBonding && feed.is_graduated() {
            return Err(OracleError::AlreadyGraduated.into());
        }

        let instant_price = read_source_price(source_kind, source_ai)?;
        if instant_price == 0 {
            return Err(OracleError::StaleSource.into());
        }

        // Append to ring.
        feed.ring_buffer[feed.ring_idx as usize] = instant_price;
        feed.ring_idx = ((feed.ring_idx as usize + 1) % RING_LEN) as u8;

        // Republish median.
        let median =
            ring_median(&feed.ring_buffer).ok_or(OracleError::StaleSource)?;
        feed.price_lamports_per_token = median;

        let clock = Clock::get()?;
        feed.last_update_slot = clock.slot;

        let mut data = feed_ai.try_borrow_mut_data()?;
        feed.write_into(&mut data[..Feed::LEN])?;

        msg!(
            "Update: feed={}, instant={}, median={}, slot={}",
            feed_ai.key,
            instant_price,
            median,
            clock.slot,
        );
        Ok(())
    }

    fn process_graduate(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
        // Accounts:
        //   0: [writable] feed
        //   1: []         source (the bonding-curve account — we check the
        //                         `complete` byte at offset 0xA6)
        let iter = &mut accounts.iter();
        let feed_ai = next_account_info(iter)?;
        let source_ai = next_account_info(iter)?;

        if !feed_ai.is_writable {
            return Err(OracleError::AccountNotWritable.into());
        }
        if feed_ai.owner != program_id {
            return Err(OracleError::FeedSizeMismatch.into());
        }
        let mut feed = {
            let data = feed_ai.try_borrow_data()?;
            Feed::read_from(&data[..Feed::LEN])?
        };
        if !feed.is_initialized() {
            return Err(OracleError::FeedNotInitialized.into());
        }
        if feed.source != *source_ai.key {
            return Err(OracleError::WrongSourceAccount.into());
        }
        if feed.is_graduated() {
            return Err(OracleError::AlreadyGraduated.into());
        }
        let kind = feed
            .source_kind_enum()
            .ok_or(OracleError::InvalidSourceKind)?;
        if kind != SourceKind::PumpBonding {
            // Graduation only applies to bonding-curve feeds.
            return Err(OracleError::InvalidSourceKind.into());
        }

        // Bonding-curve layout: `complete: bool` at offset 0xA6. We accept
        // any non-zero byte as "complete" since the canonical layout stores
        // it as a `bool`.
        let source_data = source_ai.try_borrow_data()?;
        let complete_offset = 0xA6usize;
        if source_data.len() <= complete_offset {
            return Err(OracleError::StaleSource.into());
        }
        if source_data[complete_offset] == 0 {
            return Err(OracleError::NotGraduated.into());
        }
        drop(source_data);

        feed.graduated = 1;
        let mut data = feed_ai.try_borrow_mut_data()?;
        feed.write_into(&mut data[..Feed::LEN])?;
        msg!("Graduate: feed={}", feed_ai.key);
        Ok(())
    }

    fn process_convert_source(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: ConvertSourceArgs,
    ) -> ProgramResult {
        // Accounts:
        //   0: [writable] feed
        //   1: []         new_source (will become feed.source)
        let iter = &mut accounts.iter();
        let feed_ai = next_account_info(iter)?;
        let new_source_ai = next_account_info(iter)?;

        if !feed_ai.is_writable {
            return Err(OracleError::AccountNotWritable.into());
        }
        if feed_ai.owner != program_id {
            return Err(OracleError::FeedSizeMismatch.into());
        }

        let new_kind = SourceKind::from_u8(args.new_kind)
            .ok_or(OracleError::InvalidSourceKind)?;
        if new_source_ai.key.to_bytes() != args.new_source {
            return Err(OracleError::WrongSourceAccount.into());
        }

        let mut feed = {
            let data = feed_ai.try_borrow_data()?;
            Feed::read_from(&data[..Feed::LEN])?
        };
        if !feed.is_initialized() {
            return Err(OracleError::FeedNotInitialized.into());
        }
        // Only graduated bonding-curve feeds may be converted. (An AMM feed
        // is already live — no conversion needed.)
        if !feed.is_graduated() {
            return Err(OracleError::ConvertRequiresGraduation.into());
        }

        // Read a fresh price from the new source to bootstrap the ring.
        let fresh = read_source_price(new_kind, new_source_ai)?;
        if fresh == 0 {
            return Err(OracleError::StaleSource.into());
        }

        feed.source = Pubkey::new_from_array(args.new_source);
        feed.source_kind = args.new_kind;
        // Reset the ring: the bonding-curve prices are no longer
        // representative. The median over the new AMM converges as fresh
        // Updates arrive.
        feed.ring_buffer = [0u64; RING_LEN];
        feed.ring_buffer[0] = fresh;
        feed.ring_idx = 1;
        feed.price_lamports_per_token = fresh;

        let clock = Clock::get()?;
        feed.last_update_slot = clock.slot;

        let mut data = feed_ai.try_borrow_mut_data()?;
        feed.write_into(&mut data[..Feed::LEN])?;
        msg!(
            "ConvertSource: feed={}, new_kind={}, fresh={}",
            feed_ai.key,
            args.new_kind,
            fresh,
        );
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Source-kind readers
// ----------------------------------------------------------------------

/// Dispatch to the per-source reader. Output is an instantaneous
/// lamports-per-token price (u64). Returns 0 iff the source's reserves /
/// state indicate a stale pool — callers should treat 0 as `StaleSource`.
fn read_source_price(
    kind: SourceKind,
    source: &AccountInfo,
) -> Result<u64, OracleError> {
    match kind {
        SourceKind::PumpBonding => read_pump_bonding(source),
        SourceKind::PumpSwap => read_pumpswap(source),
        SourceKind::RaydiumCpmm => read_raydium_cpmm(source),
        SourceKind::MeteoraDlmm => read_meteora_dlmm(source),
    }
}

/// pump.fun bonding curve:
///   0x00: discriminator (8 bytes)
///   0x08: virtual_sol_reserves   (u64 LE)
///   0x10: virtual_token_reserves (u64 LE)
/// Price = sol_reserves * POS_SCALE / token_reserves (lamports per token,
/// where token is in smallest unit).
fn read_pump_bonding(source: &AccountInfo) -> Result<u64, OracleError> {
    let data = source.try_borrow_data().map_err(|_| OracleError::StaleSource)?;
    if data.len() < 0x18 {
        return Err(OracleError::StaleSource);
    }
    let sol_reserves = u64::from_le_bytes(data[0x08..0x10].try_into().unwrap());
    let token_reserves = u64::from_le_bytes(data[0x10..0x18].try_into().unwrap());
    if sol_reserves == 0 || token_reserves == 0 {
        return Err(OracleError::StaleSource);
    }
    amm_price(sol_reserves, token_reserves)
}

/// PumpSwap AMM:
///   0x00: discriminator
///   0x08: base_reserve  (u64 LE)
///   0x10: quote_reserve (u64 LE)
/// Price = quote_reserve / base_reserve (same shape as CPMM).
fn read_pumpswap(source: &AccountInfo) -> Result<u64, OracleError> {
    let data = source.try_borrow_data().map_err(|_| OracleError::StaleSource)?;
    if data.len() < 0x18 {
        return Err(OracleError::StaleSource);
    }
    let base = u64::from_le_bytes(data[0x08..0x10].try_into().unwrap());
    let quote = u64::from_le_bytes(data[0x10..0x18].try_into().unwrap());
    if base == 0 || quote == 0 {
        return Err(OracleError::StaleSource);
    }
    amm_price(quote, base)
}

/// Raydium CPMM pool account (simplified):
///   0x00: discriminator
///   0x08: base_reserve  (u64 LE)
///   0x10: quote_reserve (u64 LE)
fn read_raydium_cpmm(source: &AccountInfo) -> Result<u64, OracleError> {
    let data = source.try_borrow_data().map_err(|_| OracleError::StaleSource)?;
    if data.len() < 0x18 {
        return Err(OracleError::StaleSource);
    }
    let base = u64::from_le_bytes(data[0x08..0x10].try_into().unwrap());
    let quote = u64::from_le_bytes(data[0x10..0x18].try_into().unwrap());
    if base == 0 || quote == 0 {
        return Err(OracleError::StaleSource);
    }
    amm_price(quote, base)
}

/// Meteora DLMM: read `active_bin_id` + `bin_step` and compute
///   price = base_price * (1 + bin_step / 10_000)^active_bin_id.
/// Simplified layout for our adapter:
///   0x00: discriminator
///   0x08: active_bin_id  (i32 LE)
///   0x0C: bin_step       (u16 LE)     [bps]
///   0x0E: base_price     (u64 LE)     [lamports per token at bin 0]
fn read_meteora_dlmm(source: &AccountInfo) -> Result<u64, OracleError> {
    let data = source.try_borrow_data().map_err(|_| OracleError::StaleSource)?;
    if data.len() < 0x16 {
        return Err(OracleError::StaleSource);
    }
    let bin_id = i32::from_le_bytes(data[0x08..0x0C].try_into().unwrap());
    let bin_step = u16::from_le_bytes(data[0x0C..0x0E].try_into().unwrap());
    let base_price = u64::from_le_bytes(data[0x0E..0x16].try_into().unwrap());
    if base_price == 0 || bin_step == 0 || bin_step > 10_000 {
        return Err(OracleError::StaleSource);
    }

    // Compound (1 + bin_step/10_000)^bin_id as repeated integer multiplies.
    // For on-chain efficiency we cap |bin_id| at 10_000 — beyond that the
    // price already saturates. Each step multiplies by (10_000 + bin_step)
    // / 10_000 using u128 intermediates.
    let steps = bin_id.unsigned_abs().min(10_000) as u32;
    let mul_num = 10_000u128 + bin_step as u128;
    let mul_den = 10_000u128;
    let mut price: u128 = base_price as u128;
    if bin_id >= 0 {
        for _ in 0..steps {
            price = price
                .checked_mul(mul_num)
                .ok_or(OracleError::Overflow)?
                / mul_den;
            if price == 0 {
                return Ok(0);
            }
        }
    } else {
        for _ in 0..steps {
            price = price
                .checked_mul(mul_den)
                .ok_or(OracleError::Overflow)?
                / mul_num;
            if price == 0 {
                return Ok(0);
            }
        }
    }
    if price > u64::MAX as u128 {
        return Err(OracleError::Overflow);
    }
    Ok(price as u64)
}

/// AMM pricing: `numerator * POS_SCALE / denominator`, saturating at u64.
/// `POS_SCALE = 1e6` matches Percolator's `POS_SCALE` and keeps integer
/// math stable for small-reserve pools.
fn amm_price(numerator: u64, denominator: u64) -> Result<u64, OracleError> {
    const POS_SCALE: u128 = 1_000_000;
    if denominator == 0 {
        return Err(OracleError::StaleSource);
    }
    let scaled = (numerator as u128)
        .checked_mul(POS_SCALE)
        .ok_or(OracleError::Overflow)?;
    let price = scaled / denominator as u128;
    if price > u64::MAX as u128 {
        return Err(OracleError::Overflow);
    }
    Ok(price as u64)
}
