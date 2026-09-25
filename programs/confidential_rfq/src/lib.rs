//! Confidential request-for-quote market backed by Zama encrypted stores and tokens.

#![allow(unexpected_cfgs)]
#![allow(ambiguous_glob_reexports)]

pub mod errors;
pub mod instructions;
pub mod state;
pub mod util;

pub use errors::ConfidentialRfqError;
pub use instructions::*;
pub use state::CurrentAccountVersion;

use anchor_lang::prelude::*;

declare_id!("7Y7wXXw2GWp6v3FMSQYrMqbB6re5cs2E8i7HqgKwCXpp");

#[program]
pub mod confidential_rfq {
    use super::*;

    pub fn create_market(ctx: Context<CreateMarket>) -> Result<()> {
        instructions::create_market::create_market(ctx)
    }

    pub fn add_maker(ctx: Context<AddMaker>, maker_id: u64, maker: Pubkey) -> Result<()> {
        instructions::add_maker::add_maker(ctx, maker_id, maker)
    }

    pub fn remove_maker(ctx: Context<RemoveMaker>, maker_id: u64) -> Result<()> {
        instructions::remove_maker::remove_maker(ctx, maker_id)
    }

    pub fn request_quote<'info>(
        ctx: Context<'info, RequestQuote<'info>>,
        amounts: zama_host::CoprocessorInputAttestation,
        user_buyer: zama_host::CoprocessorInputAttestation,
        timeout: i64,
        asset_escrow: zama_host::CoprocessorInputAttestation,
        basis_escrow: zama_host::CoprocessorInputAttestation,
    ) -> Result<()> {
        instructions::request_quote::request_quote(
            ctx,
            amounts,
            user_buyer,
            timeout,
            asset_escrow,
            basis_escrow,
        )
    }

    pub fn place_bid<'info>(
        ctx: Context<'info, PlaceBid<'info>>,
        maker_id: u64,
        prices: zama_host::CoprocessorInputAttestation,
        asset_transfer_attestation: zama_host::CoprocessorInputAttestation,
        basis_transfer_attestation: zama_host::CoprocessorInputAttestation,
    ) -> Result<()> {
        instructions::place_bid::place_bid(
            ctx,
            maker_id,
            prices,
            asset_transfer_attestation,
            basis_transfer_attestation,
        )
    }

    pub fn claim_rfq_user<'info>(ctx: Context<'info, ClaimRfqUser<'info>>) -> Result<()> {
        instructions::claim_user::claim_rfq_user(ctx)
    }

    pub fn claim_rfq_maker<'info>(
        ctx: Context<'info, ClaimRfqMaker<'info>>,
        maker_id: u64,
        bid_index: u64,
    ) -> Result<()> {
        instructions::claim_maker::claim_rfq_maker(ctx, maker_id, bid_index)
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
