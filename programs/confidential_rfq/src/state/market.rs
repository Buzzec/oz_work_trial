use crate::{
    ConfidentialRfqError, state::CurrentAccountVersion, util::pda::market_maker_group_signer_seeds,
};
use anchor_lang::prelude::*;

#[account]
pub struct Market {
    pub version: u8,
    pub admin: Pubkey,
    pub maker_group_bump: u8,
    /// Sorted by maker ID; mutations go through `add_maker` and `remove_maker`.
    makers: Vec<MakerEntry>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
struct MakerEntry {
    maker_id: u64,
    maker: Pubkey,
}

impl Market {
    pub fn new(admin: Pubkey, maker_group_bump: u8) -> Self {
        Self {
            version: Self::VERSION,
            admin,
            maker_group_bump,
            makers: Vec::new(),
        }
    }

    /// Account discriminator, fixed fields, vector length, and maker entries.
    pub const fn space(maker_count: usize) -> usize {
        8 + 1 + 32 + 1 + 4 + maker_count * (8 + 32)
    }

    pub fn maker_count(&self) -> usize {
        self.makers.len()
    }

    pub fn maker(&self, maker_id: u64) -> Option<Pubkey> {
        self.maker_index(maker_id)
            .ok()
            .map(|index| self.makers[index].maker)
    }

    pub fn add_maker(&mut self, maker_id: u64, maker: Pubkey) -> Result<()> {
        require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
        let index = self
            .maker_index(maker_id)
            .err()
            .ok_or(error!(ConfidentialRfqError::MakerAlreadyExists))?;
        self.makers.insert(index, MakerEntry { maker_id, maker });
        Ok(())
    }

    pub fn remove_maker(&mut self, maker_id: u64) -> Result<()> {
        require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
        let index = self
            .maker_index(maker_id)
            .map_err(|_| error!(ConfidentialRfqError::MakerNotFound))?;
        self.makers.remove(index);
        Ok(())
    }

    fn maker_index(&self, maker_id: u64) -> std::result::Result<usize, usize> {
        self.makers
            .binary_search_by_key(&maker_id, |entry| entry.maker_id)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MakerId;
    use std::collections::BTreeMap;

    fn market() -> Market {
        Market::new(Pubkey::new_unique(), 0)
    }

    fn serialized(market: &Market) -> Vec<u8> {
        let mut data = Vec::new();
        market.try_serialize(&mut data).unwrap();
        data
    }

    #[test]
    fn makers_stay_sorted_through_insertions_and_removals() {
        let mut market = market();
        let makers = [30, 10, u64::MAX, 20, 1].map(|id| (id, Pubkey::new_unique()));
        for (id, maker) in makers {
            market.add_maker(id, maker).unwrap();
        }
        assert_eq!(
            market
                .makers
                .iter()
                .map(|entry| entry.maker_id)
                .collect::<Vec<_>>(),
            [1, 10, 20, 30, u64::MAX]
        );
        for (id, maker) in makers {
            assert_eq!(market.maker(id), Some(maker));
        }
        for missing in [0, 2, 15, 31] {
            assert_eq!(market.maker(missing), None);
        }
        for id in [20, 1, u64::MAX] {
            market.remove_maker(id).unwrap();
            assert_eq!(market.maker(id), None);
        }
        assert_eq!(market.maker_count(), 2);
        assert_eq!(market.maker(10), Some(makers[1].1));
        assert_eq!(market.maker(30), Some(makers[0].1));
        market.add_maker(20, makers[3].1).unwrap();
        assert_eq!(market.maker(20), Some(makers[3].1));
    }

    #[test]
    fn rejected_mutations_leave_existing_makers_unchanged() {
        let mut market = market();
        let alice = Pubkey::new_unique();
        market.add_maker(1, alice).unwrap();
        let before = serialized(&market);
        assert!(market.add_maker(1, Pubkey::new_unique()).is_err());
        assert!(market.add_maker(0, Pubkey::new_unique()).is_err());
        assert!(market.remove_maker(0).is_err());
        assert!(market.remove_maker(2).is_err());
        assert_eq!(serialized(&market), before);
        assert_eq!(market.maker(1), Some(alice));
    }

    #[test]
    fn maker_keys_can_be_reused_under_distinct_ids() {
        let mut market = market();
        let maker = Pubkey::new_unique();
        market.add_maker(2, maker).unwrap();
        market.add_maker(1, maker).unwrap();
        market.remove_maker(1).unwrap();
        assert_eq!(market.maker(2), Some(maker));
        market.add_maker(1, maker).unwrap();
        assert_eq!(market.maker(1), Some(maker));
    }

    #[test]
    fn market_space_matches_serialized_size_as_membership_changes() {
        let mut market = market();
        for maker_id in (1..=300).rev() {
            market.add_maker(maker_id, Pubkey::new_unique()).unwrap();
            assert_eq!(
                serialized(&market).len(),
                Market::space(market.maker_count())
            );
        }
        for maker_id in 1..=300 {
            market.remove_maker(maker_id).unwrap();
            assert_eq!(
                serialized(&market).len(),
                Market::space(market.maker_count())
            );
        }
    }

    #[test]
    fn sorted_vector_preserves_the_previous_map_account_encoding() {
        let mut market = market();
        let mut previous_makers = BTreeMap::new();
        for id in [30, 1, 20] {
            let maker = Pubkey::new_unique();
            market.add_maker(id, maker).unwrap();
            previous_makers.insert(MakerId::new(id).unwrap(), maker);
        }
        let mut previous_data = Market::DISCRIMINATOR.to_vec();
        AnchorSerialize::serialize(&market.version, &mut previous_data).unwrap();
        AnchorSerialize::serialize(&market.admin, &mut previous_data).unwrap();
        AnchorSerialize::serialize(&market.maker_group_bump, &mut previous_data).unwrap();
        AnchorSerialize::serialize(&previous_makers, &mut previous_data).unwrap();
        assert_eq!(serialized(&market), previous_data);
        let decoded = Market::try_deserialize(&mut previous_data.as_slice()).unwrap();
        for (id, maker) in previous_makers {
            assert_eq!(decoded.maker(id.get()), Some(maker));
        }
    }
}
