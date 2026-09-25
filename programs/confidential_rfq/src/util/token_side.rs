//! The accounts for one confidential token side of an RFQ.

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
