//! Instruction dispatch + `CreateSlab` implementation.
//!
//! Only `CreateSlab` does real work today. Every other branch routes to
//! `PercolatorError::NotImplemented` so clients get a meaningful
//! `ProgramError::Custom` they can match on.

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    msg,
    program::{invoke, invoke_signed},
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::Sysvar,
};

use crate::{
    error::PercolatorError,
    instruction::{
        BootstrapLpArgs, CrankArgs, CrankKind, CreateMarketArgs, CreateSlabArgs, DepositArgs,
        InitializeEngineArgs, LiquidateArgs, PercolatorInstruction, PlaceOrderArgs,
        RiskParamsArgs, WithdrawArgs,
    },
    state::{
        engine_region_size, slab_account_size, SlabHeader, ENGINE_OFFSET, ORIGIN_OPEN,
        VAULT_SEED,
    },
};

/// Minimum paid-listing fee (task #23 v2). 0.5 SOL floor. The frontend can
/// charge more per-tier (e.g. 1.5 SOL after 10 listings); the program only
/// enforces the floor so no one bypasses the UI gating by signing a 0 SOL tx
/// directly. Historical alias kept for client SDKs that still reference the
/// old name — see `MARKET_CREATION_FEE_LAMPORTS` below.
pub const MIN_MARKET_CREATION_FEE_LAMPORTS: u64 = 500_000_000;

/// Backward-compat: the fixed 0.5 SOL listing fee before task #23 v2 went
/// to tiered pricing. Equal to `MIN_MARKET_CREATION_FEE_LAMPORTS`.
pub const MARKET_CREATION_FEE_LAMPORTS: u64 = MIN_MARKET_CREATION_FEE_LAMPORTS;

/// Dev treasury. Every `CreateMarket` must route its fee here. Mainnet will
/// override via a compile-time env or an on-chain Config PDA later.
pub const TREASURY_PUBKEY: Pubkey = solana_program::pubkey!(
    "EM7mXeCaUvj4yJ6zmEtgDfrUUSiK2vuyiwvijNpayktn"
);

/// Slot 0 of every slab is reserved for the protocol LP counterparty that
/// every `PlaceOrder` trades against. A dedicated sentinel owner distinguishes
/// it from user accounts and prevents `process_deposit`'s owner scan from
/// ever matching it against a real pubkey.
pub const PROTOCOL_LP_SLOT: u16 = 0;
pub const PROTOCOL_LP_OWNER_SENTINEL: [u8; 32] = [0xFFu8; 32];

/// Liquidator bounty in basis points of the victim's pre-liquidation capital.
/// 0.5% — meant to cover tx fees plus a small incentive. Capped at whatever
/// capital is left after the engine charges its liquidation fee to insurance.
pub const LIQ_BOUNTY_BPS: u128 = 50;

/// Flat crank bounty, charged off the insurance fund when there was work.
/// 1_000 in mint-native units (e.g., `1_000` of a 6-decimal token = 0.001).
pub const CRANK_BOUNTY: u64 = 1_000;

pub struct Processor;

impl Processor {
    pub fn process(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        instruction_data: &[u8],
    ) -> ProgramResult {
        let ix = PercolatorInstruction::unpack(instruction_data)?;
        match ix {
            PercolatorInstruction::CreateSlab(args) => {
                Self::process_create_slab(program_id, accounts, args)
            }
            PercolatorInstruction::InitializeEngine(args) => {
                Self::process_initialize_engine(program_id, accounts, args)
            }
            PercolatorInstruction::Deposit(args) => {
                Self::process_deposit(program_id, accounts, args)
            }
            PercolatorInstruction::Withdraw(args) => {
                Self::process_withdraw(program_id, accounts, args)
            }
            PercolatorInstruction::BootstrapLp(args) => {
                Self::process_bootstrap_lp(program_id, accounts, args)
            }
            PercolatorInstruction::PlaceOrder(args) => {
                Self::process_place_order(program_id, accounts, args)
            }
            PercolatorInstruction::Liquidate(args) => {
                Self::process_liquidate(program_id, accounts, args)
            }
            PercolatorInstruction::Crank(args) => {
                Self::process_crank(program_id, accounts, args)
            }
            PercolatorInstruction::CreateMarket(args) => {
                Self::process_create_market(program_id, accounts, args)
            }
        }
    }

    fn process_create_slab(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: CreateSlabArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let payer = next_account_info(iter)?;
        let slab_account = next_account_info(iter)?;
        let mint = next_account_info(iter)?;
        let oracle = next_account_info(iter)?;
        let system = next_account_info(iter)?;

        // Basic guards. We deliberately don't validate the mint/oracle beyond
        // existence — this instruction just records them for the wrapper.
        if !payer.is_signer {
            msg!("CreateSlab: payer must sign");
            return Err(PercolatorError::MissingSigner.into());
        }
        if !payer.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if !slab_account.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if *system.key != system_program::ID {
            return Err(PercolatorError::InvalidSystemProgram.into());
        }

        // Expected account shape on entry:
        //   - The client has already issued `system_instruction::create_account`
        //     (or allocate + assign + transfer) in a *prior* instruction of
        //     the same transaction, pre-allocating the full slab size and
        //     handing ownership to this program.
        //   - We enforce that pattern here because the RiskEngine is ~1.52 MiB
        //     at MAX_ACCOUNTS=4096, which blows past the per-CPI
        //     `MAX_PERMITTED_DATA_INCREASE = 10240` ceiling that Solana
        //     imposes on programs that try to CPI create_account themselves.
        //     Client-side pre-allocation has no such cap — the runtime accepts
        //     up to `MAX_PERMITTED_DATA_LENGTH = 10 MiB` per account.
        //
        // This means the on-chain side's job is to *initialize*, not
        // *allocate*. We validate the account matches what we expect and
        // refuse to write over anything populated.

        let expected_size = slab_account_size();
        if slab_account.data_len() != expected_size {
            msg!(
                "CreateSlab: slab data_len {} != expected {}",
                slab_account.data_len(),
                expected_size,
            );
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if slab_account.owner != program_id {
            msg!("CreateSlab: slab must be owned by this program before init");
            return Err(PercolatorError::SlabAlreadyInitialized.into());
        }

        // Rent check: the account must be rent-exempt. If the client
        // under-funded it we'd silently create a reap-risk account, so fail
        // loudly.
        let rent = Rent::get()?;
        if !rent.is_exempt(slab_account.lamports(), expected_size) {
            msg!("CreateSlab: slab account is not rent-exempt");
            return Err(PercolatorError::SlabSizeMismatch.into());
        }

        // Refuse to overwrite an already-initialized slab. We treat a non-zero
        // first word as proof of initialization. This is coarse but sufficient
        // for v0 — the engine initializer will swap to a magic/version tag.
        {
            let data = slab_account.try_borrow_data()?;
            if data[..SlabHeader::LEN].iter().any(|&b| b != 0) {
                msg!("CreateSlab: slab header already populated");
                return Err(PercolatorError::SlabAlreadyInitialized.into());
            }
        }

        // Verify the caller-supplied vault bump maps `[b"vault", slab_key]`
        // to a valid PDA under this program. We don't grind here
        // (`find_program_address` is costly); we just run `create_program_
        // address` with the supplied bump. The vault token account itself
        // is created lazily by the client before the first Deposit.
        Pubkey::create_program_address(
            &[VAULT_SEED, slab_account.key.as_ref(), &[args.vault_bump]],
            program_id,
        )
        .map_err(|_| PercolatorError::VaultPdaMismatch)?;

        // Write the header. The engine region stays zeroed — a future
        // `InitializeEngine` instruction will populate it using the engine's
        // own constructor. We don't try to hand-craft a valid RiskEngine
        // here; the spec-compliant init sequence is non-trivial.
        let header =
            SlabHeader::new(*mint.key, *oracle.key, *payer.key, args.bump, args.vault_bump);
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            header.write_into(&mut data[..SlabHeader::LEN])?;
            // Defensive: client-allocated memory may or may not be zeroed
            // depending on how the tx was constructed. Explicitly zero the
            // engine region.
            for b in &mut data[ENGINE_OFFSET..] {
                *b = 0;
            }
        }

        msg!(
            "CreateSlab: mint={}, slab={}, size={}",
            mint.key,
            slab_account.key,
            expected_size,
        );
        Ok(())
    }

