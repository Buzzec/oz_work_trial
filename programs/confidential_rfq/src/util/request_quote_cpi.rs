//! CPI account assembly for opening and funding a quote.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::FheExecution;
use zama_host::CoprocessorInputAttestation;

use crate::instructions::request_quote::RequestQuote;

#[inline(never)]
pub fn execute_initialization<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    execution: FheExecution,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    crate::util::cpi::invoke(
        execution,
        zama_fhe::ExecutionCpiAccounts {
            payer: ctx.accounts.user.to_account_info(),
            authority: ctx.accounts.rfq.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            deny_scope_records: ctx.remaining_accounts.to_vec(),
            system_program: ctx.accounts.system_program.to_account_info(),
            hcu_block_meter: None,
            hcu_trusted_app_record: None,
            rand_nonce: None,
            event_authority: ctx.accounts.zama_event_authority.to_account_info(),
            transient_store: ctx.accounts.transient_store.to_account_info(),
            instructions: ctx.accounts.instructions.to_account_info(),
            program: ctx.accounts.zama_program.to_account_info(),
        },
        [ctx.accounts.rfq_store.to_account_info()],
        [ctx.accounts.rfq.to_account_info()],
        &[authority_seeds],
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetOrBasis {
    Asset,
    Basis,
}

#[inline(never)]
pub fn transfer_to_escrow<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    amount: CoprocessorInputAttestation,
    asset_or_basis: AssetOrBasis,
) -> Result<[u8; 32]> {
    let side = match asset_or_basis {
        AssetOrBasis::Asset => &ctx.accounts.asset,
        AssetOrBasis::Basis => &ctx.accounts.basis,
    };
    crate::util::cpi::transfer_attested(
        ctx.accounts.confidential_token_program.key(),
        ct::cpi::accounts::ConfidentialTransfer {
            owner: ctx.accounts.user.to_account_info(),
            payer: ctx.accounts.user.to_account_info(),
            mint: side.confidential_mint.to_account_info(),
            underlying_mint: side.underlying_mint.to_account_info(),
            from_ata: side.participant_ata.to_account_info(),
            to_ata: side.rfq_ata.to_account_info(),
            from_account: side.participant_token_account.to_account_info(),
            to_account: side.rfq_token_account.to_account_info(),
            from_store: side.participant_balance_store.to_account_info(),
            to_store: side.rfq_balance_store.to_account_info(),
            zama_event_authority: ctx.accounts.zama_event_authority.to_account_info(),
            transient_store: ctx.accounts.transient_store.to_account_info(),
            instructions: ctx.accounts.instructions.to_account_info(),
            zama_program: ctx.accounts.zama_program.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
            hcu_block_meter: None,
            hcu_trusted_app_record: None,
            result_store: Some(ctx.accounts.rfq_store.to_account_info()),
            event_authority: ctx
                .accounts
                .confidential_token_event_authority
                .to_account_info(),
            program: ctx.accounts.confidential_token_program.to_account_info(),
        },
        amount,
    )
}

#[inline(never)]
pub fn refund_to_user<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    amount_handle: [u8; 32],
    asset: bool,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let side = if asset {
        &ctx.accounts.asset
    } else {
        &ctx.accounts.basis
    };
    crate::util::cpi::transfer_from_grant(
        ctx.accounts.confidential_token_program.key(),
        ct::cpi::accounts::ConfidentialTransferFromValue {
            owner: ctx.accounts.rfq.to_account_info(),
            payer: ctx.accounts.user.to_account_info(),
            mint: side.confidential_mint.to_account_info(),
            underlying_mint: side.underlying_mint.to_account_info(),
            from_ata: side.rfq_ata.to_account_info(),
            to_ata: side.participant_ata.to_account_info(),
            from_account: side.rfq_token_account.to_account_info(),
            to_account: side.participant_token_account.to_account_info(),
            from_store: side.rfq_balance_store.to_account_info(),
            to_store: side.participant_balance_store.to_account_info(),
            amount_store: None,
            amount_authority: None,
            zama_event_authority: ctx.accounts.zama_event_authority.to_account_info(),
            transient_store: ctx.accounts.transient_store.to_account_info(),
            instructions: ctx.accounts.instructions.to_account_info(),
            zama_program: ctx.accounts.zama_program.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
            hcu_block_meter: None,
            hcu_trusted_app_record: None,
            event_authority: ctx
                .accounts
                .confidential_token_event_authority
                .to_account_info(),
            program: ctx.accounts.confidential_token_program.to_account_info(),
        },
        &[authority_seeds],
        amount_handle,
    )
}
