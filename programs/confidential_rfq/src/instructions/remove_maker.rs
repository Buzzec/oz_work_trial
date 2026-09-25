//! Remove one maker and revoke their market-group decryption rights.

use crate::util::{
    cpi,
    market::{market_space, remove_maker_entry},
    pda::{market_maker_group_seeds, market_maker_group_signer_seeds},
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
        realloc = market_space(market.makers.len().saturating_sub(1)),
        realloc::payer = admin,
        realloc::zero = false,
    )]
    pub market: Account<'info, crate::state::market::Market>,
    /// CHECK: canonical market group PDA; signs the host revocation CPI.
    #[account(seeds = market_maker_group_seeds(market.to_account_info().key).as_slice(), bump = market.maker_group_bump)]
    pub maker_group: UncheckedAccount<'info>,
    pub host_config: Box<Account<'info, zama_host::HostConfig>>,
    /// CHECK: canonical host delegation PDA, checked by host CPI.
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
}

pub fn remove_maker(ctx: Context<RemoveMaker>, maker_id: u64) -> Result<()> {
    remove_maker_entry(&mut ctx.accounts.market, maker_id)?;

    let market_key = ctx.accounts.market.key();
    let seeds =
        &market_maker_group_signer_seeds(&market_key, &ctx.accounts.market.maker_group_bump);

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
