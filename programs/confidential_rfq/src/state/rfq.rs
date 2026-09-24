use crate::{CurrentAccountVersion, extend_key, extend_key_u64};
use anchor_lang::prelude::*;
use std::num::NonZeroU64;

pub const RFQ_AUTHORITY_SEED: &[u8] = b"rfq_authority";

#[account(zero_copy)]
#[repr(C, packed)]
pub struct RFQ {
    pub version: u8,
    pub market: Pubkey,
    /// Should be randomly derived from slot-hashes
    pub nonce: [u8; 32],
    pub authority_bump: u8,
    pub user: Pubkey,
    pub bid_count: u64,
    pub asset_mint: Pubkey,
    pub basis_mint: Pubkey,
}
impl CurrentAccountVersion for RFQ {
    const VERSION: u8 = 1;
}

pub enum RFQPrivateField {
    UserBuyer,
    UserClaimed,
    Timeout,
    OfferLimit,
    Size,
    BestOffer,
    BestMaker,
    MakerBuy(NonZeroU64),
    MakerSell(NonZeroU64),
    ClosedBids,
    CanClose,
}
impl RFQPrivateField {
    pub fn key(&self) -> [u8; 32] {
        match self {
            RFQPrivateField::UserBuyer => extend_key(b"user_buyer"),
            RFQPrivateField::UserClaimed => extend_key(b"user_claimed"),
            RFQPrivateField::Timeout => extend_key(b"timeout"),
            RFQPrivateField::OfferLimit => extend_key(b"offer_limit"),
            RFQPrivateField::Size => extend_key(b"size"),
            RFQPrivateField::BestOffer => extend_key(b"best_offer"),
            RFQPrivateField::BestMaker => extend_key(b"best_maker"),
            RFQPrivateField::MakerBuy(id) => extend_key_u64(b"maker_buy_", id.get()),
            RFQPrivateField::MakerSell(id) => extend_key_u64(b"maker_sell_", id.get()),
            RFQPrivateField::ClosedBids => extend_key(b"closed_bids"),
            RFQPrivateField::CanClose => extend_key(b"can_close"),
        }
    }
}
