//! The accounts for one confidential token side of an RFQ.

use crate::ConfidentialRfqError;
use anchor_lang::prelude::*;
use confidential_token as ct;

#[derive(Accounts)]
pub struct TokenSide<'info> {
    pub confidential_mint: Box<Account<'info, ct::ConfidentialMint>>,
    /// CHECK: the confidential-token CPI verifies the underlying mint.
    pub underlying_mint: UncheckedAccount<'info>,
    /// CHECK: the confidential-token CPI verifies the participant ATA.
    pub participant_ata: UncheckedAccount<'info>,
    /// CHECK: the confidential-token CPI verifies the RFQ authority ATA.
    pub rfq_ata: UncheckedAccount<'info>,
    /// CHECK: canonical address checked by the RFQ instruction and token CPI.
    #[account(mut)]
    pub participant_token_account: UncheckedAccount<'info>,
    /// CHECK: canonical address checked by the RFQ instruction and token CPI.
    #[account(mut)]
    pub rfq_token_account: UncheckedAccount<'info>,
    /// CHECK: canonical address checked by the RFQ instruction and token CPI.
    #[account(mut)]
    pub participant_balance_store: UncheckedAccount<'info>,
    /// CHECK: canonical address checked by the RFQ instruction and token CPI.
    #[account(mut)]
    pub rfq_balance_store: UncheckedAccount<'info>,
}
impl<'info> TokenSide<'info> {
    pub fn validate(&self, user: Pubkey, authority: Pubkey) -> Result<()> {
        validate_token_store(
            &self.participant_token_account,
            &self.participant_balance_store,
            self.confidential_mint.key(),
            user,
            false,
        )?;
        validate_token_store(
            &self.rfq_token_account,
            &self.rfq_balance_store,
            self.confidential_mint.key(),
            authority,
            true,
        )
    }

    /// Accept an existing canonical escrow, including one permissionlessly prepared by
    /// another caller. Funding happens only after its owner, mint, and host store are bound.
    pub fn validate_prepared_escrow(&self, authority: Pubkey) -> Result<()> {
        validate_token_store(
            &self.rfq_token_account,
            &self.rfq_balance_store,
            self.confidential_mint.key(),
            authority,
            false,
        )
    }
}

/// Bind token ownership, mint, and balance storage. Initialized accounts supply their
/// owner-checked canonical bumps, avoiding repeated PDA searches during every transfer.
/// Only creation accepts empty escrow accounts, whose canonical addresses still get checked.
fn validate_token_store(
    token_account: &AccountInfo,
    balance_store: &AccountInfo,
    mint: Pubkey,
    authority: Pubkey,
    allow_empty: bool,
) -> Result<()> {
    if allow_empty && token_account.data_is_empty() {
        let token = ct::token_account_address(mint, authority).0;
        require!(
            token_account.key() == token
                && balance_store.key() == ct::encrypted_store_address(mint, token).0,
            ConfidentialRfqError::InvalidRfqAccounts
        );
    } else {
        require_keys_eq!(
            *token_account.owner,
            ct::ID,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        let token = ct::ConfidentialTokenAccount::try_deserialize(
            &mut &token_account.try_borrow_data()?[..],
        )?;
        require!(
            token.owner == authority && token.mint == mint,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        let token_key = Pubkey::create_program_address(
            &[
                b"token-account",
                mint.as_ref(),
                authority.as_ref(),
                &[token.bump],
            ],
            &ct::ID,
        )
        .map_err(|_| error!(ConfidentialRfqError::InvalidRfqAccounts))?;
        require!(
            token_account.key() == token_key,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        let store = crate::util::rfq::read_encrypted_store(balance_store)?;
        require!(
            store.program == ct::ID
                && store.authority == token_key
                && store.scope == mint.to_bytes()
                && store.get(&ct::balance_key()).is_some(),
            ConfidentialRfqError::InvalidRfqAccounts
        );
    }
    Ok(())
}
