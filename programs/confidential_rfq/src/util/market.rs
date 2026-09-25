//! Market sizing, map updates, and delegation-address validation.

use crate::ConfidentialRfqError;
use crate::state::{CurrentAccountVersion, market::Market};
use anchor_lang::prelude::*;
use std::num::NonZeroU64;

const MARKET_HEADER_SPACE: usize = 8 + 1 + 32 + 1 + 4;
const MAKER_ENTRY_SPACE: usize = 8 + 32;

pub fn market_space(maker_count: usize) -> usize {
    MARKET_HEADER_SPACE + maker_count * MAKER_ENTRY_SPACE
}

pub fn add_maker_entry(market: &mut Market, maker_id: u64, maker: Pubkey) -> Result<()> {
    let maker_id = NonZeroU64::new(maker_id).ok_or(error!(ConfidentialRfqError::InvalidMakerId))?;
    if market.makers.insert(maker_id, maker).is_some() {
        return Err(error!(ConfidentialRfqError::MakerAlreadyExists));
    }
    Ok(())
}

pub fn remove_maker_entry(market: &mut Market, maker_id: u64) -> Result<()> {
    let maker_id = NonZeroU64::new(maker_id).ok_or(error!(ConfidentialRfqError::InvalidMakerId))?;
    require!(
        market.makers.remove(&maker_id).is_some(),
        ConfidentialRfqError::MakerNotFound
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CurrentAccountVersion, MakerId};
    use crate::util::pda::market_maker_group_address;
    use std::collections::BTreeMap;

    fn market() -> Market {
        Market {
            version: Market::VERSION,
            admin: Pubkey::new_unique(),
            maker_group_bump: 0,
            makers: BTreeMap::new(),
        }
    }

    #[test]
    fn market_space_matches_serialized_size_as_membership_changes() {
        let mut market = market();
        for maker_id in 1..=300 {
            let mut data = Vec::new();
            market.try_serialize(&mut data).unwrap();
            assert_eq!(data.len(), market_space(market.makers.len()));
            add_maker_entry(&mut market, maker_id, Pubkey::new_unique()).unwrap();
        }
        for maker_id in 1..=300 {
            let mut data = Vec::new();
            market.try_serialize(&mut data).unwrap();
            assert_eq!(data.len(), market_space(market.makers.len()));
            remove_maker_entry(&mut market, maker_id).unwrap();
        }
    }

    #[test]
    fn maker_ids_and_keys_are_unique_and_nonzero() {
        let mut market = market();
        let alice = Pubkey::new_unique();
        add_maker_entry(&mut market, 1, alice).unwrap();
        assert!(add_maker_entry(&mut market, 1, Pubkey::new_unique()).is_err());
        assert!(add_maker_entry(&mut market, 2, alice).is_err());
        assert!(add_maker_entry(&mut market, 0, Pubkey::new_unique()).is_err());
        assert!(add_maker_entry(&mut market, 2, Pubkey::default()).is_err());
        assert_eq!(market.makers.len(), 1);
        assert_eq!(market.makers[&MakerId::new(1).unwrap()], alice);
    }

    #[test]
    fn removed_maker_id_and_key_can_be_reused() {
        let mut market = market();
        let maker = Pubkey::new_unique();
        add_maker_entry(&mut market, 1, maker).unwrap();
        remove_maker_entry(&mut market, 1).unwrap();
        assert!(remove_maker_entry(&mut market, 1).is_err());
        add_maker_entry(&mut market, 1, maker).unwrap();
    }
}
