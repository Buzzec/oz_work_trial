//! Shared PDA seeds for account constraints, address derivation, and CPI signing.

use crate::state::{market::Market, rfq::RFQ};
use anchor_lang::prelude::*;
use bytemuck::bytes_of;

pub fn market_maker_group_seeds<'a>(market: &'a Account<Market>) -> [&'a [u8]; 2] {
    [
        b"market_maker_group",
        <Account<_> as AsRef<AccountInfo>>::as_ref(market)
            .key
            .as_ref(),
    ]
}

pub fn market_maker_group_signer_seeds<'a>(market: &'a Account<Market>) -> [&'a [u8]; 3] {
    let [prefix, market_key] = market_maker_group_seeds(market);
    [prefix, market_key, bytes_of(&market.maker_group_bump)]
}

pub fn market_maker_group_address(market: &Account<Market>) -> (Pubkey, u8) {
    Pubkey::find_program_address(&market_maker_group_seeds(market), &crate::ID)
}

pub fn rfq_seeds<'a>(
    market: &'a Account<Market>,
    user: &'a Pubkey,
    nonce: &'a [u8; 32],
) -> [&'a [u8]; 4] {
    rfq_seeds_from_keys(
        <Account<_> as AsRef<AccountInfo>>::as_ref(market).key,
        user,
        nonce,
    )
}

pub fn rfq_seeds_from_keys<'a>(
    market: &'a Pubkey,
    user: &'a Pubkey,
    nonce: &'a [u8; 32],
) -> [&'a [u8]; 4] {
    [b"rfq", market.as_ref(), user.as_ref(), nonce]
}

pub fn rfq_address(market: &Account<Market>, user: &Pubkey, nonce: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&rfq_seeds(market, user, nonce), &crate::ID)
}

pub fn rfq_signer_seeds<'a>(
    market: &'a Account<Market>,
    user: &'a Pubkey,
    nonce: &'a [u8; 32],
    bump: &'a u8,
) -> [&'a [u8]; 5] {
    let [prefix, rfq, user, nonce] = rfq_seeds(market, user, nonce);
    [prefix, rfq, user, nonce, bytes_of(bump)]
}

/// Recover an existing RFQ's signer seeds using its encrypted store's scope.
pub fn rfq_state_signer_seeds<'a>(rfq: &'a RFQ, scope: &'a [u8; 32]) -> [&'a [u8]; 5] {
    let [prefix, market, user, nonce] = rfq_seeds_from_keys(&rfq.market, &rfq.user, scope);
    [prefix, market, user, nonce, bytes_of(&rfq.bump)]
}

/// System-owned, data-empty account used to pay storage rent for this RFQ.
pub fn rfq_funder_address(rfq: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"rfq_funder", rfq.as_ref()], &crate::ID)
}

pub fn rfq_funder_signer_seeds<'a>(rfq: &'a Pubkey, bump: &'a u8) -> [&'a [u8]; 3] {
    [b"rfq_funder", rfq.as_ref(), std::slice::from_ref(bump)]
}

/// Authority of a maker's separate encrypted store, scoped to this RFQ's nonce.
pub fn maker_store_address(rfq: Pubkey, maker_id: u32) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"rfq_maker_store", rfq.as_ref(), &maker_id.to_le_bytes()],
        &crate::ID,
    )
}

pub fn maker_store_signer_seeds<'a>(
    rfq: &'a Pubkey,
    maker_id: &'a [u8; 4],
    bump: &'a u8,
) -> [&'a [u8]; 4] {
    [
        b"rfq_maker_store",
        rfq.as_ref(),
        maker_id,
        std::slice::from_ref(bump),
    ]
}
