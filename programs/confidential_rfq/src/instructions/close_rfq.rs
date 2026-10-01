//! Return the RFQ and funder rent after certified settlement, retaining host-owned stores.

use anchor_lang::{
    prelude::*,
    system_program::{self, Transfer},
};
use zama_host::{
    EncryptedStore, HostConfig, KmsContext, instructions::MmrInclusionProof, program::ZamaHost,
};

use crate::{
    ConfidentialRfqError,
    state::{
        CurrentAccountVersion,
        rfq::{RFQ, RFQPrivateField},
    },
    util::{close_rfq_cpi, pda::rfq_funder_signer_seeds, rfq::validate_rfq_store},
};

#[derive(Accounts)]
pub struct CloseRfq<'info> {
    /// Anyone may close the RFQ; all available rent goes to its recorded user.
    #[account(
        mut,
        close = user,
        has_one = user @ ConfidentialRfqError::InvalidUser,
        constraint = rfq.load()?.version == RFQ::VERSION @ ConfidentialRfqError::InvalidRfqVersion,
    )]
    pub rfq: AccountLoader<'info, RFQ>,
    /// CHECK: compared with `rfq.user`; receives the RFQ and funder rent.
    #[account(mut)]
    pub user: UncheckedAccount<'info>,
    /// System-owned, data-empty rent reserve, checked against the RFQ-bound PDA.
    #[account(
        mut,
        seeds = [b"rfq_funder", rfq.key().as_ref()],
        bump,
        constraint = rfq_funder.data_is_empty() @ ConfidentialRfqError::InvalidRfqAccounts,
        constraint = rfq.load()?.funder_bump == crate::util::pda::rfq_funder_address(rfq.key()).1
            @ ConfidentialRfqError::InvalidRfqAccounts,
    )]
    pub rfq_funder: SystemAccount<'info>,
    /// Retained primary store containing the current public closure predicate.
    #[account(constraint = {
        validate_rfq_store(rfq.key(), &*rfq.load()?, rfq_store.key(), &rfq_store)?;
        true
    })]
    pub rfq_store: Box<Account<'info, EncryptedStore>>,
    pub host_config: Box<Account<'info, HostConfig>>,
    pub kms_context: Box<Account<'info, KmsContext>>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
}

/// Verify that the current encrypted state is terminal and has zero active bids,
/// then refund the RFQ and funder. Host-owned stores remain because the checked-in
/// host has no authority-controlled closure API. The RFQ is the act-once guard.
pub fn close_rfq<'info>(
    ctx: Context<'info, CloseRfq<'info>>,
    signatures: Vec<[u8; 65]>,
    extra_data: Vec<u8>,
    proof: MmrInclusionProof,
) -> Result<()> {
    let rfq_key = ctx.accounts.rfq.key();
    require!(
        ctx.remaining_accounts.is_empty(),
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let funder_bump = ctx.bumps.rfq_funder;

    // Certify the current slot handle, never a historical true value. open_stores
    // continues to describe retained storage and does not gate account closure.
    let handle = ctx
        .accounts
        .rfq_store
        .get(&RFQPrivateField::CanClose.key())
        .ok_or(error!(ConfidentialRfqError::CanCloseMissing))?;
    close_rfq_cpi::verify_can_close(
        ctx.accounts.zama_program.key(),
        zama_host::cpi::accounts::VerifyPublicDecrypt {
            host_config: ctx.accounts.host_config.to_account_info(),
            kms_context: ctx.accounts.kms_context.to_account_info(),
            encrypted_store: ctx.accounts.rfq_store.to_account_info(),
        },
        handle,
        signatures,
        extra_data,
        proof,
    )?;

    // The funder signs a System transfer; Anchor closes the RFQ after success.
    // No rent from encrypted stores is reclaimed or promised by this instruction.
    let lamports = ctx.accounts.rfq_funder.lamports();
    if lamports != 0 {
        let seeds = rfq_funder_signer_seeds(&rfq_key, &funder_bump);
        system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.key(),
                Transfer {
                    from: ctx.accounts.rfq_funder.to_account_info(),
                    to: ctx.accounts.user.to_account_info(),
                },
                &[&seeds],
            ),
            lamports,
        )?;
    }
    Ok(())
}
