//! Add one maker and delegate market-group decryption rights to them.

use crate::util::{
    cpi,
    market::{add_maker_entry, market_space},
    pda::{market_maker_group_seeds, market_maker_group_signer_seeds},
};
use anchor_lang::prelude::*;
use zama_host::program::ZamaHost;

#[derive(Accounts)]
pub struct AddMaker<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        mut,
        has_one = admin,
        realloc = market_space(market.makers.len() + 1),
        realloc::payer = admin,
        realloc::zero = false,
    )]
    pub market: Account<'info, crate::state::market::Market>,
    /// CHECK: Group PDA
    #[account(seeds = market_maker_group_seeds(&market).as_slice(), bump = market.maker_group_bump)]
    pub maker_group: UncheckedAccount<'info>,
    /// CHECK: Checked by CPI
    pub host_config: UncheckedAccount<'info>,
    /// CHECK: Checked by CPI
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
}

pub fn add_maker(ctx: Context<AddMaker>, maker_id: u64, maker: Pubkey) -> Result<()> {
    add_maker_entry(&mut ctx.accounts.market, maker_id, maker)?;

    let market_key = ctx.accounts.market.key();
    let seeds = &market_maker_group_signer_seeds(&ctx.accounts.market);

    cpi::delegate_for_user_decryption(
        ctx.accounts.zama_program.key(),
        zama_host::cpi::accounts::DelegateForUserDecryption {
            payer: ctx.accounts.admin.to_account_info(),
            delegator: ctx.accounts.maker_group.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            delegation_record: ctx.accounts.delegation_record.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
        },
        &[seeds],
        maker,
        Pubkey::new_from_array(zama_host::WILDCARD_AUTHORITY_BYTES),
        u64::MAX,
    )
}
