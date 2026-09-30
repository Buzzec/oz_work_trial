//! Permissionless user settlement and owner-authorized cancellation.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{
    Bool, Encrypted, ExecutionCpiAccounts, FheExecution, FheExecutionBuilder, FheHandle,
    ReturningFheExecution, Scalar, Store, StoreId, Uint,
};
use zama_host::{EncryptedStore, program::ZamaHost};

use crate::{
    ConfidentialRfqError,
    state::rfq::{RFQ, RFQPrivateField, RFQState, invalid_fhe},
    util::{
        cpi,
        pda::rfq_state_signer_seeds,
        rfq::validate_rfq_store,
        token_side::{
            __client_accounts_token_side, __cpi_client_accounts_token_side, TokenSide,
            TokenSideBumps,
        },
    },
};

#[derive(Accounts)]
pub struct ClaimRfqUser<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,
    /// CHECK: matched to the RFQ's recorded user; never selected by the caller.
    pub user: UncheckedAccount<'info>,
    pub rfq: AccountLoader<'info, RFQ>,
    #[account(mut)]
    pub rfq_store: Box<Account<'info, EncryptedStore>>,
    pub asset: TokenSide<'info>,
    pub basis: TokenSide<'info>,
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
    /// CHECK: validated by the confidential-token CPI.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ct::program::ConfidentialToken>,
    pub system_program: Program<'info, System>,
}

pub fn claim_rfq_user<'info>(ctx: Context<'info, ClaimRfqUser<'info>>) -> Result<()> {
    settle_user(ctx, false)
}

/// Validate the beneficiary, compute both private payouts, and transfer them atomically.
/// Cancellation and normal settlement share the same transfer path; only their FHE
/// predicates and terminal states differ. The final side writes the act-once marker.
pub(super) fn settle_user<'info>(
    ctx: Context<'info, ClaimRfqUser<'info>>,
    cancel: bool,
) -> Result<()> {
    let rfq_key = ctx.accounts.rfq.key();
    let rfq = *ctx.accounts.rfq.load()?;
    require_keys_eq!(
        ctx.accounts.user.key(),
        rfq.user,
        ConfidentialRfqError::InvalidUser
    );
    if cancel {
        require_keys_eq!(
            ctx.accounts.caller.key(),
            rfq.user,
            ConfidentialRfqError::InvalidUser
        );
    }
    validate_rfq_store(
        rfq_key,
        &rfq,
        ctx.accounts.rfq_store.key(),
        &ctx.accounts.rfq_store,
    )?;
    for (side, mint) in [
        (&ctx.accounts.asset, rfq.asset_mint),
        (&ctx.accounts.basis, rfq.basis_mint),
    ] {
        require_keys_eq!(
            side.confidential_mint.key(),
            mint,
            ConfidentialRfqError::MintMismatch
        );
        side.validate(rfq.user, rfq_key)?;
    }
    let now = u64::try_from(Clock::get()?.unix_timestamp)
        .map_err(|_| error!(ConfidentialRfqError::InvalidRfqInput))?;
    let rfq_seeds = rfq_state_signer_seeds(&rfq, &rfq.nonce);
    let signers: &[&[&[u8]]] = &[&rfq_seeds];

    // Each amount receives a transient grant to its own escrow. Basis runs first;
    // the asset batch commits Claimed/Canceled only after both amounts are computed.
    for asset in [false, true] {
        let side = if asset {
            &ctx.accounts.asset
        } else {
            &ctx.accounts.basis
        };
        let target = ct::balance_slot(side.confidential_mint.key(), side.rfq_token_account.key()).0;
        let execution = user_payout_execution(&ctx.accounts.rfq_store, target, asset, cancel, now)?;
        let handle = cpi::invoke_returning(
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
            [
                ctx.accounts.rfq_store.to_account_info(),
                side.rfq_balance_store.to_account_info(),
            ],
            [ctx.accounts.rfq.to_account_info()],
            signers,
        )?;
        cpi::transfer_from_grant(
            ctx.accounts.confidential_token_program.key(),
            ct::cpi::accounts::ConfidentialTransferFromValue {
                owner: ctx.accounts.rfq.to_account_info(),
                payer: ctx.accounts.caller.to_account_info(),
                mint: side.confidential_mint.to_account_info(),
                underlying_mint: side.underlying_mint.to_account_info(),
                from_ata: side.rfq_ata.to_account_info(),
                to_ata: side.participant_ata.to_account_info(),
                from_account: side.rfq_token_account.to_account_info(),
                to_account: side.participant_token_account.to_account_info(),
                from_store: side.rfq_balance_store.to_account_info(),
                to_store: side.participant_balance_store.to_account_info(),
                amount_store: None,
                amount_authority: None,
                zama_event_authority: ctx.accounts.zama_event_authority.to_account_info(),
                transient_store: ctx.accounts.transient_store.to_account_info(),
                instructions: ctx.accounts.instructions.to_account_info(),
                zama_program: ctx.accounts.zama_program.to_account_info(),
                host_config: ctx.accounts.host_config.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                hcu_block_meter: None,
                hcu_trusted_app_record: None,
                event_authority: ctx
                    .accounts
                    .confidential_token_event_authority
                    .to_account_info(),
                program: ctx.accounts.confidential_token_program.to_account_info(),
            },
            signers,
            handle,
        )?;
    }
    Ok(())
}

