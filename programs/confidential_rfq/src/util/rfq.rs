//! Bind an existing RFQ to the encrypted store holding its original input nonce.

use crate::{
    ConfidentialRfqError, CurrentAccountVersion, state::rfq::RFQ, util::pda::rfq_state_signer_seeds,
};
use anchor_lang::prelude::*;
use std::cell::Ref;
use zama_host::EncryptedStore;

pub fn validate_rfq_store(
    rfq_key: Pubkey,
    rfq: &RFQ,
    store_key: Pubkey,
    store: &EncryptedStore,
) -> Result<()> {
    require!(
        rfq.version == RFQ::VERSION && store.scope == rfq.nonce,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    // These bumps were recorded by canonical initialization in the owning programs.
    // Revalidate the address directly without repeating the bump search on every CPI phase.
    let expected_rfq =
        Pubkey::create_program_address(&rfq_state_signer_seeds(rfq, &store.scope), &crate::ID)
            .map_err(|_| error!(ConfidentialRfqError::InvalidRfqAccounts))?;
    require!(
        rfq_key == expected_rfq,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let expected_store = Pubkey::create_program_address(
        &[
            zama_host::ENCRYPTED_STORE_SEED,
            crate::ID.as_ref(),
            rfq_key.as_ref(),
            &store.scope,
            &[store.bump],
        ],
        &zama_host::ID,
    )
    .map_err(|_| error!(ConfidentialRfqError::RfqStoreMismatch))?;
    require!(
        store_key == expected_store && store.program == crate::ID && store.authority == rfq_key,
        ConfidentialRfqError::RfqStoreMismatch
    );
    Ok(())
}

/// Verify a maker store's canonical host address and RFQ-bound authority.
pub fn validate_maker_store(
    rfq_key: Pubkey,
    rfq: &RFQ,
    maker_id: u32,
    store_key: Pubkey,
    store: &EncryptedStore,
) -> Result<()> {
    require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
    let authority = crate::util::pda::maker_store_address(rfq_key, maker_id).0;
    let (expected, bump) = zama_host::encrypted_store_address(crate::ID, authority, rfq.nonce);
    require!(
        store_key == expected
            && store.program == crate::ID
            && store.authority == authority
            && store.scope == rfq.nonce
            && store.bump == bump,
        ConfidentialRfqError::RfqStoreMismatch
    );
    Ok(())
}

/// Deserialize a canonical host-owned store after CPI mutations.
pub fn read_encrypted_store(info: &AccountInfo) -> Result<EncryptedStore> {
    require_keys_eq!(
        *info.owner,
        zama_host::ID,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let state = EncryptedStore::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    // Host ownership and deserialization above make its stored canonical bump trustworthy.
    let address = Pubkey::create_program_address(
        &[
            zama_host::ENCRYPTED_STORE_SEED,
            state.program.as_ref(),
            state.authority.as_ref(),
            &state.scope,
            &[state.bump],
        ],
        &zama_host::ID,
    )
    .map_err(|_| error!(ConfidentialRfqError::InvalidRfqAccounts))?;
    require!(
        info.key() == address,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    Ok(state)
}

/// Borrow the canonical host journal without copying its history onto the stack or heap.
/// Callers must release this read-only borrow before CPI and bind the results they consume
/// to the expected producer, append range, and consumer grants after acquiring a fresh one.
pub fn read_journal<'a>(
    account: &'a AccountInfo<'_>,
) -> Result<Ref<'a, zama_host::TransientStore>> {
    require_keys_eq!(
        *account.owner,
        zama_host::ID,
        ConfidentialRfqError::InvalidTransferResult
    );
    let data = account.try_borrow_data()?;
    let discriminator = <zama_host::TransientStore as anchor_lang::Discriminator>::DISCRIMINATOR;
    require!(
        data.len() == zama_host::TransientStore::SPACE && data.starts_with(discriminator),
        ConfidentialRfqError::InvalidTransferResult
    );
    let state = Ref::filter_map(data, |data| {
        bytemuck::try_from_bytes::<zama_host::TransientStore>(&data[discriminator.len()..]).ok()
    })
    .map_err(|_| error!(ConfidentialRfqError::InvalidTransferResult))?;
    state.validate(*account.key)?;
    Ok(state)
}
