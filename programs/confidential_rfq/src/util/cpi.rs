//! Reusable Zama and confidential-token CPI calls shared by RFQ instructions.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{ExecutionCpiAccounts, FheExecution, FheTyped, ReturningFheExecution};
use zama_host::CoprocessorInputAttestation;

use crate::ConfidentialRfqError;

pub fn invoke<'info>(
    execution: FheExecution,
    accounts: ExecutionCpiAccounts<'info>,
    dynamic: impl IntoIterator<Item = AccountInfo<'info>>,
    authorities: impl IntoIterator<Item = AccountInfo<'info>>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let dynamic = dynamic.into_iter().filter(|account| {
        execution.dynamic_account_requirements().any(|required| {
            required.requires_dynamic_account() && required.pubkey() == account.key()
        })
    });
    let authorities = authorities.into_iter().filter(|account| {
        execution
            .store_authority_requirements()
            .any(|required| required.pubkey() == account.key())
    });
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
    let dynamic = dynamic.into_iter().filter(|account| {
        execution
            .execution()
            .dynamic_account_requirements()
            .any(|required| {
                required.requires_dynamic_account() && required.pubkey() == account.key()
            })
    });
    let authorities = authorities.into_iter().filter(|account| {
        execution
            .execution()
            .store_authority_requirements()
            .any(|required| required.pubkey() == account.key())
    });
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
