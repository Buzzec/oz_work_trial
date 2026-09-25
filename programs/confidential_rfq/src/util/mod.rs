pub mod bid_cpi;
pub mod close_rfq_cpi;
pub mod cpi;
pub mod pda;
pub mod request_quote_cpi;
pub mod rfq;
pub mod token_side;

use crate::ConfidentialRfqError;
use anchor_lang::prelude::*;
use derive_more::Deref;
pub use token_side::TokenSide;
use zama_host::CoprocessorInputAttestation;

pub trait InputExt {
    fn validate(&self, user: Pubkey, contract_address: Pubkey) -> Result<()>;
}
impl InputExt for CoprocessorInputAttestation {
    fn validate(&self, user: Pubkey, contract_address: Pubkey) -> Result<()> {
        require_keys_eq!(
            user,
            Pubkey::new_from_array(self.user_address),
            ConfidentialRfqError::InvalidRfqAccounts
        );
        require_keys_eq!(
            contract_address,
            Pubkey::new_from_array(self.contract_address),
            ConfidentialRfqError::InvalidRfqAccounts
        );
        Ok(())
    }
}

pub trait Contains<'a, T> {
    fn get(&'a self) -> T;
}
#[derive(Copy, Clone, Debug, Deref)]
pub struct ZamaEventAuthority<'a, 'info>(pub &'a UncheckedAccount<'info>);
#[derive(Copy, Clone, Debug, Deref)]
pub struct ConfidentialTokenEventAuthority<'a, 'info>(pub &'a UncheckedAccount<'info>);
#[derive(Copy, Clone, Debug, Deref)]
pub struct TransientStore<'a, 'info>(pub &'a UncheckedAccount<'info>);
#[derive(Copy, Clone, Debug, Deref)]
pub struct InstructionsAccount<'a, 'info>(pub &'a UncheckedAccount<'info>);
#[derive(Copy, Clone, Debug, Deref)]
pub struct HostConfig<'a, 'info>(pub &'a UncheckedAccount<'info>);
