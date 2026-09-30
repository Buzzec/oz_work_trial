//! Confidential request-for-quote market backed by Zama encrypted stores and tokens.

#![allow(unexpected_cfgs)]
#![allow(ambiguous_glob_reexports)]

pub mod errors;
pub mod instructions;
pub mod state;
pub mod util;

#[cfg(any(
    test,
    all(
        target_os = "solana",
        feature = "custom-heap",
        not(feature = "no-entrypoint")
    )
))]
mod allocator;

pub use errors::ConfidentialRfqError;
pub use instructions::*;
pub use state::CurrentAccountVersion;
pub use util::EncryptedInput;

use anchor_lang::prelude::*;

declare_id!("7Y7wXXw2GWp6v3FMSQYrMqbB6re5cs2E8i7HqgKwCXpp");

#[program]
pub mod confidential_rfq {
    use super::*;

    pub fn create_market(ctx: Context<CreateMarket>) -> Result<()> {
        instructions::create_market::create_market(ctx)
    }

    pub fn add_maker(ctx: Context<AddMaker>, maker_id: u32, maker: Pubkey) -> Result<()> {
        instructions::add_maker::add_maker(ctx, maker_id, maker)
    }

    pub fn remove_maker(ctx: Context<RemoveMaker>, maker_id: u32) -> Result<()> {
        instructions::remove_maker::remove_maker(ctx, maker_id)
    }

    pub fn request_quote<'info>(
        ctx: Context<'info, RequestQuote<'info>>,
        amounts: EncryptedInput,
        user_buyer: EncryptedInput,
        timeout: EncryptedInput,
        bid_capacity: u32,
    ) -> Result<()> {
        let user = ctx.accounts.user.key();
        instructions::request_quote::request_quote(
            ctx,
            Box::new(amounts.into_attestation(user, crate::ID)),
            Box::new(user_buyer.into_attestation(user, crate::ID)),
            Box::new(timeout.into_attestation(user, crate::ID)),
            bid_capacity,
        )
    }

    pub fn fund_quote<'info>(
        ctx: Context<'info, FundQuote<'info>>,
        asset_escrow: EncryptedInput,
        basis_escrow: EncryptedInput,
    ) -> Result<()> {
        let user = ctx.accounts.user.key();
        instructions::fund_quote::fund_quote(
            ctx,
            Box::new(asset_escrow.into_attestation(user, confidential_token::ID)),
            Box::new(basis_escrow.into_attestation(user, confidential_token::ID)),
        )
    }

    pub fn place_bid<'info>(
        ctx: Context<'info, PlaceBid<'info>>,
        maker_id: u32,
        prices: EncryptedInput,
        asset_transfer_attestation: EncryptedInput,
        basis_transfer_attestation: EncryptedInput,
    ) -> Result<()> {
        let maker = ctx.accounts.maker.key();
        instructions::place_bid::place_bid(
            ctx,
            maker_id,
            Box::new(prices.into_attestation(maker, crate::ID)),
            Box::new(asset_transfer_attestation.into_attestation(maker, confidential_token::ID)),
            Box::new(basis_transfer_attestation.into_attestation(maker, confidential_token::ID)),
        )
    }

    pub fn claim_rfq_user<'info>(ctx: Context<'info, ClaimRfqUser<'info>>) -> Result<()> {
        instructions::claim_user::claim_rfq_user(ctx)
    }

    pub fn claim_rfq_maker<'info>(
        ctx: Context<'info, ClaimRfqMaker<'info>>,
        maker_id: u32,
    ) -> Result<()> {
        instructions::claim_maker::claim_rfq_maker(ctx, maker_id)
    }

    pub fn cancel_quote<'info>(ctx: Context<'info, ClaimRfqUser<'info>>) -> Result<()> {
        instructions::cancel_quote::cancel_quote(ctx)
    }

    pub fn expire_rfq<'info>(
        ctx: Context<'info, ExpireRfq<'info>>,
        maker_id: Option<u32>,
    ) -> Result<()> {
        instructions::expire_rfq::expire_rfq(ctx, maker_id)
    }

    pub fn calculate_winner<'info>(
        ctx: Context<'info, CalculateWinner<'info>>,
        maker_id: u32,
    ) -> Result<()> {
        instructions::calculate_winner::calculate_winner(ctx, maker_id)
    }

    pub fn close_rfq<'info>(
        ctx: Context<'info, CloseRfq<'info>>,
        signatures: Vec<[u8; 65]>,
        extra_data: Vec<u8>,
        proof: zama_host::instructions::MmrInclusionProof,
    ) -> Result<()> {
        instructions::close_rfq::close_rfq(ctx, signatures, extra_data, proof)
    }
}