    fn process_initialize_engine(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: InitializeEngineArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let slab_account = next_account_info(iter)?;
        let creator = next_account_info(iter)?;

        if !slab_account.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if slab_account.owner != program_id {
            msg!("InitializeEngine: slab not owned by program");
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        let expected_size = slab_account_size();
        if slab_account.data_len() != expected_size {
            msg!(
                "InitializeEngine: slab data_len {} != expected {}",
                slab_account.data_len(),
                expected_size,
            );
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if !creator.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }

        // Read and validate the header. We need to know:
        //   - CreateSlab ran (header is non-zero), else this is noise.
        //   - The signer is the recorded creator.
        //   - We haven't already initialized (idempotency).
        let mut header = SlabHeader::read_from(&slab_account.try_borrow_data()?[..SlabHeader::LEN])?;
        if header.mint == Pubkey::default()
            && header.oracle == Pubkey::default()
            && header.creator == Pubkey::default()
        {
            msg!("InitializeEngine: slab header is empty, run CreateSlab first");
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if header.creator != *creator.key {
            msg!("InitializeEngine: signer is not the slab creator on record");
            return Err(PercolatorError::WrongSigner.into());
        }
        if header.is_initialized() {
            msg!("InitializeEngine: engine region already initialized");
            return Err(PercolatorError::AlreadyInitialized.into());
        }

        // Pre-flight risk-param validation. The parent crate's validator
        // `panic!`s on bad input, which would abort the transaction as an
        // opaque runtime error. We mirror its key invariants here and return a
        // clean `InvalidRiskParams` instead.
        validate_risk_params(&args.risk_params, args.init_oracle_price)?;

        let engine_params = args.risk_params.into_engine_params();
        let clock = Clock::get()?;
        let init_slot = clock.slot;
        let init_oracle_price = args.init_oracle_price;

        // Cast the engine region to `&mut percolator::RiskEngine`.
        //
        // SAFETY:
        //   - `RiskEngine` is `#[repr(C)]` with a byte-stable layout.
        //   - Solana guarantees 8-byte alignment for account data, and the
        //     engine region starts at `ENGINE_OFFSET = 104` which keeps the
        //     8-byte alignment.
        //   - We borrow the whole engine region exclusively via
        //     `try_borrow_mut_data`, so no other reference aliases these bytes
        //     for the duration of the call.
        //   - CreateSlab zero-fills the engine region, so every byte inside
        //     the struct is a valid bit-pattern for every field (u8/u64/i128/
        //     `[u64; 2]` newtypes all accept zero). `init_in_place` then
        //     canonicalises every field, so even non-zero starting memory
        //     (e.g. resurrected account) would be safe.
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            let engine_bytes = &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine_ptr = engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine;
            let engine: &mut percolator::RiskEngine = unsafe { &mut *engine_ptr };
            engine.init_in_place(engine_params, init_slot, init_oracle_price);
        }

        // Flip the initialized flag in the header. Rewrite the full header
        // since `write_into` expects a full buffer — cheap at 104 bytes.
        header.initialized = 1;
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            header.write_into(&mut data[..SlabHeader::LEN])?;
        }

        msg!(
            "InitializeEngine: slab={}, mark_price={}, funding_cap={}, slot={}",
            slab_account.key,
            init_oracle_price,
            args.risk_params.max_accrual_dt_slots,
            init_slot,
        );
        Ok(())
    }

