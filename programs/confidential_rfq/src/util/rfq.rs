//! Bind an existing RFQ to the encrypted store holding its original input nonce.

use crate::{ConfidentialRfqError, state::rfq::RFQ, util::pda::rfq_seeds_from_keys};
use anchor_lang::prelude::*;
use zama_host::EncryptedStore;

pub fn validate_rfq_store(
    rfq_key: Pubkey,
    rfq: &RFQ,
    store_key: Pubkey,
    store: &EncryptedStore,
) -> Result<()> {
    let (expected_rfq, bump) = Pubkey::find_program_address(
        &rfq_seeds_from_keys(&rfq.market, &rfq.user, &store.scope),
        &crate::ID,
    );
    require!(
        rfq_key == expected_rfq && rfq.bump == bump,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let (expected_store, store_bump) =
        zama_host::encrypted_store_address(crate::ID, rfq_key, store.scope);
    require!(
        store_key == expected_store
            && store.program == crate::ID
            && store.authority == rfq_key
            && store.bump == store_bump,
        ConfidentialRfqError::RfqStoreMismatch
    );
    Ok(())
}
