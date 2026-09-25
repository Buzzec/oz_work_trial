//! Create an admin-owned market with an empty maker map.

use crate::{
    state::{CurrentAccountVersion, market::Market},
    util::{market::market_space, pda::market_maker_group_address},
};
use anchor_lang::prelude::*;
use std::collections::BTreeMap;

#[derive(Accounts)]
pub struct CreateMarket<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    /// A fresh keypair so the creator may open more than one market.
    #[account(init, payer = admin, space = market_space(0))]
    pub market: Account<'info, Market>,
    pub system_program: Program<'info, System>,
}

pub fn create_market(ctx: Context<CreateMarket>) -> Result<()> {
    let (_, maker_group_bump) = market_maker_group_address(&ctx.accounts.market);
    ctx.accounts.market.set_inner(Market {
        version: Market::VERSION,
        admin: ctx.accounts.admin.key(),
        maker_group_bump,
        makers: BTreeMap::new(),
    });
    Ok(())
}