    fn process_deposit(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: DepositArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let slab_account = next_account_info(iter)?;
        let user = next_account_info(iter)?;
        let user_ta = next_account_info(iter)?;
        let vault_ta = next_account_info(iter)?;
        let mint = next_account_info(iter)?;
        let token_program = next_account_info(iter)?;
        let _system = next_account_info(iter)?;

        if args.amount == 0 {
            return Err(PercolatorError::ZeroAmount.into());
        }
        if !user.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }
        if !slab_account.is_writable
            || !user_ta.is_writable
            || !vault_ta.is_writable
        {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if slab_account.owner != program_id {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if slab_account.data_len() != slab_account_size() {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if *token_program.key != spl_token::ID {
            return Err(PercolatorError::InvalidTokenProgram.into());
        }

        let header =
            SlabHeader::read_from(&slab_account.try_borrow_data()?[..SlabHeader::LEN])?;
        if !header.is_initialized() {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if header.mint != *mint.key {
            return Err(PercolatorError::WrongMint.into());
        }

        // Re-derive vault PDA from the stored bump. `create_program_address`
        // is the cheap path; the bump seed lives in the header.
        let expected_vault = Pubkey::create_program_address(
            &[VAULT_SEED, slab_account.key.as_ref(), &[header.vault_bump]],
            program_id,
        )
        .map_err(|_| PercolatorError::VaultPdaMismatch)?;
        if expected_vault != *vault_ta.key {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        // SPL token account validation. Parse both the user's source ATA
        // and the vault ATA: mint must match the slab's mint; user ATA's
        // authority must be the signer.
        if *user_ta.owner != spl_token::ID || *vault_ta.owner != spl_token::ID {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        let user_token = spl_token::state::Account::unpack(&user_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        let vault_token = spl_token::state::Account::unpack(&vault_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        if user_token.mint != header.mint {
            return Err(PercolatorError::TokenAccountWrongMint.into());
        }
        if vault_token.mint != header.mint {
            return Err(PercolatorError::TokenAccountWrongMint.into());
        }
        if user_token.owner != *user.key {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }

        let clock = Clock::get()?;

        // Slot selection: scan the engine's account table for an existing
        // entry whose `owner == user.key`. If found, reuse. Otherwise claim
        // the freelist head. Bitmap capacity is enforced inside the engine's
        // `materialize_at` (called by `deposit_not_atomic`).
        let (chosen_idx, is_fresh_account) = {
            let data = slab_account.try_borrow_data()?;
            let engine_bytes = &data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine_ptr = engine_bytes.as_ptr() as *const percolator::RiskEngine;
            let engine: &percolator::RiskEngine = unsafe { &*engine_ptr };

            let mut found: Option<u16> = None;
            let max = (engine.params.max_accounts as usize).min(percolator::MAX_ACCOUNTS);
            for i in 0..max {
                if engine.is_used(i) && engine.accounts[i].owner == user.key.to_bytes() {
                    found = Some(i as u16);
                    break;
                }
            }
            match found {
                Some(i) => (i, false),
                None => {
                    let head = engine.free_head;
                    if head == u16::MAX {
                        return Err(PercolatorError::SlabFull.into());
                    }
                    (head, true)
                }
            }
        };

        // Engine call: capital += amount, C_tot += amount, V += amount. The
        // oracle price is ignored (the engine marks it `_` in
        // `deposit_not_atomic`); we pass the stored `last_oracle_price` so
        // we don't need the oracle account here.
        let amount_u128 = args.amount as u128;
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            let engine_bytes =
                &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine_ptr = engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine;
            let engine: &mut percolator::RiskEngine = unsafe { &mut *engine_ptr };
            let px = engine.last_oracle_price;
            engine
                .deposit_not_atomic(chosen_idx, amount_u128, px, clock.slot)
                .map_err(|_| PercolatorError::EngineError)?;
            if is_fresh_account {
                // `set_owner` refuses to overwrite a non-zero owner; at this
                // point materialize_at has just zeroed it, so this is safe.
                engine
                    .set_owner(chosen_idx, user.key.to_bytes())
                    .map_err(|_| PercolatorError::EngineError)?;
            }
        }

        // CPI: SPL token transfer user -> vault for amount. Mutating state
        // before the CPI is safe here because spl-token does not re-enter
        // us, and the engine invariants now match what the vault balance
        // will be once the transfer lands.
        let ix = spl_token::instruction::transfer(
            token_program.key,
            user_ta.key,
            vault_ta.key,
            user.key,
            &[],
            args.amount,
        )?;
        invoke(
            &ix,
            &[
                user_ta.clone(),
                vault_ta.clone(),
                user.clone(),
                token_program.clone(),
            ],
        )?;

        msg!(
            "Deposit: slab={}, user={}, idx={}, amount={}, fresh={}",
            slab_account.key,
            user.key,
            chosen_idx,
            args.amount,
            is_fresh_account,
        );
        Ok(())
    }

    fn process_withdraw(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: WithdrawArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let slab_account = next_account_info(iter)?;
        let user = next_account_info(iter)?;
        let user_ta = next_account_info(iter)?;
        let vault_ta = next_account_info(iter)?;
        let mint = next_account_info(iter)?;
        let token_program = next_account_info(iter)?;
        let _system = next_account_info(iter)?;

        if args.amount == 0 {
            return Err(PercolatorError::ZeroAmount.into());
        }
        if !user.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }
        if !slab_account.is_writable
            || !user_ta.is_writable
            || !vault_ta.is_writable
        {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if slab_account.owner != program_id {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if slab_account.data_len() != slab_account_size() {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if *token_program.key != spl_token::ID {
            return Err(PercolatorError::InvalidTokenProgram.into());
        }

        let header =
            SlabHeader::read_from(&slab_account.try_borrow_data()?[..SlabHeader::LEN])?;
        if !header.is_initialized() {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if header.mint != *mint.key {
            return Err(PercolatorError::WrongMint.into());
        }

        // Vault PDA: re-derive from stored bump and verify identity.
        let expected_vault = Pubkey::create_program_address(
            &[VAULT_SEED, slab_account.key.as_ref(), &[header.vault_bump]],
            program_id,
        )
        .map_err(|_| PercolatorError::VaultPdaMismatch)?;
        if expected_vault != *vault_ta.key {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        // Token-account validation.
        if *user_ta.owner != spl_token::ID || *vault_ta.owner != spl_token::ID {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        let user_token = spl_token::state::Account::unpack(&user_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        let vault_token = spl_token::state::Account::unpack(&vault_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        if user_token.mint != header.mint || vault_token.mint != header.mint {
            return Err(PercolatorError::TokenAccountWrongMint.into());
        }
        if user_token.owner != *user.key {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        // Vault token account must be self-owned by the vault PDA (the
        // convention this crate follows: vault_ta address == vault PDA ==
        // vault_ta.authority).
        if vault_token.owner != expected_vault {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        // Locate the user's existing engine slot. Withdraw is meaningless
        // for a non-existent account, so refuse rather than pick a free
        // slot.
        let chosen_idx: u16 = {
            let data = slab_account.try_borrow_data()?;
            let engine_bytes =
                &data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine_ptr = engine_bytes.as_ptr() as *const percolator::RiskEngine;
            let engine: &percolator::RiskEngine = unsafe { &*engine_ptr };

            let max = (engine.params.max_accounts as usize).min(percolator::MAX_ACCOUNTS);
            let mut found: Option<u16> = None;
            for i in 0..max {
                if engine.is_used(i) && engine.accounts[i].owner == user.key.to_bytes() {
                    found = Some(i as u16);
                    break;
                }
            }
            found.ok_or(PercolatorError::EngineError)?
        };

        let clock = Clock::get()?;

        // Engine call: the engine performs accrual + lazy settlement +
        // matured-PnL conversion (when h == 1) + the `amount <= capital`
        // check internally. Our wrapper just hands it the current oracle
        // price (stored on the engine) and the configured admission window.
        //
        // h computation performed inside the engine, at
        // `finalize_touched_accounts_post_live`:
        //
        //     residual = max(vault - (c_tot + insurance), 0)
        //     h_num   = min(residual, pnl_matured_pos_tot)
        //     h_den   = pnl_matured_pos_tot
        //     h       = h_num / h_den         (or 0 when h_den == 0)
        //     is_whole = (h_den > 0 && h_num == h_den)
        //
        // When `is_whole` is true, matured positive PnL on a flat account
        // is released into capital before the `amount <= capital` check.
        // When h < 1, no conversion happens and only pre-existing capital
        // is withdrawable this instruction.
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            let engine_bytes =
                &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine_ptr = engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine;
            let engine: &mut percolator::RiskEngine = unsafe { &mut *engine_ptr };
            let px = engine.last_oracle_price;
            let h_min = engine.params.h_min;
            let h_max = engine.params.h_max;
            engine
                .withdraw_not_atomic(
                    chosen_idx,
                    args.amount as u128,
                    px,
                    clock.slot,
                    0i128, // funding_rate_e9 — task #15 wires the oracle
                    h_min,
                    h_max,
                )
                .map_err(|_| PercolatorError::EngineError)?;
        }

        // CPI: vault_ta -> user_ta, signed by the vault PDA via the stored
        // bump. invoke_signed verifies the provided seeds derive to the
        // authority pubkey referenced in the transfer instruction.
        let ix = spl_token::instruction::transfer(
            token_program.key,
            vault_ta.key,
            user_ta.key,
            vault_ta.key, // authority == vault_ta key (self-owned PDA)
            &[],
            args.amount,
        )?;
        let slab_key = slab_account.key.to_bytes();
        let vault_bump = [header.vault_bump];
        let seeds: &[&[u8]] = &[VAULT_SEED, slab_key.as_ref(), &vault_bump];
        invoke_signed(
            &ix,
            &[
                vault_ta.clone(),
                user_ta.clone(),
                vault_ta.clone(),
                token_program.clone(),
            ],
            &[seeds],
        )?;

        msg!(
            "Withdraw: slab={}, user={}, idx={}, amount={}",
            slab_account.key,
            user.key,
            chosen_idx,
            args.amount,
        );
        Ok(())
    }

    fn process_bootstrap_lp(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: BootstrapLpArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let slab_account = next_account_info(iter)?;
        let creator = next_account_info(iter)?;
        let creator_ta = next_account_info(iter)?;
        let vault_ta = next_account_info(iter)?;
        let mint = next_account_info(iter)?;
        let token_program = next_account_info(iter)?;
        let _system = next_account_info(iter)?;

        if args.amount == 0 {
            return Err(PercolatorError::ZeroAmount.into());
        }
        if !creator.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }
        if !slab_account.is_writable || !creator_ta.is_writable || !vault_ta.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if slab_account.owner != program_id {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if slab_account.data_len() != slab_account_size() {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if *token_program.key != spl_token::ID {
            return Err(PercolatorError::InvalidTokenProgram.into());
        }

        let header =
            SlabHeader::read_from(&slab_account.try_borrow_data()?[..SlabHeader::LEN])?;
        if !header.is_initialized() {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if header.creator != *creator.key {
            return Err(PercolatorError::WrongSigner.into());
        }
        if header.mint != *mint.key {
            return Err(PercolatorError::WrongMint.into());
        }

        let expected_vault = Pubkey::create_program_address(
            &[VAULT_SEED, slab_account.key.as_ref(), &[header.vault_bump]],
            program_id,
        )
        .map_err(|_| PercolatorError::VaultPdaMismatch)?;
        if expected_vault != *vault_ta.key {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        if *creator_ta.owner != spl_token::ID || *vault_ta.owner != spl_token::ID {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        let creator_token =
            spl_token::state::Account::unpack(&creator_ta.try_borrow_data()?)
                .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        let vault_token = spl_token::state::Account::unpack(&vault_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        if creator_token.mint != header.mint || vault_token.mint != header.mint {
            return Err(PercolatorError::TokenAccountWrongMint.into());
        }
        if creator_token.owner != *creator.key {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }

        let clock = Clock::get()?;

        // Reject replay: slot 0 already materialized (probably as LP, but we
        // refuse regardless to keep this a one-shot).
        {
            let data = slab_account.try_borrow_data()?;
            let engine: &percolator::RiskEngine = unsafe {
                &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
            };
            if engine.is_used(PROTOCOL_LP_SLOT as usize) {
                return Err(PercolatorError::LpAlreadyBootstrapped.into());
            }
            // SECURITY: BootstrapLp requires free_head == 0. InitializeEngine
            // leaves the freelist head at 0, so BootstrapLp MUST run before
            // any user Deposit — otherwise a user's Deposit would claim
            // slot 0, the LP could never be seeded, and the slab's
            // PlaceOrder path becomes permanently unusable. Task #21 (seed
            // script) composes BootstrapLp + Deposit in the right order.
            if engine.free_head != PROTOCOL_LP_SLOT {
                return Err(PercolatorError::LpAlreadyBootstrapped.into());
            }
        }

        // Deposit into slot 0 to materialize it and seed capital.
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            let engine_bytes =
                &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine: &mut percolator::RiskEngine =
                unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
            let px = engine.last_oracle_price;
            engine
                .deposit_not_atomic(
                    PROTOCOL_LP_SLOT,
                    args.amount as u128,
                    px,
                    clock.slot,
                )
                .map_err(|_| PercolatorError::EngineError)?;
            // Mark slot 0 as an LP account with a sentinel owner that can't
            // collide with any ed25519-valid pubkey. The sentinel blocks
            // the Deposit owner-scan from accidentally reusing slot 0 for
            // a user.
            engine.accounts[PROTOCOL_LP_SLOT as usize].kind = percolator::Account::KIND_LP;
            engine.accounts[PROTOCOL_LP_SLOT as usize].owner = PROTOCOL_LP_OWNER_SENTINEL;
        }

        // CPI: creator -> vault.
        let ix = spl_token::instruction::transfer(
            token_program.key,
            creator_ta.key,
            vault_ta.key,
            creator.key,
            &[],
            args.amount,
        )?;
        invoke(
            &ix,
            &[
                creator_ta.clone(),
                vault_ta.clone(),
                creator.clone(),
                token_program.clone(),
            ],
        )?;

        msg!(
            "BootstrapLp: slab={}, amount={}, creator={}",
            slab_account.key,
            args.amount,
            creator.key,
        );
        Ok(())
    }

    fn process_place_order(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: PlaceOrderArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let slab_account = next_account_info(iter)?;
        let user = next_account_info(iter)?;
        let oracle_account = next_account_info(iter)?;
        let _clock_sysvar = next_account_info(iter)?; // PlaceOrder reads via Clock::get()

        if args.size == 0 {
            return Err(PercolatorError::ZeroSize.into());
        }
        if args.side > 1 {
            return Err(PercolatorError::InvalidSide.into());
        }
        if args.max_price < args.min_price {
            return Err(PercolatorError::SlippageExceeded.into());
        }
        if !user.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }
        if !slab_account.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if slab_account.owner != program_id {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if slab_account.data_len() != slab_account_size() {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }

        let header =
            SlabHeader::read_from(&slab_account.try_borrow_data()?[..SlabHeader::LEN])?;
        if !header.is_initialized() {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if *oracle_account.key != header.oracle {
            return Err(PercolatorError::StaleOracle.into());
        }

        // Read u64 mark price from the first 8 bytes of the oracle account.
        // Task #15 replaces this with a typed oracle-feed deserializer.
        let oracle_data = oracle_account.try_borrow_data()?;
        if oracle_data.len() < 8 {
            return Err(PercolatorError::StaleOracle.into());
        }
        let oracle_price = u64::from_le_bytes(oracle_data[..8].try_into().unwrap());
        drop(oracle_data);
        if oracle_price == 0 || oracle_price > percolator::MAX_ORACLE_PRICE {
            return Err(PercolatorError::StaleOracle.into());
        }
        if oracle_price < args.min_price || oracle_price > args.max_price {
            return Err(PercolatorError::SlippageExceeded.into());
        }

        let clock = Clock::get()?;

        // Locate the user's existing slot. PlaceOrder requires an existing
        // account — collateral must already be deposited.
        let user_idx: u16 = {
            let data = slab_account.try_borrow_data()?;
            let engine: &percolator::RiskEngine = unsafe {
                &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
            };
            let max = (engine.params.max_accounts as usize).min(percolator::MAX_ACCOUNTS);
            let mut found: Option<u16> = None;
            for i in 0..max {
                if engine.is_used(i) && engine.accounts[i].owner == user.key.to_bytes() {
                    found = Some(i as u16);
                    break;
                }
            }
            found.ok_or(PercolatorError::EngineError)?
        };

        // Pre-check: LP counterparty materialized and still the sentinel
        // owner. If slot 0 was taken by a user (because BootstrapLp never
        // ran before user deposits), we can't trade against it.
        {
            let data = slab_account.try_borrow_data()?;
            let engine: &percolator::RiskEngine = unsafe {
                &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
            };
            let lp = PROTOCOL_LP_SLOT as usize;
            if !engine.is_used(lp)
                || engine.accounts[lp].owner != PROTOCOL_LP_OWNER_SENTINEL
            {
                return Err(PercolatorError::LpNotBootstrapped.into());
            }

            // SECURITY: pre-check side gating for pure opens. The engine
            // also enforces DrainOnly / ResetPending internally, but
            // returns a generic Unauthorized. We surface the specific
            // DrainOnly error so frontends can distinguish "side gated"
            // from "bad signer" without log scraping. MUST match the
            // engine's internal OI-increase gate — if the two drift,
            // a user could route an order through the wrapper that the
            // engine then rejects with a worse error code.
            let cur_user_pos = engine.effective_pos_q(user_idx as usize);
            let user_side = if args.side == 0 {
                percolator::Side::Long
            } else {
                percolator::Side::Short
            };
            // Does this order open / grow OI on `user_side`?
            //   - If user currently has no position, any order opens on user_side.
            //   - If user is already on user_side, growing size keeps opening.
            //   - If user is on the opposite side, the order reduces or flips.
            //     A flip also opens OI on user_side beyond the flip point.
            let opens_on_user_side = match (cur_user_pos.signum(), user_side) {
                (0, _) => true,
                (1, percolator::Side::Long) => true,
                (-1, percolator::Side::Short) => true,
                // Sign opposite: reduce/close/flip. A flip happens iff
                // args.size * POS_SCALE_mag > |cur_pos|. POS_SCALE is 1e6
                // and `args.size` is already in POS_SCALE units, so direct
                // compare.
                _ => (args.size as i128) > cur_user_pos.unsigned_abs() as i128,
            };
            if opens_on_user_side {
                let mode = match user_side {
                    percolator::Side::Long => engine.side_mode_long,
                    percolator::Side::Short => engine.side_mode_short,
                };
                if mode != percolator::SideMode::Normal {
                    return Err(PercolatorError::DrainOnly.into());
                }
            }
        }

        // Engine call. The wrapper maps side → (a, b) ordering:
        //   Long  : (user, LP), size_q = +size   → user gets +size, LP gets -size
        //   Short : (LP, user), size_q = +size   → LP gets +size, user gets -size
        let size_q = args.size as i128;
        let (a, b) = if args.side == 0 {
            (user_idx, PROTOCOL_LP_SLOT)
        } else {
            (PROTOCOL_LP_SLOT, user_idx)
        };
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            let engine_bytes =
                &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine: &mut percolator::RiskEngine =
                unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
            let h_min = engine.params.h_min;
            let h_max = engine.params.h_max;
            engine
                .execute_trade_not_atomic(
                    a,
                    b,
                    oracle_price,
                    clock.slot,
                    size_q,
                    oracle_price,
                    0i128, // funding_rate_e9, wired to oracle adapter in #15
                    h_min,
                    h_max,
                )
                .map_err(|_| PercolatorError::EngineError)?;
        }

        msg!(
            "PlaceOrder: slab={}, user={}, idx={}, side={}, size={}, px={}",
            slab_account.key,
            user.key,
            user_idx,
            args.side,
            args.size,
            oracle_price,
        );
        Ok(())
    }

    fn process_liquidate(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: LiquidateArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let slab_account = next_account_info(iter)?;
        let liquidator = next_account_info(iter)?;
        let liquidator_ta = next_account_info(iter)?;
        let oracle_account = next_account_info(iter)?;
        let _clock_sysvar = next_account_info(iter)?;
        let vault_ta = next_account_info(iter)?;
        let token_program = next_account_info(iter)?;

        if !liquidator.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }
        if !slab_account.is_writable || !liquidator_ta.is_writable || !vault_ta.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if slab_account.owner != program_id {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if slab_account.data_len() != slab_account_size() {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if *token_program.key != spl_token::ID {
            return Err(PercolatorError::InvalidTokenProgram.into());
        }
        if args.victim_slot == PROTOCOL_LP_SLOT {
            return Err(PercolatorError::ProtocolLpNotLiquidatable.into());
        }
        if (args.victim_slot as usize) >= percolator::MAX_ACCOUNTS {
            return Err(PercolatorError::InvalidVictimSlot.into());
        }

        let header =
            SlabHeader::read_from(&slab_account.try_borrow_data()?[..SlabHeader::LEN])?;
        if !header.is_initialized() {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if *oracle_account.key != header.oracle {
            return Err(PercolatorError::StaleOracle.into());
        }
        let expected_vault = Pubkey::create_program_address(
            &[VAULT_SEED, slab_account.key.as_ref(), &[header.vault_bump]],
            program_id,
        )
        .map_err(|_| PercolatorError::VaultPdaMismatch)?;
        if expected_vault != *vault_ta.key {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        // Token-account validation: both ATAs must carry the slab's mint;
        // liquidator must own their receiving ATA; vault must be self-owned
        // by the vault PDA.
        if *liquidator_ta.owner != spl_token::ID || *vault_ta.owner != spl_token::ID {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        let liq_token = spl_token::state::Account::unpack(&liquidator_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        let vault_token = spl_token::state::Account::unpack(&vault_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        if liq_token.mint != header.mint || vault_token.mint != header.mint {
            return Err(PercolatorError::TokenAccountWrongMint.into());
        }
        if liq_token.owner != *liquidator.key {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        if vault_token.owner != expected_vault {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        // Oracle price read (see process_place_order for the temporary
        // bytes-0..8 protocol).
        let oracle_data = oracle_account.try_borrow_data()?;
        if oracle_data.len() < 8 {
            return Err(PercolatorError::StaleOracle.into());
        }
        let oracle_price = u64::from_le_bytes(oracle_data[..8].try_into().unwrap());
        drop(oracle_data);
        if oracle_price == 0 || oracle_price > percolator::MAX_ORACLE_PRICE {
            return Err(PercolatorError::StaleOracle.into());
        }

        let clock = Clock::get()?;

        // Snapshot victim's pre-liquidation capital for bounty sizing.
        let orig_capital: u128 = {
            let data = slab_account.try_borrow_data()?;
            let engine: &percolator::RiskEngine = unsafe {
                &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
            };
            if !engine.is_used(args.victim_slot as usize) {
                return Err(PercolatorError::InvalidVictimSlot.into());
            }
            engine.accounts[args.victim_slot as usize].capital.get()
        };

        // Engine call. Flow per spec §9.4 / §9.5:
        //   1. accrue_market_to
        //   2. touch_account_live_local (lazy A/K settle)
        //   3. liquidate_at_oracle_internal:
        //        - close position at oracle
        //        - pay liq_fee into insurance
        //        - enqueue ADL (deficit D into K_opposite socialization)
        //        - on k-overflow: fallback to h-haircut via finalize
        //   4. finalize_touched_accounts_post_live
        //   5. schedule/finalize end-of-instruction side resets (drives
        //      side_mode transitions when A hits floor)
        //
        // Returns Ok(false) if the account is above maintenance margin.
        let did_liquidate = {
            let mut data = slab_account.try_borrow_mut_data()?;
            let engine_bytes =
                &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine: &mut percolator::RiskEngine =
                unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
            let h_min = engine.params.h_min;
            let h_max = engine.params.h_max;
            engine
                .liquidate_at_oracle_not_atomic(
                    args.victim_slot,
                    clock.slot,
                    oracle_price,
                    percolator::LiquidationPolicy::FullClose,
                    0i128,
                    h_min,
                    h_max,
                )
                .map_err(|_| PercolatorError::EngineError)?
        };
        if !did_liquidate {
            return Err(PercolatorError::AccountHealthy.into());
        }

        // Bounty: LIQ_BOUNTY_BPS * orig_capital, capped at remaining capital
        // (don't double-count what the engine already ripped out for the
        // insurance fee; the cap guarantees we never drive capital below 0).
        let bounty: u128 = {
            let mut data = slab_account.try_borrow_mut_data()?;
            let engine_bytes =
                &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
            let engine: &mut percolator::RiskEngine =
                unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
            let remaining = engine.accounts[args.victim_slot as usize].capital.get();
            let target = orig_capital
                .saturating_mul(LIQ_BOUNTY_BPS)
                / 10_000u128;
            let b = core::cmp::min(target, remaining);
            if b > 0 {
                // SECURITY: three-way decrement preserves conservation
                //   (V >= C_tot + I). All three fields MUST move by the
                // same amount, else the engine's `check_conservation`
                // postcondition will fire on the next public instruction.
                // `set_capital` is engine-private so we write the `pub`
                // fields directly; checked_sub on c_tot/vault guards
                // against corruption where capital exceeds c_tot.
                let new_cap = remaining - b;
                engine.accounts[args.victim_slot as usize].capital =
                    percolator::U128::new(new_cap);
                let new_c_tot = engine
                    .c_tot
                    .get()
                    .checked_sub(b)
                    .ok_or(PercolatorError::EngineError)?;
                engine.c_tot = percolator::U128::new(new_c_tot);
                let new_v = engine
                    .vault
                    .get()
                    .checked_sub(b)
                    .ok_or(PercolatorError::EngineError)?;
                engine.vault = percolator::U128::new(new_v);
            }
            b
        };

        if bounty > 0 {
            let ix = spl_token::instruction::transfer(
                token_program.key,
                vault_ta.key,
                liquidator_ta.key,
                vault_ta.key, // authority = vault PDA (self-owned)
                &[],
                bounty as u64,
            )?;
            let slab_key = slab_account.key.to_bytes();
            let vault_bump = [header.vault_bump];
            let seeds: &[&[u8]] = &[VAULT_SEED, slab_key.as_ref(), &vault_bump];
            invoke_signed(
                &ix,
                &[
                    vault_ta.clone(),
                    liquidator_ta.clone(),
                    vault_ta.clone(),
                    token_program.clone(),
                ],
                &[seeds],
            )?;
        }

        msg!(
            "Liquidate: slab={}, victim={}, liquidator={}, orig_capital={}, bounty={}",
            slab_account.key,
            args.victim_slot,
            liquidator.key,
            orig_capital,
            bounty,
        );
        Ok(())
    }

    fn process_crank(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: CrankArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let slab_account = next_account_info(iter)?;
        let caller = next_account_info(iter)?;
        let _clock_sysvar = next_account_info(iter)?;
        let caller_ta = next_account_info(iter)?;
        let vault_ta = next_account_info(iter)?;
        let token_program = next_account_info(iter)?;

        if !caller.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }
        if !slab_account.is_writable || !caller_ta.is_writable || !vault_ta.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if slab_account.owner != program_id {
            return Err(PercolatorError::SlabNotInitialized.into());
        }
        if slab_account.data_len() != slab_account_size() {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if *token_program.key != spl_token::ID {
            return Err(PercolatorError::InvalidTokenProgram.into());
        }

        let kind = match args.kind {
            0 => CrankKind::Funding,
            1 => CrankKind::Gc,
            2 => CrankKind::AdlReset,
            _ => return Err(PercolatorError::InvalidCrankKind.into()),
        };

        let header =
            SlabHeader::read_from(&slab_account.try_borrow_data()?[..SlabHeader::LEN])?;
        if !header.is_initialized() {
            return Err(PercolatorError::SlabNotInitialized.into());
        }

        let expected_vault = Pubkey::create_program_address(
            &[VAULT_SEED, slab_account.key.as_ref(), &[header.vault_bump]],
            program_id,
        )
        .map_err(|_| PercolatorError::VaultPdaMismatch)?;
        if expected_vault != *vault_ta.key {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        if *caller_ta.owner != spl_token::ID || *vault_ta.owner != spl_token::ID {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        let caller_token = spl_token::state::Account::unpack(&caller_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        let vault_token = spl_token::state::Account::unpack(&vault_ta.try_borrow_data()?)
            .map_err(|_| PercolatorError::TokenAccountWrongOwner)?;
        if caller_token.mint != header.mint || vault_token.mint != header.mint {
            return Err(PercolatorError::TokenAccountWrongMint.into());
        }
        if caller_token.owner != *caller.key {
            return Err(PercolatorError::TokenAccountWrongOwner.into());
        }
        if vault_token.owner != expected_vault {
            return Err(PercolatorError::VaultPdaMismatch.into());
        }

        let clock = Clock::get()?;
        let mut did_work = false;
        let mut reclaimed: u32 = 0;

        match kind {
            CrankKind::Funding => {
                let mut data = slab_account.try_borrow_mut_data()?;
                let engine_bytes =
                    &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
                let engine: &mut percolator::RiskEngine =
                    unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
                // Accrue only if time has advanced; otherwise the call is a
                // zero-delta no-op and we still want to report NothingToDo.
                if clock.slot <= engine.last_market_slot {
                    return Err(PercolatorError::NothingToDo.into());
                }
                let px = engine.last_oracle_price;
                engine
                    .accrue_market_to(clock.slot, px, 0i128)
                    .map_err(|_| PercolatorError::EngineError)?;
                engine.current_slot = clock.slot;
                engine.last_crank_slot = clock.slot;
                did_work = true;
            }
            CrankKind::Gc => {
                // reclaim_empty_account_not_atomic can't be called inside a
                // borrow of slab data because it's a `&mut self` method on
                // the engine, and we can only hold one `try_borrow_mut_data`
                // at a time. Loop: pick a candidate (release borrow) → call
                // on engine (fresh borrow) → advance cursor.
                //
                // We scan at most GC_CLOSE_BUDGET slots per crank; skip
                // slot 0 (protocol LP) explicitly. Non-dust slots return
                // engine errors that we swallow.
                let (mut cursor, max_accounts) = {
                    let data = slab_account.try_borrow_data()?;
                    let engine: &percolator::RiskEngine = unsafe {
                        &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
                    };
                    (engine.gc_cursor as usize, engine.params.max_accounts as usize)
                };
                let budget = percolator::GC_CLOSE_BUDGET as usize;
                for _ in 0..budget.min(max_accounts) {
                    cursor = (cursor + 1) % max_accounts.max(1);
                    if cursor == PROTOCOL_LP_SLOT as usize {
                        continue;
                    }
                    // Read: is this slot used + flat-clean?
                    let eligible = {
                        let data = slab_account.try_borrow_data()?;
                        let engine: &percolator::RiskEngine = unsafe {
                            &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
                        };
                        if !engine.is_used(cursor) {
                            false
                        } else {
                            let a = &engine.accounts[cursor];
                            a.position_basis_q == 0
                                && a.pnl == 0
                                && a.reserved_pnl == 0
                                && a.sched_present == 0
                                && a.pending_present == 0
                                && a.capital.get() < engine.params.min_initial_deposit.get()
                        }
                    };
                    if !eligible {
                        continue;
                    }
                    let mut data = slab_account.try_borrow_mut_data()?;
                    let engine_bytes =
                        &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
                    let engine: &mut percolator::RiskEngine = unsafe {
                        &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine)
                    };
                    if engine
                        .reclaim_empty_account_not_atomic(cursor as u16, clock.slot)
                        .is_ok()
                    {
                        reclaimed += 1;
                    }
                }
                // Persist cursor for next crank call.
                {
                    let mut data = slab_account.try_borrow_mut_data()?;
                    let engine_bytes =
                        &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
                    let engine: &mut percolator::RiskEngine = unsafe {
                        &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine)
                    };
                    engine.gc_cursor = cursor as u16;
                }
                if reclaimed > 0 {
                    did_work = true;
                } else {
                    return Err(PercolatorError::NothingToDo.into());
                }
            }
            CrankKind::AdlReset => {
                // Advance the side-mode state machine by running a settle
                // on the protocol LP slot. `settle_account_not_atomic` runs
                // the full touch + finalize + schedule/finalize-reset
                // sequence, which transitions DrainOnly → ResetPending →
                // Normal when conditions are met.
                let (side_l_before, side_s_before) = {
                    let data = slab_account.try_borrow_data()?;
                    let engine: &percolator::RiskEngine = unsafe {
                        &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
                    };
                    (engine.side_mode_long, engine.side_mode_short)
                };
                {
                    let mut data = slab_account.try_borrow_mut_data()?;
                    let engine_bytes =
                        &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
                    let engine: &mut percolator::RiskEngine = unsafe {
                        &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine)
                    };
                    // AdlReset requires some used account to settle on.
                    // We use the LP (slot 0) if bootstrapped; else any
                    // used slot; else NothingToDo.
                    let mut anchor: Option<u16> = None;
                    if engine.is_used(PROTOCOL_LP_SLOT as usize) {
                        anchor = Some(PROTOCOL_LP_SLOT);
                    } else {
                        let max = engine.params.max_accounts as usize;
                        for i in 0..max {
                            if engine.is_used(i) {
                                anchor = Some(i as u16);
                                break;
                            }
                        }
                    }
                    let anchor = anchor.ok_or(PercolatorError::NothingToDo)?;
                    let px = engine.last_oracle_price;
                    let h_min = engine.params.h_min;
                    let h_max = engine.params.h_max;
                    engine
                        .settle_account_not_atomic(anchor, px, clock.slot, 0i128, h_min, h_max)
                        .map_err(|_| PercolatorError::EngineError)?;
                }
                let (side_l_after, side_s_after) = {
                    let data = slab_account.try_borrow_data()?;
                    let engine: &percolator::RiskEngine = unsafe {
                        &*(data[ENGINE_OFFSET..].as_ptr() as *const percolator::RiskEngine)
                    };
                    (engine.side_mode_long, engine.side_mode_short)
                };
                if side_l_before != side_l_after || side_s_before != side_s_after {
                    did_work = true;
                } else {
                    // The settle still advanced current_slot / did housekeeping
                    // but no side-mode transition fired. Treat as NothingToDo
                    // so keeper loops don't rack up bounties on no-op calls.
                    return Err(PercolatorError::NothingToDo.into());
                }
            }
        }

        if did_work {
            // Pay the caller a flat bounty out of the vault. The bounty is
            // tiny (CRANK_BOUNTY) and the conservation invariant is
            // preserved by decrementing `engine.vault` and `engine.c_tot`
            // by the same amount — but wait, we don't want to eat user
            // capital for crank bounties. Pay from insurance instead: if
            // `insurance_fund.balance >= CRANK_BOUNTY`, decrement that
            // and the vault. Otherwise pay whatever's available.
            let bounty: u64 = {
                let mut data = slab_account.try_borrow_mut_data()?;
                let engine_bytes =
                    &mut data[ENGINE_OFFSET..ENGINE_OFFSET + engine_region_size()];
                let engine: &mut percolator::RiskEngine =
                    unsafe { &mut *(engine_bytes.as_mut_ptr() as *mut percolator::RiskEngine) };
                let ins = engine.insurance_fund.balance.get();
                // SECURITY: bounty draws from insurance, not user capital,
                // so c_tot is NOT touched. Two-way decrement: insurance +
                // vault, which preserves `V >= C_tot + I`. The bounty is
                // also clamped against the configured `insurance_floor`
                // so repeated cranks cannot drain insurance below spec
                // (fixes SECURITY.md "Known limits" #2).
                let floor = engine.params.insurance_floor.get();
                let available = ins.saturating_sub(floor);
                let pay = core::cmp::min(available, CRANK_BOUNTY as u128) as u64;
                if pay > 0 {
                    let new_ins = ins - pay as u128;
                    engine.insurance_fund.balance = percolator::U128::new(new_ins);
                    let new_v = engine
                        .vault
                        .get()
                        .checked_sub(pay as u128)
                        .ok_or(PercolatorError::EngineError)?;
                    engine.vault = percolator::U128::new(new_v);
                }
                pay
            };
            if bounty > 0 {
                let ix = spl_token::instruction::transfer(
                    token_program.key,
                    vault_ta.key,
                    caller_ta.key,
                    vault_ta.key,
                    &[],
                    bounty,
                )?;
                let slab_key = slab_account.key.to_bytes();
                let vault_bump = [header.vault_bump];
                let seeds: &[&[u8]] = &[VAULT_SEED, slab_key.as_ref(), &vault_bump];
                invoke_signed(
                    &ix,
                    &[
                        vault_ta.clone(),
                        caller_ta.clone(),
                        vault_ta.clone(),
                        token_program.clone(),
                    ],
                    &[seeds],
                )?;
            }
            msg!(
                "Crank: kind={:?}, reclaimed={}, bounty={}",
                kind,
                reclaimed,
                bounty,
            );
        }
        Ok(())
    }

    /// Permissionless paid listing (task #23). Charges
    /// `MARKET_CREATION_FEE_LAMPORTS` to the payer, routes it to
    /// `TREASURY_PUBKEY`, and writes a header with `origin = ORIGIN_OPEN`.
    /// The slab account, like `CreateSlab`, must be pre-allocated by the
    /// client in a prior instruction of the same tx (to get past the
    /// 10-KiB CPI create_account cap).
    fn process_create_market(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        args: CreateMarketArgs,
    ) -> ProgramResult {
        let iter = &mut accounts.iter();
        let payer = next_account_info(iter)?;
        let slab_account = next_account_info(iter)?;
        let mint = next_account_info(iter)?;
        let oracle = next_account_info(iter)?;
        let treasury = next_account_info(iter)?;
        let system = next_account_info(iter)?;
        let _token_program = next_account_info(iter)?;

        // Basic guards mirror `process_create_slab` — paid listings share
        // the "pre-allocated, program-owned, zero-header" precondition.
        if !payer.is_signer {
            return Err(PercolatorError::MissingSigner.into());
        }
        if !payer.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if !slab_account.is_writable || !treasury.is_writable {
            return Err(PercolatorError::AccountNotWritable.into());
        }
        if *system.key != system_program::ID {
            return Err(PercolatorError::InvalidSystemProgram.into());
        }
        // SECURITY: the treasury must be the canonical pubkey. Otherwise
        // a paid market could route its fee to an attacker-controlled
        // account and still write ORIGIN_OPEN into the header.
        if *treasury.key != TREASURY_PUBKEY {
            return Err(PercolatorError::WrongTreasury.into());
        }

        let expected_size = slab_account_size();
        if slab_account.data_len() != expected_size {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        if slab_account.owner != program_id {
            return Err(PercolatorError::SlabAlreadyInitialized.into());
        }
        let rent = Rent::get()?;
        if !rent.is_exempt(slab_account.lamports(), expected_size) {
            return Err(PercolatorError::SlabSizeMismatch.into());
        }
        // Replay guard: refuse if the header already carries any non-zero
        // byte. Same invariant as CreateSlab.
        {
            let data = slab_account.try_borrow_data()?;
            if data[..SlabHeader::LEN].iter().any(|&b| b != 0) {
                return Err(PercolatorError::SlabAlreadyInitialized.into());
            }
        }

        // Verify the caller-supplied vault bump.
        Pubkey::create_program_address(
            &[VAULT_SEED, slab_account.key.as_ref(), &[args.vault_bump]],
            program_id,
        )
        .map_err(|_| PercolatorError::VaultPdaMismatch)?;

        // SECURITY: enforce the minimum listing fee on-chain. The wrapper
        // frontend sets `fee_lamports` per tier (first 10 listings = 0.5
        // SOL, then 1.5 SOL), but a malicious client could hand-craft an
        // ix with a lower value. Floor it here; let anything above the
        // floor through so env-based price hikes don't need a program
        // upgrade.
        if args.fee_lamports < MIN_MARKET_CREATION_FEE_LAMPORTS {
            return Err(PercolatorError::ListingFeeTooLow.into());
        }

        // CPI SystemProgram.transfer(payer → treasury, fee_lamports).
        // Native SOL only; token-denominated fees are out of scope.
        let transfer_ix = system_instruction::transfer(
            payer.key,
            treasury.key,
            args.fee_lamports,
        );
        invoke(
            &transfer_ix,
            &[payer.clone(), treasury.clone(), system.clone()],
        )?;

        // Write the header. `origin` goes to ORIGIN_OPEN to distinguish
        // from Day-1 seeded markets on the `/markets` page.
        let mut header =
            SlabHeader::new(*mint.key, *oracle.key, *payer.key, 0, args.vault_bump);
        header.origin = ORIGIN_OPEN;
        {
            let mut data = slab_account.try_borrow_mut_data()?;
            header.write_into(&mut data[..SlabHeader::LEN])?;
            // Defensive zero of engine region (like CreateSlab).
            for b in &mut data[ENGINE_OFFSET..] {
                *b = 0;
            }
        }

        msg!(
            "CreateMarket: mint={}, slab={}, payer={}, fee={}",
            mint.key,
            slab_account.key,
            payer.key,
            args.fee_lamports,
        );
        Ok(())
    }
}

/// Pre-flight validation of `RiskParamsArgs` before handing them to the parent
/// crate's `init_in_place`, whose own validator panics. This mirrors the
/// parent's `validate_params` checks. Skipping a check here only means the
/// parent will panic on the same input — harmless for correctness, bad for UX,
/// so keep these in sync if the parent grows new invariants.
fn validate_risk_params(
    p: &RiskParamsArgs,
    init_oracle_price: u64,
) -> Result<(), PercolatorError> {
    // Oracle price seed (spec §2.7)
    if init_oracle_price == 0 || init_oracle_price > percolator::MAX_ORACLE_PRICE {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Capacity (spec §1.4)
    if p.max_accounts == 0 || (p.max_accounts as usize) > percolator::MAX_ACCOUNTS {
        return Err(PercolatorError::InvalidRiskParams);
    }
    if p.max_active_positions_per_side == 0
        || p.max_active_positions_per_side > p.max_accounts
    {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Margin ordering and bounds (spec §1.4)
    // initial_margin_bps == 0 means "no margin required" = infinite leverage.
    // The parent doesn't forbid it, but it's a footgun — reject here.
    if p.initial_margin_bps == 0 {
        return Err(PercolatorError::InvalidRiskParams);
    }
    if p.maintenance_margin_bps > p.initial_margin_bps {
        return Err(PercolatorError::InvalidRiskParams);
    }
    if p.initial_margin_bps > percolator::MAX_MARGIN_BPS
        || p.trading_fee_bps > percolator::MAX_TRADING_FEE_BPS
        || p.liquidation_fee_bps > percolator::MAX_LIQUIDATION_FEE_BPS
    {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Nonzero margin floor ordering (spec §1.4)
    if p.min_nonzero_mm_req == 0
        || p.min_nonzero_mm_req >= p.min_nonzero_im_req
        || p.min_nonzero_im_req > p.min_initial_deposit
    {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Min initial deposit bounds (spec §1.4)
    if p.min_initial_deposit == 0 || p.min_initial_deposit > percolator::MAX_VAULT_TVL {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Liquidation fee ordering (spec §1.4)
    if p.min_liquidation_abs > p.liquidation_fee_cap
        || p.liquidation_fee_cap > percolator::MAX_PROTOCOL_FEE_ABS
    {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Insurance floor (spec §1.4)
    if p.insurance_floor > percolator::MAX_VAULT_TVL {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Warmup horizon (spec §6.1). h_max > 0 is required for any live
    // admission to succeed.
    if p.h_min > p.h_max || p.h_max == 0 {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Resolve price deviation bound (spec §10.7)
    if p.resolve_price_deviation_bps > percolator::MAX_RESOLVE_PRICE_DEVIATION_BPS {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Funding envelope (spec §1.4). "funding_cap=0" maps to either a zero per-
    // call accrual window (max_accrual_dt_slots = 0) or a zero cumulative
    // lifetime — both make funding accrual impossible; reject.
    if p.max_accrual_dt_slots == 0 {
        return Err(PercolatorError::InvalidRiskParams);
    }
    if (p.max_abs_funding_e9_per_slot as i128) > percolator::MAX_ABS_FUNDING_E9_PER_SLOT {
        return Err(PercolatorError::InvalidRiskParams);
    }
    if p.min_funding_lifetime_slots < p.max_accrual_dt_slots {
        return Err(PercolatorError::InvalidRiskParams);
    }

    // Envelope product must fit i128. u128 fits more than enough headroom to
    // compute ADL_ONE * MAX_ORACLE_PRICE * rate * dt and compare to i128::MAX.
    let adl = percolator::ADL_ONE;
    let px = percolator::MAX_ORACLE_PRICE as u128;
    let rate = p.max_abs_funding_e9_per_slot as u128;
    let envelope = adl
        .checked_mul(px)
        .and_then(|v| v.checked_mul(rate))
        .and_then(|v| v.checked_mul(p.max_accrual_dt_slots as u128));
    match envelope {
        Some(v) if v <= i128::MAX as u128 => {}
        _ => return Err(PercolatorError::InvalidRiskParams),
    }

    let lifetime = adl
        .checked_mul(px)
        .and_then(|v| v.checked_mul(rate))
        .and_then(|v| v.checked_mul(p.min_funding_lifetime_slots as u128));
    match lifetime {
        Some(v) if v <= i128::MAX as u128 => {}
        _ => return Err(PercolatorError::InvalidRiskParams),
    }

    Ok(())
}
