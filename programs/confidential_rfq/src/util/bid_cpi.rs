//! CPI plumbing for one public PlaceBid operation. The FHE price, funding,
//! and winner calculations remain in the instruction module.

use crate::instructions::place_bid::PlaceBid;
use crate::util::cpi;
use crate::util::pda::rfq_authority_signer_seeds;
use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{ExecutionCpiAccounts, FheExecution, Uint};
use zama_host::CoprocessorInputAttestation;

#[inline(never)]
pub(crate) fn invoke_bid_execution<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    execution: FheExecution,
    rfq_key: Pubkey,
    nonce: [u8; 32],
    authority_bump: u8,
) -> Result<()> {
    let bump = [authority_bump];
    let signer_seeds = &rfq_authority_signer_seeds(&rfq_key, &nonce, &bump);
    cpi::invoke(
        execution,
        ExecutionCpiAccounts {
            payer: ctx.accounts.maker.to_account_info(),
            authority: ctx.accounts.rfq_authority.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            deny_scope_records: ctx.remaining_accounts.to_vec(),
            system_program: ctx.accounts.system_program.to_account_info(),
            hcu_block_meter: ctx
                .accounts
                .hcu_block_meter
                .as_ref()
                .map(ToAccountInfo::to_account_info),
            hcu_trusted_app_record: ctx
                .accounts
                .hcu_trusted_app_record
                .as_ref()
                .map(ToAccountInfo::to_account_info),
            rand_nonce: None,
            event_authority: ctx.accounts.zama_event_authority.to_account_info(),
            transient_store: ctx.accounts.transient_store.to_account_info(),
            instructions: ctx.accounts.instructions.to_account_info(),
            program: ctx.accounts.zama_program.to_account_info(),
        },
        [ctx.accounts.rfq_store.to_account_info()],
        [ctx.accounts.rfq_authority.to_account_info()],
        &[signer_seeds],
    )
}

#[inline(never)]
pub(crate) fn invoke_returning_bid_execution<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    execution: zama_fhe::ReturningFheExecution<Uint<64>>,
    rfq_key: Pubkey,
    nonce: [u8; 32],
    authority_bump: u8,
) -> Result<[u8; 32]> {
    let mut dynamic = vec![ctx.accounts.rfq_store.to_account_info()];
    for required in execution.execution().dynamic_account_requirements() {
        if !required.requires_dynamic_account() || required.pubkey() == ctx.accounts.rfq_store.key()
        {
            continue;
        }
        if required.pubkey() == ctx.accounts.rfq_asset_balance_store.key() {
            dynamic.push(ctx.accounts.rfq_asset_balance_store.to_account_info());
        } else if required.pubkey() == ctx.accounts.rfq_basis_balance_store.key() {
            dynamic.push(ctx.accounts.rfq_basis_balance_store.to_account_info());
        }
    }
    let bump = [authority_bump];
    let signer_seeds = &rfq_authority_signer_seeds(&rfq_key, &nonce, &bump);
    cpi::invoke_returning(
        execution,
        ExecutionCpiAccounts {
            payer: ctx.accounts.maker.to_account_info(),
            authority: ctx.accounts.rfq_authority.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            deny_scope_records: ctx.remaining_accounts.to_vec(),
            system_program: ctx.accounts.system_program.to_account_info(),
            hcu_block_meter: ctx
                .accounts
                .hcu_block_meter
                .as_ref()
                .map(ToAccountInfo::to_account_info),
            hcu_trusted_app_record: ctx
                .accounts
                .hcu_trusted_app_record
                .as_ref()
                .map(ToAccountInfo::to_account_info),
            rand_nonce: None,
            event_authority: ctx.accounts.zama_event_authority.to_account_info(),
            transient_store: ctx.accounts.transient_store.to_account_info(),
            instructions: ctx.accounts.instructions.to_account_info(),
            program: ctx.accounts.zama_program.to_account_info(),
        },
        dynamic,
        [ctx.accounts.rfq_authority.to_account_info()],
        &[signer_seeds],
    )
}

