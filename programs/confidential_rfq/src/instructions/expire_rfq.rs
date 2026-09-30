//! Advance a live RFQ after its confidential deadline, without revealing that deadline.

use anchor_lang::prelude::*;
use zama_fhe::{ExecutionCpiAccounts, FheExecution, Scalar, Store, Uint};
use zama_host::{EncryptedStore, program::ZamaHost};

use crate::{
    ConfidentialRfqError, CurrentAccountVersion,
    state::{
        market::Market,
        rfq::{RFQ, RFQPrivateField, RFQState, invalid_fhe},
    },
    util::{cpi, pda::rfq_state_signer_seeds, rfq::validate_rfq_store},
};

#[derive(Accounts)]
pub struct ExpireRfq<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    pub rfq: AccountLoader<'info, RFQ>,
    #[account(mut)]
    pub rfq_store: Box<Account<'info, EncryptedStore>>,
    /// CHECK: validated by the host CPI.
    pub host_config: UncheckedAccount<'info>,
    /// CHECK: validated by the host CPI.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: validated by the host CPI.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: validated by the host CPI.
    pub instructions: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
}

pub fn expire_rfq<'info>(
    ctx: Context<'info, ExpireRfq<'info>>,
    maker_id: Option<u32>,
) -> Result<()> {
    let rfq_key = ctx.accounts.rfq.key();
    let rfq = *ctx.accounts.rfq.load()?;
    require_eq!(
        ctx.accounts.market.version,
        Market::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require_keys_eq!(
        ctx.accounts.market.key(),
        rfq.market,
        ConfidentialRfqError::MarketMismatch
    );
    // Both the user and a maker with deadline access may request the FHE transition.
    if ctx.accounts.caller.key() != rfq.user {
        let id = maker_id.ok_or(error!(ConfidentialRfqError::UnauthorizedMaker))?;
        require!(
            ctx.accounts.market.active_maker(id) == Some(ctx.accounts.caller.key()),
            ConfidentialRfqError::UnauthorizedMaker
        );
    }
    validate_rfq_store(
        rfq_key,
        &rfq,
        ctx.accounts.rfq_store.key(),
        &ctx.accounts.rfq_store,
    )?;
    let now = u64::try_from(Clock::get()?.unix_timestamp)
        .map_err(|_| error!(ConfidentialRfqError::InvalidRfqInput))?;
    let execution = expiry_execution(&ctx.accounts.rfq_store, now)?;
    let rfq_seeds = rfq_state_signer_seeds(&rfq, &rfq.nonce);
    cpi::invoke(
        execution,
        ExecutionCpiAccounts {
            payer: ctx.accounts.caller.to_account_info(),
            authority: ctx.accounts.rfq.to_account_info(),
            host_config: ctx.accounts.host_config.to_account_info(),
            deny_scope_records: ctx.remaining_accounts.to_vec(),
            system_program: ctx.accounts.system_program.to_account_info(),
            hcu_block_meter: None,
            hcu_trusted_app_record: None,
            rand_nonce: None,
            event_authority: ctx.accounts.zama_event_authority.to_account_info(),
            transient_store: ctx.accounts.transient_store.to_account_info(),
            instructions: ctx.accounts.instructions.to_account_info(),
            program: ctx.accounts.zama_program.to_account_info(),
        },
        [ctx.accounts.rfq_store.to_account_info()],
        [ctx.accounts.rfq.to_account_info()],
        &[&rfq_seeds],
    )
}

/// Apply the deadline and state predicates privately; an empty auction finishes immediately.
#[inline(never)]
fn expiry_execution(account: &EncryptedStore, now: u64) -> Result<FheExecution> {
    let store = Store::new(account);
    let state = store
        .get::<Uint<8>>(RFQPrivateField::State.key())
        .map_err(invalid_fhe)?;
    let timeout = store
        .get::<Uint<64>>(RFQPrivateField::ExpireTimestamp.key())
        .map_err(invalid_fhe)?;
    let count = store
        .get::<Uint<32>>(RFQPrivateField::BidCount.key())
        .map_err(invalid_fhe)?;
    FheExecution::build(store.id(), |fhe| {
        let live = fhe.eq(state, Scalar::<Uint<8>>::u8(RFQState::Valid as u8))?;
        let elapsed = fhe.le(timeout, Scalar::<Uint<64>>::u64(now))?;
        let expire = fhe.and(live, elapsed)?;
        let empty = fhe.eq(count, Scalar::<Uint<32>>::u32(0))?;
        let claimable = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(RFQState::Claimable as u8))?;
        let expired = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(RFQState::Expired as u8))?;
        let next = fhe.if_then_else(empty, claimable, expired)?;
        let next = fhe.if_then_else(expire, next, state)?;
        fhe.output(next, store.set(RFQPrivateField::State.key()).make_public())?;
        Ok(())
    })
    .map_err(invalid_fhe)
}
