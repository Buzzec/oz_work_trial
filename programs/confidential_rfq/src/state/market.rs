use crate::{
    ConfidentialRfqError,
    state::{CurrentAccountVersion, MakerId},
    util::pda::market_maker_group_signer_seeds,
};
use anchor_lang::prelude::*;
use std::collections::BTreeMap;

/// This is pretty non-optimal. It would be preferable to use zero-copy for this but would need a map implementation.
#[account]
pub struct Market {
    pub version: u8,
    pub admin: Pubkey,
    pub maker_group_bump: u8,
    pub makers: BTreeMap<MakerId, Pubkey>,
}
impl CurrentAccountVersion for Market {
    const VERSION: u8 = 1;

    fn version(&self) -> u8 {
        self.version
    }
}

pub trait MarketExt {
    fn maker_group(&self) -> Result<Pubkey>;
}
impl<'info> MarketExt for Account<'info, Market> {
    fn maker_group(&self) -> Result<Pubkey> {
        Pubkey::create_program_address(&market_maker_group_signer_seeds(self), &crate::ID)
            .map_err(|_| error!(ConfidentialRfqError::InvalidRfqAccounts))
    }
}
