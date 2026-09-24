use crate::{CurrentAccountVersion, MakerId};
use anchor_lang::prelude::*;
use std::collections::BTreeMap;

pub const MARKET_MAKER_GROUP_SEED: &[u8] = b"market_maker_group";

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
}
