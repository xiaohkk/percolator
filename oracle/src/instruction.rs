//! On-the-wire instruction format. Leading `u8` discriminator + Borsh body.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::error::OracleError;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InstructionTag {
    InitializeFeed = 0,
    Update = 1,
    Graduate = 2,
    ConvertSource = 3,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct InitializeFeedArgs {
    pub mint: [u8; 32],
    pub source: [u8; 32],
    /// `SourceKind` as u8.
    pub source_kind: u8,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ConvertSourceArgs {
    /// New `SourceKind` after graduation.
    pub new_kind: u8,
    pub new_source: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OracleInstruction {
    InitializeFeed(InitializeFeedArgs),
    /// Permissionless: reads the source account, updates the feed. No args.
    Update,
    /// Permissionless once the bonding curve's `complete` flag is set.
    Graduate,
    ConvertSource(ConvertSourceArgs),
}

impl OracleInstruction {
    pub fn unpack(input: &[u8]) -> Result<Self, ProgramError> {
        let (&tag, rest) = input
            .split_first()
            .ok_or(ProgramError::from(OracleError::InvalidInstructionData))?;
        let decode = || ProgramError::from(OracleError::InvalidInstructionData);

        let ix = match tag {
            t if t == InstructionTag::InitializeFeed as u8 => OracleInstruction::InitializeFeed(
                InitializeFeedArgs::try_from_slice(rest).map_err(|_| decode())?,
            ),
            t if t == InstructionTag::Update as u8 => OracleInstruction::Update,
            t if t == InstructionTag::Graduate as u8 => OracleInstruction::Graduate,
            t if t == InstructionTag::ConvertSource as u8 => OracleInstruction::ConvertSource(
                ConvertSourceArgs::try_from_slice(rest).map_err(|_| decode())?,
            ),
            _ => return Err(decode()),
        };
        Ok(ix)
    }

    pub fn pack(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(80);
        match self {
            OracleInstruction::InitializeFeed(a) => {
                buf.push(InstructionTag::InitializeFeed as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
            OracleInstruction::Update => {
                buf.push(InstructionTag::Update as u8);
            }
            OracleInstruction::Graduate => {
                buf.push(InstructionTag::Graduate as u8);
            }
            OracleInstruction::ConvertSource(a) => {
                buf.push(InstructionTag::ConvertSource as u8);
                a.serialize(&mut buf).expect("borsh infallible on Vec");
            }
        }
        buf
    }
}

/// Helper: encode a Pubkey as a `[u8; 32]` for Borsh args.
pub fn pk_bytes(p: &Pubkey) -> [u8; 32] {
    p.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_initialize_feed() {
        let ix = OracleInstruction::InitializeFeed(InitializeFeedArgs {
            mint: [1u8; 32],
            source: [2u8; 32],
            source_kind: 0,
        });
        let bytes = ix.pack();
        assert_eq!(bytes[0], InstructionTag::InitializeFeed as u8);
        assert_eq!(OracleInstruction::unpack(&bytes).unwrap(), ix);
    }

    #[test]
    fn roundtrip_update_graduate() {
        let bytes = OracleInstruction::Update.pack();
        assert_eq!(
            OracleInstruction::unpack(&bytes).unwrap(),
            OracleInstruction::Update
        );
        let bytes = OracleInstruction::Graduate.pack();
        assert_eq!(
            OracleInstruction::unpack(&bytes).unwrap(),
            OracleInstruction::Graduate
        );
    }

    #[test]
    fn roundtrip_convert_source() {
        let ix = OracleInstruction::ConvertSource(ConvertSourceArgs {
            new_kind: 2,
            new_source: [9u8; 32],
        });
        let bytes = ix.pack();
        assert_eq!(OracleInstruction::unpack(&bytes).unwrap(), ix);
    }

    #[test]
    fn unknown_tag_rejected() {
        assert!(OracleInstruction::unpack(&[255]).is_err());
    }
}
