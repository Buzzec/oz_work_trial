//! Reusable Zama and confidential-token CPI calls shared by RFQ instructions.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{ExecutionCpiAccounts, FheExecution, FheTyped, ReturningFheExecution};
use zama_host::CoprocessorInputAttestation;

use crate::ConfidentialRfqError;
use crate::instructions::claim_maker::ClaimRfqMaker;
use crate::instructions::claim_user::ClaimRfqUser;

pub fn invoke<'info>(
    execution: FheExecution,
    accounts: ExecutionCpiAccounts<'info>,
    dynamic: impl IntoIterator<Item = AccountInfo<'info>>,
    authorities: impl IntoIterator<Item = AccountInfo<'info>>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let resolved = execution
        .resolve_accounts(dynamic, authorities)
        .map_err(|error| {
            msg!("invalid RFQ FHE CPI accounts: {:?}", error);
            error!(ConfidentialRfqError::InvalidFheExecution)
        })?;
    execution.invoke(accounts, &resolved, signer_seeds)
}

pub fn initialize_token_account<'info>(
    program: Pubkey,
    accounts: ct::cpi::accounts::InitializeTokenAccount<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    ct::cpi::initialize_token_account(CpiContext::new_with_signer(program, accounts, signer_seeds))
}

pub fn delegate_for_user_decryption<'info>(
    program: Pubkey,
    accounts: zama_host::cpi::accounts::DelegateForUserDecryption<'info>,
    signer_seeds: &[&[&[u8]]],
    delegate: Pubkey,
    authority: Pubkey,
    expiration_slot: u64,
) -> Result<()> {
    zama_host::cpi::delegate_for_user_decryption(
        CpiContext::new_with_signer(program, accounts, signer_seeds),
        delegate,
        authority,
        expiration_slot,
    )
}

pub fn revoke_delegation_for_user_decryption<'info>(
    program: Pubkey,
    accounts: zama_host::cpi::accounts::RevokeDelegationForUserDecryption<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    zama_host::cpi::revoke_delegation_for_user_decryption(CpiContext::new_with_signer(
        program,
        accounts,
        signer_seeds,
    ))
}

pub fn invoke_returning<'info, T: FheTyped>(
    execution: ReturningFheExecution<T>,
    accounts: ExecutionCpiAccounts<'info>,
    dynamic: impl IntoIterator<Item = AccountInfo<'info>>,
    authorities: impl IntoIterator<Item = AccountInfo<'info>>,
    signer_seeds: &[&[&[u8]]],
) -> Result<[u8; 32]> {
    let resolved = execution
        .execution()
        .resolve_accounts(dynamic, authorities)
        .map_err(|error| {
            msg!("invalid RFQ FHE CPI accounts: {:?}", error);
            error!(ConfidentialRfqError::InvalidFheExecution)
        })?;
    execution.invoke(accounts, &resolved, signer_seeds)
}

pub fn transfer_from_grant<'info>(
    program: Pubkey,
    accounts: ct::cpi::accounts::ConfidentialTransferFromValue<'info>,
    signer_seeds: &[&[&[u8]]],
    handle: [u8; 32],
) -> Result<()> {
    ct::cpi::confidential_transfer_from_value(
        CpiContext::new_with_signer(program, accounts, signer_seeds),
        ct::TransferInput::Grant { handle },
    )
}

#[derive(Clone, Copy)]
pub enum PayoutToken {
    Asset,
    Basis,
}

/// Spend a maker payout grant against the matching RFQ escrow token account.
#[inline(never)]
pub fn transfer_maker_payout<'info>(
    ctx: &Context<'info, ClaimRfqMaker<'info>>,
    authority_seeds: &[&[u8]],
    handle: [u8; 32],
    token: PayoutToken,
) -> Result<()> {
    let side = match token {
        PayoutToken::Asset => &ctx.accounts.asset,
        PayoutToken::Basis => &ctx.accounts.basis,
    };
    transfer_from_grant(
        ctx.accounts.confidential_token_program.key(),
        ct::cpi::accounts::ConfidentialTransferFromValue {
            owner: ctx.accounts.rfq_authority.to_account_info(),
            payer: ctx.accounts.maker.to_account_info(),
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
        handle,
    )
}

pub fn transfer_attested<'info>(
    program: Pubkey,
    accounts: ct::cpi::accounts::ConfidentialTransfer<'info>,
    attestation: CoprocessorInputAttestation,
) -> Result<[u8; 32]> {
    ct::cpi::confidential_transfer(CpiContext::new(program, accounts), attestation)?;
    let (producer, data) = anchor_lang::solana_program::program::get_return_data()
        .ok_or(ConfidentialRfqError::InvalidTransferResult)?;
    require_keys_eq!(
        producer,
        ct::ID,
        ConfidentialRfqError::InvalidTransferResult
    );
    data.try_into()
        .map_err(|_| error!(ConfidentialRfqError::InvalidTransferResult))
}

/// Spend both encrypted user payout grants from the RFQ escrow accounts.
#[inline(never)]
pub fn transfer_user_payouts<'info>(
    ctx: &Context<'info, ClaimRfqUser<'info>>,
    authority_seeds: &[&[u8]],
    asset_handle: [u8; 32],
    basis_handle: [u8; 32],
) -> Result<()> {
    for (side, handle) in [
        (&ctx.accounts.asset, asset_handle),
        (&ctx.accounts.basis, basis_handle),
    ] {
        transfer_from_grant(
            ctx.accounts.confidential_token_program.key(),
            ct::cpi::accounts::ConfidentialTransferFromValue {
                owner: ctx.accounts.rfq_authority.to_account_info(),
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
            handle,
        )?;
    }
    Ok(())
}
