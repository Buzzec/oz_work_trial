use crate::{
    ConfidentialRfqError,
    state::{CurrentAccountVersion, extend_key, extend_key_u64},
};
use anchor_lang::prelude::*;
use derive_more::{Deref, DerefMut, From};
use std::num::NonZeroU64;
use zama_fhe::{Bool, FheHandle, Store, Uint};

#[account(zero_copy)]
#[repr(C, packed)]
#[derive(InitSpace)]
pub struct RFQ {
    pub version: u8,
    pub market: Pubkey,
    pub user: Pubkey,
    pub bump: u8,
    pub timeout: i64,
    pub bid_count: u64,
    pub asset_mint: Pubkey,
    pub basis_mint: Pubkey,
}
impl CurrentAccountVersion for RFQ {
    const VERSION: u8 = 1;

    fn version(&self) -> u8 {
        self.version
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RFQPrivateField {
    UserBuyer,
    UserClaimed,
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

#[derive(Deref, DerefMut, From)]
pub struct RFQStore<'a>(pub Store<'a>);
impl<'a> RFQStore<'a> {
    pub fn user_buyer(&self) -> Result<FheHandle<Bool>> {
        self.get(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)
    }

    pub fn user_claimed(&self) -> Result<FheHandle<Bool>> {
        self.get(RFQPrivateField::UserClaimed.key())
            .map_err(invalid_fhe)
    }

    pub fn offer_limit(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::OfferLimit.key())
            .map_err(invalid_fhe)
    }

    pub fn size(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::Size.key()).map_err(invalid_fhe)
    }

    pub fn best_offer(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::BestOffer.key())
            .map_err(invalid_fhe)
    }

    pub fn best_maker(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)
    }

    pub fn maker_buy(&self, id: NonZeroU64) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::MakerBuy(id).key())
            .map_err(invalid_fhe)
    }

    pub fn maker_sell(&self, id: NonZeroU64) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::MakerSell(id).key())
            .map_err(invalid_fhe)
    }

    pub fn closed_bids(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::ClosedBids.key())
            .map_err(invalid_fhe)
    }

    pub fn can_close(&self) -> Result<FheHandle<Bool>> {
        self.get(RFQPrivateField::CanClose.key())
            .map_err(invalid_fhe)
    }
}

pub fn invalid_fhe(error: zama_fhe::FheExecutionBuildError) -> Error {
    msg!("invalid FHE execution: {:?}", error);
    error!(ConfidentialRfqError::InvalidFheExecution)
}
