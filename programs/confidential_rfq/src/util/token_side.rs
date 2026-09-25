//! The accounts for one confidential token side of an RFQ.

use crate::{
    ConfidentialRfqError,
    util::{
        ConfidentialTokenEventAuthority, Contains, HostConfig, InstructionsAccount, TransientStore,
        ZamaEventAuthority,
    },
};
use anchor_lang::prelude::*;
use confidential_token as ct;
use confidential_token::program::ConfidentialToken;
use zama_host::program::ZamaHost;

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
        let expected_user_token = ct::token_account_address(self.confidential_mint.key(), user).0;
        let expected_user_store_address =
            ct::encrypted_store_address(self.confidential_mint.key(), expected_user_token).0;

        let expected_rfq_token =
            ct::token_account_address(self.confidential_mint.key(), authority).0;
        let expected_rfq_store_address =
            ct::encrypted_store_address(self.confidential_mint.key(), expected_rfq_token).0;

        require_keys_eq!(
            self.participant_ata.key(),
            expected_user_token,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        require_keys_eq!(
            self.participant_balance_store.key(),
            expected_user_store_address,
            ConfidentialRfqError::InvalidRfqAccounts
        );

        require_keys_eq!(
            self.rfq_ata.key(),
            expected_rfq_token,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        require_keys_eq!(
            self.rfq_balance_store.key(),
            expected_rfq_store_address,
            ConfidentialRfqError::InvalidRfqAccounts
        );

        Ok(())
    }

    pub fn initialize_escrow_account<'a, C>(
        &self,
        ctx: &'a C,
        payer: &impl ToAccountInfo<'info>,
        owner: &impl ToAccountInfo<'info>,
        authority_seeds: &[&[u8]],
    ) -> Result<()>
    where
        'info: 'a,
        C: Contains<'a, &'a Program<'info, ConfidentialToken>>,
        C: Contains<'a, ZamaEventAuthority<'a, 'info>>,
        C: Contains<'a, ConfidentialTokenEventAuthority<'a, 'info>>,
        C: Contains<'a, TransientStore<'a, 'info>>,
        C: Contains<'a, InstructionsAccount<'a, 'info>>,
        C: Contains<'a, HostConfig<'a, 'info>>,
        C: Contains<'a, &'a Program<'info, System>>,
        C: Contains<'a, &'a Program<'info, ZamaHost>>,
    {
        crate::util::cpi::initialize_token_account(
            Contains::<&'a Program<'info, ConfidentialToken>>::get(ctx).key(),
            ct::cpi::accounts::InitializeTokenAccount {
                payer: payer.to_account_info(),
                owner: owner.to_account_info(),
                mint: self.confidential_mint.to_account_info(),
                token_account: self.rfq_token_account.to_account_info(),
                balance_encrypted_store: self.rfq_balance_store.to_account_info(),
                zama_event_authority: Contains::<ZamaEventAuthority>::get(ctx).to_account_info(),
                transient_store: Contains::<TransientStore>::get(ctx).to_account_info(),
                instructions: Contains::<InstructionsAccount>::get(ctx).to_account_info(),
                zama_program: Contains::<&'a Program<'info, ZamaHost>>::get(ctx).to_account_info(),
                host_config: Contains::<HostConfig>::get(ctx).to_account_info(),
                system_program: Contains::<&'a Program<'info, System>>::get(ctx).to_account_info(),
                hcu_block_meter: None,
                hcu_trusted_app_record: None,
                event_authority: Contains::<ConfidentialTokenEventAuthority>::get(ctx)
                    .to_account_info(),
                program: Contains::<&'a Program<'info, ConfidentialToken>>::get(ctx)
                    .to_account_info(),
            },
            &[authority_seeds],
        )
    }
}
