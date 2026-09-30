//! Remove one maker and revoke their market-group decryption rights.

use crate::{
    state::market::Market,
    util::{
        cpi,
        pda::{market_maker_group_seeds, market_maker_group_signer_seeds},
    },
};
use anchor_lang::prelude::*;
use zama_host::program::ZamaHost;

#[derive(Accounts)]
pub struct RemoveMaker<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        mut,
        has_one = admin,
        constraint = market.version == <Market as crate::CurrentAccountVersion>::VERSION @ crate::ConfidentialRfqError::InvalidRfqAccounts,
    )]
    pub market: Account<'info, Market>,
    /// CHECK: canonical market group PDA; signs the host revocation CPI.
    #[account(seeds = market_maker_group_seeds(&market).as_slice(), bump = market.maker_group_bump)]
    pub maker_group: UncheckedAccount<'info>,
    pub host_config: Box<Account<'info, zama_host::HostConfig>>,
    /// CHECK: canonical host delegation PDA, checked by host CPI.
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
}

pub fn remove_maker(ctx: Context<RemoveMaker>, maker_id: u32) -> Result<()> {
    let maker = ctx
        .accounts
        .market
        .active_maker(maker_id)
        .ok_or(error!(crate::ConfidentialRfqError::MakerNotFound))?;
    require_keys_eq!(
        ctx.accounts.delegation_record.key(),
        zama_host::user_decryption_delegation_address(
            ctx.accounts.maker_group.key(),
            maker,
            Pubkey::new_from_array(zama_host::WILDCARD_AUTHORITY_BYTES),
        )
        .0,
        crate::ConfidentialRfqError::InvalidRfqAccounts
    );
    ctx.accounts.market.remove_maker(maker_id)?;

    let seeds = &market_maker_group_signer_seeds(&ctx.accounts.market);

    cpi::revoke_delegation_for_user_decryption(
        ctx.accounts.zama_program.key(),
        zama_host::cpi::accounts::RevokeDelegationForUserDecryption {
            delegator: ctx.accounts.maker_group.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            delegation_record: ctx.accounts.delegation_record.to_account_info(),
        },
        &[seeds],
    )
}
