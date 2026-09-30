use crate::{
    ConfidentialRfqError, state::CurrentAccountVersion, util::pda::market_maker_group_signer_seeds,
};
use anchor_lang::prelude::*;

#[account]
pub struct Market {
    pub version: u8,
    pub admin: Pubkey,
    pub maker_group_bump: u8,
    /// Sorted by maker ID. Disabled entries retain ownership of outstanding bids.
    makers: Vec<MakerEntry>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
struct MakerEntry {
    maker_id: u32,
    maker: Pubkey,
    active: bool,
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

    /// Account discriminator, fixed fields, vector length, and retained maker entries.
    pub const fn space(maker_count: usize) -> usize {
        8 + 1 + 32 + 1 + 4 + maker_count * (4 + 32 + 1)
    }

    pub fn maker_count(&self) -> usize {
        self.makers.len()
    }

    /// Historical identity remains available after a maker is disabled.
    pub fn maker(&self, maker_id: u32) -> Option<Pubkey> {
        if self.version != Self::VERSION {
            return None;
        }
        self.maker_index(maker_id)
            .ok()
            .map(|index| self.makers[index].maker)
    }

    /// Only active entries may submit or update bids.
    pub fn active_maker(&self, maker_id: u32) -> Option<Pubkey> {
        if self.version != Self::VERSION {
            return None;
        }
        self.maker_index(maker_id)
            .ok()
            .map(|index| &self.makers[index])
            .filter(|entry| entry.active)
            .map(|entry| entry.maker)
    }

    pub fn add_maker(&mut self, maker_id: u32, maker: Pubkey) -> Result<()> {
        require_eq!(
            self.version,
            Self::VERSION,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
        require_keys_neq!(
            maker,
            Pubkey::default(),
            ConfidentialRfqError::InvalidMakerKey
        );
        match self.maker_index(maker_id) {
            Ok(index) => {
                let entry = &mut self.makers[index];
                require!(
                    !entry.active && entry.maker == maker,
                    ConfidentialRfqError::MakerAlreadyExists
                );
                entry.active = true;
            }
            Err(index) => {
                require!(
                    self.makers.iter().all(|entry| entry.maker != maker),
                    ConfidentialRfqError::MakerKeyAlreadyExists
                );
                self.makers.insert(
                    index,
                    MakerEntry {
                        maker_id,
                        maker,
                        active: true,
                    },
                );
            }
        }
        Ok(())
    }

    pub fn remove_maker(&mut self, maker_id: u32) -> Result<()> {
        require_eq!(
            self.version,
            Self::VERSION,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
        let index = self
            .maker_index(maker_id)
            .map_err(|_| error!(ConfidentialRfqError::MakerNotFound))?;
        require!(
            self.makers[index].active,
            ConfidentialRfqError::MakerNotFound
        );
        self.makers[index].active = false;
        Ok(())
    }

    fn maker_index(&self, maker_id: u32) -> std::result::Result<usize, usize> {
        self.makers
            .binary_search_by_key(&maker_id, |entry| entry.maker_id)
    }
}
impl CurrentAccountVersion for Market {
    const VERSION: u8 = 2;

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

    fn market() -> Market {
        Market::new(Pubkey::new_unique(), 0)
    }

    fn serialized(market: &Market) -> Vec<u8> {
        let mut data = Vec::new();
        market.try_serialize(&mut data).unwrap();
        data
    }

    #[test]
    fn disabled_makers_keep_their_identity_and_sorted_position() {
        let mut market = market();
        let makers = [30, 10, u32::MAX, 20, 1].map(|id| (id, Pubkey::new_unique()));
        for (id, maker) in makers {
            market.add_maker(id, maker).unwrap();
        }
        assert_eq!(
            market
                .makers
                .iter()
                .map(|entry| entry.maker_id)
                .collect::<Vec<_>>(),
            [1, 10, 20, 30, u32::MAX]
        );
        for (id, maker) in makers {
            assert_eq!(market.active_maker(id), Some(maker));
            market.remove_maker(id).unwrap();
            assert_eq!(market.active_maker(id), None);
            assert_eq!(market.maker(id), Some(maker));
        }
        assert_eq!(market.maker_count(), makers.len());
        market.add_maker(20, makers[3].1).unwrap();
        assert_eq!(market.active_maker(20), Some(makers[3].1));
    }

    #[test]
    fn rejected_mutations_preserve_current_and_historical_membership() {
        let mut market = market();
        let alice = Pubkey::new_unique();
        market.add_maker(1, alice).unwrap();
        let before = serialized(&market);
        assert!(market.add_maker(1, alice).is_err());
        assert!(market.add_maker(1, Pubkey::new_unique()).is_err());
        assert!(market.add_maker(0, Pubkey::new_unique()).is_err());
        assert!(market.add_maker(2, Pubkey::default()).is_err());
        assert!(market.add_maker(2, alice).is_err());
        assert!(market.remove_maker(0).is_err());
        assert!(market.remove_maker(2).is_err());
        assert_eq!(serialized(&market), before);
        market.remove_maker(1).unwrap();
        let disabled = serialized(&market);
        assert!(market.add_maker(1, Pubkey::new_unique()).is_err());
        assert!(market.add_maker(2, alice).is_err());
        assert!(market.remove_maker(1).is_err());
        assert_eq!(serialized(&market), disabled);
    }

    #[test]
    fn unsupported_market_versions_cannot_authorize_or_change_membership() {
        let mut market = market();
        let alice = Pubkey::new_unique();
        market.add_maker(1, alice).unwrap();
        market.version = 1;
        assert_eq!(market.maker(1), None);
        assert_eq!(market.active_maker(1), None);
        let before = serialized(&market);
        assert!(market.add_maker(2, Pubkey::new_unique()).is_err());
        assert!(market.remove_maker(1).is_err());
        assert_eq!(serialized(&market), before);
    }

    #[test]
    fn market_space_accounts_for_retained_disabled_entries() {
        let mut market = market();
        for maker_id in (1..=300).rev() {
            market.add_maker(maker_id, Pubkey::new_unique()).unwrap();
            assert_eq!(
                serialized(&market).len(),
                Market::space(market.maker_count())
            );
        }
        let full_size = serialized(&market).len();
        for maker_id in 1..=300 {
            market.remove_maker(maker_id).unwrap();
            assert_eq!(serialized(&market).len(), full_size);
        }
    }
}