/// Calculate either the original deposit refund or the completed trade and buyer change.
/// Repeated or premature calls transfer zero and preserve the encrypted state.
#[inline(never)]
fn user_payout_execution(
    account: &EncryptedStore,
    target: StoreId,
    asset: bool,
    cancel: bool,
    now: u64,
) -> Result<ReturningFheExecution<Uint<64>>> {
    let store = Box::new(Store::new(account));
    let input = Box::new(UserPayoutInputs {
        state: store
            .get::<Uint<8>>(RFQPrivateField::State.key())
            .map_err(invalid_fhe)?,
        timeout: store
            .get::<Uint<64>>(RFQPrivateField::ExpireTimestamp.key())
            .map_err(invalid_fhe)?,
        buyer: store
            .get::<Bool>(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)?,
        size: store
            .get::<Uint<64>>(RFQPrivateField::Size.key())
            .map_err(invalid_fhe)?,
        limit: store
            .get::<Uint<64>>(RFQPrivateField::OfferLimit.key())
            .map_err(invalid_fhe)?,
        best: store
            .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
            .map_err(invalid_fhe)?,
        count: store
            .get::<Uint<32>>(RFQPrivateField::BidCount.key())
            .map_err(invalid_fhe)?,
    });
    FheExecution::build_returning(
        store.id(),
        #[inline(never)]
        move |fhe| {
            // Compute the private amount, then seal the state only on the final side.
            let values = user_payout_amount(fhe, &input, asset, cancel, now)?;
            fhe.output(values.payout, store.result().allow_transient(target))?;
            if asset {
                finalize_user_claim(fhe, &store, &input, &values, cancel)?;
            }
            Ok(values.payout)
        },
    )
    .map_err(invalid_fhe)
}

/// Calculate the due token amount and private authorization in one bounded SBF frame.
#[inline(never)]
fn user_payout_amount<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &UserPayoutInputs,
    asset: bool,
    cancel: bool,
    now: u64,
) -> zama_fhe::Result<Box<UserPayoutValues<'id>>> {
    let zero = fhe.trivial_encrypt_u64(0)?;
    let original = if asset {
        fhe.if_then_else(input.buyer, zero, input.size)?
    } else {
        fhe.if_then_else(input.buyer, input.limit, zero)?
    };
    if cancel {
        // An unfunded quote can be canceled at any time, but has no deposit to refund.
        let valid = fhe.eq(input.state, Scalar::<Uint<8>>::u8(RFQState::Valid as u8))?;
        let pending = fhe.gt(input.timeout, Scalar::<Uint<64>>::u64(now))?;
        let funded_cancel = fhe.and(valid, pending)?;
        let unfunded = fhe.eq(input.state, Scalar::<Uint<8>>::u8(RFQState::Unfunded as u8))?;
        let allowed = fhe.or(funded_cancel, unfunded)?;
        let payout = fhe.if_then_else(funded_cancel, original, zero)?;
        return Ok(Box::new(UserPayoutValues { payout, allowed }));
    }
    let allowed = fhe.eq(
        input.state,
        Scalar::<Uint<8>>::u8(RFQState::Claimable as u8),
    )?;
    let winner = fhe.ne(input.best, Scalar::<Uint<64>>::u64(0))?;
    let completed = if asset {
        fhe.if_then_else(input.buyer, input.size, zero)?
    } else {
        let change = fhe.sub(input.limit, input.best)?;
        fhe.if_then_else(input.buyer, change, input.best)?
    };
    let due = fhe.if_then_else(winner, completed, original)?;
    let payout = fhe.if_then_else(allowed, due, zero)?;
    Ok(Box::new(UserPayoutValues { payout, allowed }))
}

/// Consume the user's claim or cancellation and update the public closure predicate.
/// This continues the same FHE batch, preserving atomic payout/state transitions.
#[inline(never)]
fn finalize_user_claim<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    store: &Store,
    input: &UserPayoutInputs,
    values: &UserPayoutValues<'id>,
    cancel: bool,
) -> zama_fhe::Result<()> {
    let terminal = if cancel {
        RFQState::Canceled
    } else {
        RFQState::Claimed
    };
    let terminal = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(terminal as u8))?;
    let next_state = fhe.if_then_else(values.allowed, terminal, input.state)?;
    let claimed = fhe.eq(next_state, Scalar::<Uint<8>>::u8(RFQState::Claimed as u8))?;
    let canceled = fhe.eq(next_state, Scalar::<Uint<8>>::u8(RFQState::Canceled as u8))?;
    let invalid = fhe.eq(next_state, Scalar::<Uint<8>>::u8(RFQState::Invalid as u8))?;
    let terminal = fhe.or(claimed, canceled)?;
    let terminal = fhe.or(terminal, invalid)?;
    let empty = fhe.eq(input.count, Scalar::<Uint<32>>::u32(0))?;
    let can_close = fhe.and(terminal, empty)?;
    fhe.output(
        next_state,
        store.set(RFQPrivateField::State.key()).make_public(),
    )?;
    fhe.output(
        can_close,
        store.set(RFQPrivateField::CanClose.key()).make_public(),
    )?;
    Ok(())
}

struct UserPayoutValues<'id> {
    payout: Encrypted<'id, Uint<64>>,
    allowed: Encrypted<'id, Bool>,
}

struct UserPayoutInputs {
    state: FheHandle<Uint<8>>,
    timeout: FheHandle<Uint<64>>,
    buyer: FheHandle<Bool>,
    size: FheHandle<Uint<64>>,
    limit: FheHandle<Uint<64>>,
    best: FheHandle<Uint<64>>,
    count: FheHandle<Uint<32>>,
}
