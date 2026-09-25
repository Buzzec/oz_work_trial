//! Shared PDA seeds for account constraints, address derivation, and CPI signing.

use anchor_lang::prelude::*;
use bytemuck::bytes_of;

pub fn market_maker_group_seeds(market: &Pubkey) -> [&[u8]; 2] {
    [b"market_maker_group", market.as_ref()]
}

pub fn market_maker_group_signer_seeds<'a>(market: &'a Pubkey, bump: &'a u8) -> [&'a [u8]; 3] {
    let [prefix, market] = market_maker_group_seeds(market);
    [prefix, market, bytes_of(bump)]
}

pub fn market_maker_group_address(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&market_maker_group_seeds(market), &crate::ID)
}

pub fn rfq_seeds<'a>(market: &'a Pubkey, user: &'a Pubkey, nonce: &'a [u8; 32]) -> [&'a [u8]; 4] {
    [b"rfq", market.as_ref(), user.as_ref(), nonce]
}

pub fn rfq_address(market: &Pubkey, user: &Pubkey, nonce: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&rfq_seeds(market, user, nonce), &crate::ID)
}

pub fn rfq_authority_seeds<'a>(rfq: &'a Pubkey, nonce: &'a [u8; 32]) -> [&'a [u8]; 3] {
    [b"rfq_authority", rfq.as_ref(), nonce]
}

pub fn rfq_authority_signer_seeds<'a>(
    rfq: &'a Pubkey,
    nonce: &'a [u8; 32],
    bump: &'a [u8; 1],
) -> [&'a [u8]; 4] {
    let [prefix, rfq, nonce] = rfq_authority_seeds(rfq, nonce);
    [prefix, rfq, nonce, bump]
}

pub fn rfq_authority_address(rfq: &Pubkey, nonce: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&rfq_authority_seeds(rfq, nonce), &crate::ID)
}

pub fn bid_receipt_seeds<'a>(rfq: &'a Pubkey, bid_index: &'a [u8; 8]) -> [&'a [u8]; 3] {
    [b"bid_receipt", rfq.as_ref(), bid_index]
}

pub fn bid_receipt_address(rfq: &Pubkey, bid_index: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &bid_receipt_seeds(rfq, &bid_index.to_le_bytes()),
        &crate::ID,
    )
}
