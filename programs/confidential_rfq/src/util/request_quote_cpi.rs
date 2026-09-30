//! CPI account assembly for opening and funding a quote.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{FheExecution, StoreId};
use zama_host::CoprocessorInputAttestation;

use crate::{
    ConfidentialRfqError,
    instructions::{fund_quote::FundQuote, request_quote::RequestQuote},
    util::rfq::read_journal,
};

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

/// Execute the combined validation/refund circuit and retrieve its final two results.
/// Host ownership, canonical journal identity, exact append length, producer, and both
/// consumer grants are checked before either returned handle can authorize a transfer.
#[inline(never)]
pub fn execute_refund_preparation<'info>(
    ctx: &Context<'info, FundQuote<'info>>,
    execution: FheExecution,
    targets: [StoreId; 2],
    authority_seeds: &[&[u8]],
) -> Result<[[u8; 32]; 2]> {
    let before = {
        let state = read_journal(ctx.accounts.transient_store.as_ref())?;
        require_keys_eq!(
            state.payer,
            ctx.accounts.user.key(),
            ConfidentialRfqError::InvalidTransferResult
        );
        state.len()
    };
    let steps = execution.cost().steps;
    require!(steps >= 2, ConfidentialRfqError::InvalidTransferResult);
    require!(
        targets[0].address() == ctx.accounts.asset.rfq_balance_store.key()
            && targets[1].address() == ctx.accounts.basis.rfq_balance_store.key(),
        ConfidentialRfqError::InvalidTransferResult
    );
    let expected_len = before
        .checked_add(steps)
        .ok_or(error!(ConfidentialRfqError::InvalidTransferResult))?;
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
        [
            ctx.accounts.rfq_store.to_account_info(),
            ctx.accounts.asset.rfq_balance_store.to_account_info(),
            ctx.accounts.basis.rfq_balance_store.to_account_info(),
        ],
        [ctx.accounts.rfq.to_account_info()],
        &[authority_seeds],
    )?;
    let state = read_journal(ctx.accounts.transient_store.as_ref())?;
    require_keys_eq!(
        state.payer,
        ctx.accounts.user.key(),
        ConfidentialRfqError::InvalidTransferResult
    );
    require!(
        state.len() == expected_len,
        ConfidentialRfqError::InvalidTransferResult
    );
    let mut handles = [[0; 32]; 2];
    for (index, target) in targets.iter().enumerate() {
        let result = state
            .result(expected_len - 2 + index)
            .ok_or(error!(ConfidentialRfqError::InvalidTransferResult))?;
        require!(
            result.producer_store == ctx.accounts.rfq_store.key()
                && state
                    .authorized_depth(result.handle, target.address())
                    .is_some(),
            ConfidentialRfqError::InvalidTransferResult
        );
        handles[index] = result.handle;
    }
    Ok(handles)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetOrBasis {
    Asset,
    Basis,
}

/// Prepare each canonical zero-balance escrow once, accepting permissionless prior creation.
#[inline(never)]
pub fn initialize_escrow<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    asset_or_basis: AssetOrBasis,
) -> Result<()> {
    let side = match asset_or_basis {
        AssetOrBasis::Asset => &ctx.accounts.asset,
        AssetOrBasis::Basis => &ctx.accounts.basis,
    };
    if *side.rfq_token_account.owner == ct::ID {
        return side.validate_prepared_escrow(ctx.accounts.rfq.key());
    }
    require!(
        *side.rfq_token_account.owner == System::id()
            && side.rfq_token_account.data_is_empty()
            && *side.rfq_balance_store.owner == System::id()
            && side.rfq_balance_store.data_is_empty(),
        ConfidentialRfqError::InvalidRfqAccounts
    );
    ct::cpi::initialize_token_account(
        CpiContext::new(
            ctx.accounts.confidential_token_program.key(),
            ct::cpi::accounts::InitializeTokenAccount {
                payer: ctx.accounts.user.to_account_info(),
                owner: ctx.accounts.rfq.to_account_info(),
                mint: side.confidential_mint.to_account_info(),
                token_account: side.rfq_token_account.to_account_info(),
                balance_encrypted_store: side.rfq_balance_store.to_account_info(),
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
        )
        .with_remaining_accounts(ctx.remaining_accounts.to_vec()),
    )
}

#[inline(never)]
pub fn transfer_to_escrow<'info>(
    ctx: &Context<'info, FundQuote<'info>>,
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
    ctx: &Context<'info, FundQuote<'info>>,
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