pub(crate) fn transfer_token<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    attestation: CoprocessorInputAttestation,
    asset: bool,
) -> Result<[u8; 32]> {
    let (mint, underlying, maker_ata, rfq_ata, maker_token, rfq_token, maker_store, rfq_balance) =
        if asset {
            (
                &ctx.accounts.asset_confidential_mint,
                &ctx.accounts.asset_underlying_mint,
                &ctx.accounts.maker_asset_ata,
                &ctx.accounts.rfq_asset_ata,
                &ctx.accounts.maker_asset_token_account,
                &ctx.accounts.rfq_asset_token_account,
                &ctx.accounts.maker_asset_balance_store,
                &ctx.accounts.rfq_asset_balance_store,
            )
        } else {
            (
                &ctx.accounts.basis_confidential_mint,
                &ctx.accounts.basis_underlying_mint,
                &ctx.accounts.maker_basis_ata,
                &ctx.accounts.rfq_basis_ata,
                &ctx.accounts.maker_basis_token_account,
                &ctx.accounts.rfq_basis_token_account,
                &ctx.accounts.maker_basis_balance_store,
                &ctx.accounts.rfq_basis_balance_store,
            )
        };
    cpi::transfer_attested(
        ctx.accounts.confidential_token_program.key(),
        ct::cpi::accounts::ConfidentialTransfer {
            owner: ctx.accounts.maker.to_account_info(),
            payer: ctx.accounts.maker.to_account_info(),
            mint: mint.to_account_info(),
            underlying_mint: underlying.to_account_info(),
            from_ata: maker_ata.to_account_info(),
            to_ata: rfq_ata.to_account_info(),
            from_account: maker_token.to_account_info(),
            to_account: rfq_token.to_account_info(),
            from_store: maker_store.to_account_info(),
            to_store: rfq_balance.to_account_info(),
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
        attestation,
    )
}

#[inline(never)]
pub(crate) fn refund_token<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    handle: [u8; 32],
    asset: bool,
    rfq_key: Pubkey,
    nonce: [u8; 32],
    authority_bump: u8,
) -> Result<()> {
    let (mint, underlying, maker_ata, rfq_ata, maker_token, rfq_token, maker_store, rfq_balance) =
        if asset {
            (
                &ctx.accounts.asset_confidential_mint,
                &ctx.accounts.asset_underlying_mint,
                &ctx.accounts.maker_asset_ata,
                &ctx.accounts.rfq_asset_ata,
                &ctx.accounts.maker_asset_token_account,
                &ctx.accounts.rfq_asset_token_account,
                &ctx.accounts.maker_asset_balance_store,
                &ctx.accounts.rfq_asset_balance_store,
            )
        } else {
            (
                &ctx.accounts.basis_confidential_mint,
                &ctx.accounts.basis_underlying_mint,
                &ctx.accounts.maker_basis_ata,
                &ctx.accounts.rfq_basis_ata,
                &ctx.accounts.maker_basis_token_account,
                &ctx.accounts.rfq_basis_token_account,
                &ctx.accounts.maker_basis_balance_store,
                &ctx.accounts.rfq_basis_balance_store,
            )
        };
    let bump = [authority_bump];
    let authority_seeds = &rfq_authority_signer_seeds(&rfq_key, &nonce, &bump);
    cpi::transfer_from_grant(
        ctx.accounts.confidential_token_program.key(),
        ct::cpi::accounts::ConfidentialTransferFromValue {
            owner: ctx.accounts.rfq_authority.to_account_info(),
            payer: ctx.accounts.maker.to_account_info(),
            mint: mint.to_account_info(),
            underlying_mint: underlying.to_account_info(),
            from_ata: rfq_ata.to_account_info(),
            to_ata: maker_ata.to_account_info(),
            from_account: rfq_token.to_account_info(),
            to_account: maker_token.to_account_info(),
            from_store: rfq_balance.to_account_info(),
            to_store: maker_store.to_account_info(),
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
        handle,
    )
}
