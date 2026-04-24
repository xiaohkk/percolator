//! On-the-wire instruction format for the Percolator program.
//!
//! Encoding is a leading `u8` discriminator followed by Borsh-encoded fields
//! for the variant. This keeps the hot dispatch path branch-lean and lets us
//! grow the enum without disturbing existing discriminants.
//!
//! NOTE: Only `CreateSlab` carries meaningful payload today. The other
//! variants are placeholder shapes so the dispatcher wiring is real; they'll
//! be fleshed out as we wrap more of the engine.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::program_error::ProgramError;

use crate::error::PercolatorError;

/// Discriminators. These are part of the on-chain ABI — append new variants,
/// never reorder.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InstructionTag {
    CreateSlab = 0,
    Deposit = 1,
    Withdraw = 2,
    PlaceOrder = 3,
    Liquidate = 4,
    Crank = 5,
    InitializeEngine = 6,
    /// Admin one-shot: seed slot 0 as the protocol LP counterparty. A
    /// Percolator slab uses the engine's two-sided matcher internally, so
    /// the wrapper reserves slot 0 for the house LP and trades every
    /// `PlaceOrder` against it.
    BootstrapLp = 7,
    /// Permissionless paid listing. Transfers `MARKET_CREATION_FEE_LAMPORTS`
    /// from the payer to `TREASURY_PUBKEY` and writes a SlabHeader with
    /// `origin = ORIGIN_OPEN`. Task #23.
    CreateMarket = 8,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CreateSlabArgs {
    /// Caller-supplied PDA bump, stored in the header verbatim. The engine
    /// doesn't care; downstream wrappers do.
    pub bump: u8,
    /// PDA bump for the slab's vault token account, seeds
    /// `[b"vault", slab.pubkey]`. Client pre-computes via
    /// `find_program_address`; the program verifies with
    /// `create_program_address` and stores it in `SlabHeader.vault_bump`.
    /// Persisting the bump lets every downstream `invoke_signed` path
    /// reference the vault without re-grinding the address.
    pub vault_bump: u8,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct DepositArgs {
    pub amount: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct WithdrawArgs {
    pub amount: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlaceOrderArgs {
    /// 0 = long, 1 = short.
    pub side: u8,
    /// Order size in `POS_SCALE` units (1e6). Must be > 0.
    pub size: u64,
    /// Slippage guard upper bound (oracle price must be `<= max_price`).
    pub max_price: u64,
    /// Slippage guard lower bound (oracle price must be `>= min_price`).
    pub min_price: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct BootstrapLpArgs {
    /// Token amount the creator is funding the protocol LP with. Becomes
    /// the LP's starting capital and the first tokens in the slab vault.
    pub amount: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CreateMarketArgs {
    /// PDA bump for the slab's vault token account. Same semantics as
    /// `CreateSlabArgs.vault_bump`: verified on-chain with
    /// `create_program_address` and stored in `SlabHeader.vault_bump`.
    pub vault_bump: u8,
    /// Native SOL fee the caller is declaring they'll transfer to
    /// `TREASURY_PUBKEY` as part of this listing. The program enforces
    /// `fee_lamports >= MIN_MARKET_CREATION_FEE_LAMPORTS` and then CPI-
    /// transfers exactly this amount from payer to treasury.
    ///
    /// Rationale (task #23 v2): the wrapper frontend gates per-tier
    /// pricing (e.g. first 10 listings = 0.5 SOL, then 1.5 SOL) by
    /// counting `origin=ORIGIN_OPEN` slabs off-chain and picking the
    /// right fee. Encoding the fee in args (not constants) lets the
    /// pricing curve change without a program upgrade. The on-chain
    /// floor prevents the frontend from being bypassed entirely.
    pub fee_lamports: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct LiquidateArgs {
    /// Engine slot of the account being liquidated.
    pub victim_slot: u16,
}

/// Permissionless crank kinds (task #14). Encoded as `u8` on the wire.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CrankKind {
    /// Advance `last_market_slot` via `accrue_market_to` (funding update).
    Funding = 0,
    /// Sweep dust / flat-clean accounts up to `GC_CLOSE_BUDGET`.
    Gc = 1,
    /// Advance the ADL state machine on any side whose reset is ready.
    AdlReset = 2,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CrankArgs {
    /// `CrankKind` as `u8`: 0=Funding, 1=Gc, 2=AdlReset.
    pub kind: u8,
}

/// Borsh-encodable mirror of `percolator::RiskParams`.
///
/// The parent crate's `RiskParams` stores a few fields as `U128` (a BPF-safe
/// newtype around `[u64; 2]`) so it can't derive Borsh directly. This mirror
/// carries the same values as plain `u128` for on-the-wire transport, then
/// converts back via `into_engine_params()`.
///
/// Field order and meaning mirror the parent crate one-for-one. If the parent
/// grows a field, add it here in the same position.
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskParamsArgs {
    pub maintenance_margin_bps: u64,
    pub initial_margin_bps: u64,
    pub trading_fee_bps: u64,
    pub max_accounts: u64,
    pub max_crank_staleness_slots: u64,
    pub liquidation_fee_bps: u64,
    pub liquidation_fee_cap: u128,
    pub min_liquidation_abs: u128,
    pub min_initial_deposit: u128,
    pub min_nonzero_mm_req: u128,
    pub min_nonzero_im_req: u128,
    pub insurance_floor: u128,
    pub h_min: u64,
    pub h_max: u64,
    pub resolve_price_deviation_bps: u64,
    pub max_accrual_dt_slots: u64,
    pub max_abs_funding_e9_per_slot: u64,
    pub min_funding_lifetime_slots: u64,
    pub max_active_positions_per_side: u64,
}

impl RiskParamsArgs {
    /// Convert to the parent crate's `RiskParams`. Pure field shuffling; does
    /// not validate — callers must validate separately before handing this to
    /// `init_in_place`, since the parent's validator panics.
    pub fn into_engine_params(self) -> percolator::RiskParams {
        percolator::RiskParams {
            maintenance_margin_bps: self.maintenance_margin_bps,
            initial_margin_bps: self.initial_margin_bps,
            trading_fee_bps: self.trading_fee_bps,
            max_accounts: self.max_accounts,
            max_crank_staleness_slots: self.max_crank_staleness_slots,
            liquidation_fee_bps: self.liquidation_fee_bps,
            liquidation_fee_cap: percolator::U128::new(self.liquidation_fee_cap),
            min_liquidation_abs: percolator::U128::new(self.min_liquidation_abs),
            min_initial_deposit: percolator::U128::new(self.min_initial_deposit),
            min_nonzero_mm_req: self.min_nonzero_mm_req,
            min_nonzero_im_req: self.min_nonzero_im_req,
            insurance_floor: percolator::U128::new(self.insurance_floor),
            h_min: self.h_min,
            h_max: self.h_max,
            resolve_price_deviation_bps: self.resolve_price_deviation_bps,
            max_accrual_dt_slots: self.max_accrual_dt_slots,
            max_abs_funding_e9_per_slot: self.max_abs_funding_e9_per_slot,
            min_funding_lifetime_slots: self.min_funding_lifetime_slots,
            max_active_positions_per_side: self.max_active_positions_per_side,
        }
    }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct InitializeEngineArgs {
    pub risk_params: RiskParamsArgs,
    /// Initial oracle price seed for the market. Must be in
    /// `(0, MAX_ORACLE_PRICE]` (spec §2.7). The parent's in-place
    /// initializer asserts this, so we pre-validate to return a clean
    /// `InvalidRiskParams` error instead of panicking.
    pub init_oracle_price: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PercolatorInstruction {
    CreateSlab(CreateSlabArgs),
    Deposit(DepositArgs),
    Withdraw(WithdrawArgs),
    PlaceOrder(PlaceOrderArgs),
    Liquidate(LiquidateArgs),
    Crank(CrankArgs),
    InitializeEngine(InitializeEngineArgs),
    BootstrapLp(BootstrapLpArgs),
    CreateMarket(CreateMarketArgs),
}

impl PercolatorInstruction {
    /// Decode an instruction from its raw `u8`-tagged wire form.
    pub fn unpack(input: &[u8]) -> Result<Self, ProgramError> {
        let (&tag, rest) = input
            .split_first()
            .ok_or(ProgramError::from(PercolatorError::InvalidInstructionData))?;

        let decode_err = || ProgramError::from(PercolatorError::InvalidInstructionData);

        let ix = match tag {
            t if t == InstructionTag::CreateSlab as u8 => {
                PercolatorInstruction::CreateSlab(
                    CreateSlabArgs::try_from_slice(rest).map_err(|_| decode_err())?,
                )
            }
            t if t == InstructionTag::Deposit as u8 => PercolatorInstruction::Deposit(
                DepositArgs::try_from_slice(rest).map_err(|_| decode_err())?,
            ),
            t if t == InstructionTag::Withdraw as u8 => PercolatorInstruction::Withdraw(
                WithdrawArgs::try_from_slice(rest).map_err(|_| decode_err())?,
            ),
            t if t == InstructionTag::PlaceOrder as u8 => PercolatorInstruction::PlaceOrder(
                PlaceOrderArgs::try_from_slice(rest).map_err(|_| decode_err())?,
            ),
            t if t == InstructionTag::Liquidate as u8 => PercolatorInstruction::Liquidate(
                LiquidateArgs::try_from_slice(rest).map_err(|_| decode_err())?,
            ),
            t if t == InstructionTag::Crank as u8 => PercolatorInstruction::Crank(
                CrankArgs::try_from_slice(rest).map_err(|_| decode_err())?,
            ),
            t if t == InstructionTag::InitializeEngine as u8 => {
                PercolatorInstruction::InitializeEngine(
                    InitializeEngineArgs::try_from_slice(rest).map_err(|_| decode_err())?,
                )
            }
            t if t == InstructionTag::BootstrapLp as u8 => PercolatorInstruction::BootstrapLp(
                BootstrapLpArgs::try_from_slice(rest).map_err(|_| decode_err())?,
            ),
            t if t == InstructionTag::CreateMarket as u8 => PercolatorInstruction::CreateMarket(
                CreateMarketArgs::try_from_slice(rest).map_err(|_| decode_err())?,
            ),
            _ => return Err(decode_err()),
        };
        Ok(ix)
    }

    /// Serialize to the tagged wire form. Matches `unpack`.
    pub fn pack(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        match self {
            PercolatorInstruction::CreateSlab(a) => {
                buf.push(InstructionTag::CreateSlab as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::Deposit(a) => {
                buf.push(InstructionTag::Deposit as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::Withdraw(a) => {
                buf.push(InstructionTag::Withdraw as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::PlaceOrder(a) => {
                buf.push(InstructionTag::PlaceOrder as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::Liquidate(a) => {
                buf.push(InstructionTag::Liquidate as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::Crank(a) => {
                buf.push(InstructionTag::Crank as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::InitializeEngine(a) => {
                buf.push(InstructionTag::InitializeEngine as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::BootstrapLp(a) => {
                buf.push(InstructionTag::BootstrapLp as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            PercolatorInstruction::CreateMarket(a) => {
                buf.push(InstructionTag::CreateMarket as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
        }
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_create_slab() {
        let ix = PercolatorInstruction::CreateSlab(CreateSlabArgs {
            bump: 253,
            vault_bump: 250,
        });
        let bytes = ix.pack();
        assert_eq!(bytes[0], InstructionTag::CreateSlab as u8);
        let decoded = PercolatorInstruction::unpack(&bytes).unwrap();
        assert_eq!(decoded, ix);
    }

    #[test]
    fn roundtrip_all_variants() {
        let cases = [
            PercolatorInstruction::CreateSlab(CreateSlabArgs {
                bump: 0,
                vault_bump: 255,
            }),
            PercolatorInstruction::Deposit(DepositArgs { amount: 12_345 }),
            PercolatorInstruction::Withdraw(WithdrawArgs { amount: 99 }),
            PercolatorInstruction::PlaceOrder(PlaceOrderArgs {
                side: 1,
                size: 1_000_000,
                max_price: 2_000,
                min_price: 1_000,
            }),
            PercolatorInstruction::Liquidate(LiquidateArgs { victim_slot: 17 }),
            PercolatorInstruction::Crank(CrankArgs {
                kind: CrankKind::Funding as u8,
            }),
            PercolatorInstruction::InitializeEngine(InitializeEngineArgs {
                risk_params: sample_risk_params_args(),
                init_oracle_price: 1_000,
            }),
            PercolatorInstruction::BootstrapLp(BootstrapLpArgs { amount: 1_000_000 }),
            PercolatorInstruction::CreateMarket(CreateMarketArgs {
                vault_bump: 250,
                fee_lamports: 500_000_000,
            }),
        ];
        for ix in cases {
            let bytes = ix.pack();
            let decoded = PercolatorInstruction::unpack(&bytes).unwrap();
            assert_eq!(decoded, ix);
        }
    }

    #[test]
    fn roundtrip_initialize_engine_preserves_every_field() {
        let args = InitializeEngineArgs {
            risk_params: sample_risk_params_args(),
            init_oracle_price: 4_242,
        };
        let ix = PercolatorInstruction::InitializeEngine(args.clone());
        let bytes = ix.pack();
        assert_eq!(bytes[0], InstructionTag::InitializeEngine as u8);
        let decoded = PercolatorInstruction::unpack(&bytes).unwrap();
        match decoded {
            PercolatorInstruction::InitializeEngine(got) => assert_eq!(got, args),
            _ => panic!("wrong variant decoded"),
        }
    }

    /// Spec-valid `RiskParamsArgs`, used as a starting point for tests. Mirrors
    /// the bounds enforced by the parent crate's `validate_params`.
    fn sample_risk_params_args() -> RiskParamsArgs {
        RiskParamsArgs {
            maintenance_margin_bps: 500,
            initial_margin_bps: 1_000,
            trading_fee_bps: 10,
            max_accounts: 64,
            max_crank_staleness_slots: 1_000,
            liquidation_fee_bps: 100,
            liquidation_fee_cap: 1_000_000_000_000,
            min_liquidation_abs: 10_000,
            min_initial_deposit: 1_000_000_000,
            min_nonzero_mm_req: 100,
            min_nonzero_im_req: 200,
            insurance_floor: 0,
            h_min: 10,
            h_max: 1_000,
            resolve_price_deviation_bps: 500,
            max_accrual_dt_slots: 1_000,
            max_abs_funding_e9_per_slot: 100,
            min_funding_lifetime_slots: 1_000,
            max_active_positions_per_side: 64,
        }
    }

    #[test]
    fn empty_input_rejected() {
        assert!(PercolatorInstruction::unpack(&[]).is_err());
    }

    #[test]
    fn unknown_tag_rejected() {
        assert!(PercolatorInstruction::unpack(&[255, 0, 0, 0]).is_err());
    }
}
