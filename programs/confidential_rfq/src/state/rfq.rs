//! Public RFQ identity and typed access to its encrypted primary and maker stores.

use crate::{
    ConfidentialRfqError,
    state::{CurrentAccountVersion, extend_key},
};
use anchor_lang::prelude::*;
use derive_more::{Deref, DerefMut, From};
use zama_fhe::{Bool, FheHandle, Store, Uint};

/// Maximum lifetime accepted by the encrypted creation check: seven days.
pub const MAXIMUM_TIMEOUT: u64 = 7 * 24 * 60 * 60;

#[account(zero_copy)]
#[repr(C, packed)]
#[derive(InitSpace)]
pub struct RFQ {
    pub version: u8,
    pub nonce: [u8; 32],
    pub market: Pubkey,
    pub user: Pubkey,
    pub bump: u8,
    pub funder_bump: u8,
    pub open_stores: u32,
    pub asset_mint: Pubkey,
    pub basis_mint: Pubkey,
}
impl CurrentAccountVersion for RFQ {
    const VERSION: u8 = 2;
    fn version(&self) -> u8 {
        self.version
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RFQState {
    Invalid = 0,
    Valid = 1,
    Canceled = 2,
    Expired = 3,
    Claimable = 4,
    Claimed = 5,
    /// Terms are valid, but the user has not atomically funded both escrows yet.
    Unfunded = 6,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RFQPrivateField {
    State,
    ExpireTimestamp,
    BidCount,
    BidSeq,
    SearchedBids,
    UserBuyer,
    OfferLimit,
    Size,
    BestOffer,
    BestMaker,
    BestMakerIndex,
    /// Public derived predicate certified before closing the native RFQ account.
    CanClose,
}
impl RFQPrivateField {
    pub fn key(self) -> [u8; 32] {
        extend_key(match self {
            Self::State => b"state",
            Self::ExpireTimestamp => b"expire_timestamp",
            Self::BidCount => b"bid_count",
            Self::BidSeq => b"bid_seq",
            Self::SearchedBids => b"searched_bids",
            Self::UserBuyer => b"user_buyer",
            Self::OfferLimit => b"offer_limit",
            Self::Size => b"size",
            Self::BestOffer => b"best_offer",
            Self::BestMaker => b"best_maker",
            Self::BestMakerIndex => b"best_maker_index",
            Self::CanClose => b"can_close",
        })
    }
}

#[derive(Deref, DerefMut, From)]
pub struct RFQStore<'a>(pub Store<'a>);
impl RFQStore<'_> {
    pub fn state(&self) -> Result<FheHandle<Uint<8>>> {
        self.get(RFQPrivateField::State.key()).map_err(invalid_fhe)
    }
    pub fn expire_timestamp(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(RFQPrivateField::ExpireTimestamp.key())
            .map_err(invalid_fhe)
    }
    pub fn bid_count(&self) -> Result<FheHandle<Uint<32>>> {
        self.get(RFQPrivateField::BidCount.key())
            .map_err(invalid_fhe)
    }
    pub fn bid_seq(&self) -> Result<FheHandle<Uint<32>>> {
        self.get(RFQPrivateField::BidSeq.key()).map_err(invalid_fhe)
    }
    pub fn searched_bids(&self) -> Result<FheHandle<Uint<32>>> {
        self.get(RFQPrivateField::SearchedBids.key())
            .map_err(invalid_fhe)
    }
    pub fn user_buyer(&self) -> Result<FheHandle<Bool>> {
        self.get(RFQPrivateField::UserBuyer.key())
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
    pub fn best_maker(&self) -> Result<FheHandle<Uint<32>>> {
        self.get(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)
    }
    pub fn best_maker_index(&self) -> Result<FheHandle<Uint<32>>> {
        self.get(RFQPrivateField::BestMakerIndex.key())
            .map_err(invalid_fhe)
    }
    pub fn can_close(&self) -> Result<FheHandle<Bool>> {
        self.get(RFQPrivateField::CanClose.key())
            .map_err(invalid_fhe)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MakerPrivateField {
    Buy,
    Sell,
    Sequence,
}
impl MakerPrivateField {
    pub fn key(self) -> [u8; 32] {
        extend_key(match self {
            Self::Buy => b"maker_buy",
            Self::Sell => b"maker_sell",
            Self::Sequence => b"maker_seq",
        })
    }
}

#[derive(Deref, DerefMut, From)]
pub struct MakerStore<'a>(pub Store<'a>);
impl MakerStore<'_> {
    pub fn buy(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(MakerPrivateField::Buy.key()).map_err(invalid_fhe)
    }
    pub fn sell(&self) -> Result<FheHandle<Uint<64>>> {
        self.get(MakerPrivateField::Sell.key()).map_err(invalid_fhe)
    }
    pub fn sequence(&self) -> Result<FheHandle<Uint<32>>> {
        self.get(MakerPrivateField::Sequence.key())
            .map_err(invalid_fhe)
    }
}

pub fn invalid_fhe(error: zama_fhe::FheExecutionBuildError) -> Error {
    msg!("invalid FHE execution: {:?}", error);
    error!(ConfidentialRfqError::InvalidFheExecution)
}
