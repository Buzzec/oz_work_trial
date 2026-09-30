//! Cancel a live RFQ before its private expiry and return the user's deposit.

use super::claim_user::{ClaimRfqUser, settle_user};
use anchor_lang::prelude::*;

pub fn cancel_quote<'info>(ctx: Context<'info, ClaimRfqUser<'info>>) -> Result<()> {
    settle_user(ctx, true)
}
