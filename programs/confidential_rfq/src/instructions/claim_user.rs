//! User settlement after the public RFQ timeout.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{
    Bool, Encrypted, FheExecution, FheExecutionBuilder, FheHandle, Scalar, Store, StoreId, Uint,
};
use zama_host::{EncryptedStore, program::ZamaHost};

use crate::{
    ConfidentialRfqError, CurrentAccountVersion,
    state::rfq::{RFQ, RFQPrivateField},
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
    pub user: Signer<'info>,
    pub rfq: AccountLoader<'info, RFQ>,
    /// Encrypted RFQ state, checked against the nonce-bound RFQ identity below.
    #[account(mut)]
    pub rfq_store: Box<Account<'info, EncryptedStore>>,
    pub asset: TokenSide<'info>,
    pub basis: TokenSide<'info>,
    /// CHECK: validated by ZamaHost.
    pub host_config: UncheckedAccount<'info>,
    /// CHECK: validated by ZamaHost.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: shared transaction journal, validated by ZamaHost.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: instructions sysvar, validated by ZamaHost.
    pub instructions: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    /// CHECK: validated by confidential-token.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ct::program::ConfidentialToken>,
    pub system_program: Program<'info, System>,
}

pub fn claim_rfq_user<'info>(ctx: Context<'info, ClaimRfqUser<'info>>) -> Result<()> {
    let rfq_key = ctx.accounts.rfq.key();
    let rfq = *ctx.accounts.rfq.load()?;
    let nonce = ctx.accounts.rfq_store.scope;
    let rfq_user = rfq.user;
    let asset_mint = rfq.asset_mint;
    let basis_mint = rfq.basis_mint;
    let bid_count = rfq.bid_count;
    let timeout = rfq.timeout;
    require!(
        rfq.version == RFQ::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require_keys_eq!(
        ctx.accounts.user.key(),
        rfq_user,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require_keys_eq!(
        ctx.accounts.asset.confidential_mint.key(),
        asset_mint,
        ConfidentialRfqError::MintMismatch
    );
    require_keys_eq!(
        ctx.accounts.basis.confidential_mint.key(),
        basis_mint,
        ConfidentialRfqError::MintMismatch
    );
    validate_rfq_store(
        rfq_key,
        &rfq,
        ctx.accounts.rfq_store.key(),
        &ctx.accounts.rfq_store,
    )?;
    for (mint, side) in [
        (asset_mint, &ctx.accounts.asset),
        (basis_mint, &ctx.accounts.basis),
    ] {
        for (owner, token, balance_store) in [
            (rfq_key, &side.rfq_token_account, &side.rfq_balance_store),
            (
                rfq_user,
                &side.participant_token_account,
                &side.participant_balance_store,
            ),
        ] {
            let expected_token = ct::token_account_address(mint, owner).0;
            require_keys_eq!(
                token.key(),
                expected_token,
                ConfidentialRfqError::TokenAccountMismatch
            );
            require_keys_eq!(
                balance_store.key(),
                ct::encrypted_store_address(mint, expected_token).0,
                ConfidentialRfqError::TokenAccountMismatch
            );
        }
    }
    require!(
        Clock::get()?.unix_timestamp >= timeout,
        ConfidentialRfqError::RfqNotExpired
    );
    let seeds = &rfq_state_signer_seeds(&rfq, &nonce);
    // Both encrypted payouts are produced in this one public instruction.
    // Basis runs first; the asset execution also seals UserClaimed and CanClose.
    let basis_handle = execute_user_payout(&ctx, bid_count, seeds, false)?;
    let asset_handle = execute_user_payout(&ctx, bid_count, seeds, true)?;
    cpi::transfer_user_payouts(&ctx, seeds, asset_handle, basis_handle)
}

#[inline(never)]
fn execute_user_payout<'info>(
    ctx: &Context<'info, ClaimRfqUser<'info>>,
    bid_count: u64,
    seeds: &[&[u8]],
    asset: bool,
) -> Result<[u8; 32]> {
    let store = Box::new(Store::new(&ctx.accounts.rfq_store));
    let inputs = Box::new(UserPayoutInputs {
        buyer: store
            .get::<Bool>(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)?,
        claimed: store
            .get::<Bool>(RFQPrivateField::UserClaimed.key())
            .map_err(invalid_fhe)?,
        best_offer: store
            .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
            .map_err(invalid_fhe)?,
        best_maker: store
            .get::<Uint<64>>(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)?,
        size: store
            .get::<Uint<64>>(RFQPrivateField::Size.key())
            .map_err(invalid_fhe)?,
        limit: store
            .get::<Uint<64>>(RFQPrivateField::OfferLimit.key())
            .map_err(invalid_fhe)?,
    });
    let closed_bids = store
        .get::<Uint<64>>(RFQPrivateField::ClosedBids.key())
        .map_err(invalid_fhe)?;
    let side = if asset {
        &ctx.accounts.asset
    } else {
        &ctx.accounts.basis
    };
    let token_store_id = StoreId::new(
        ct::ID,
        side.rfq_token_account.key(),
        side.confidential_mint.key().to_bytes(),
    );
    let payout_output = Box::new(store.result().allow_transient(token_store_id));
    let claimed_output = Box::new(
        store
            .set(RFQPrivateField::UserClaimed.key())
            .allow(ctx.accounts.user.key()),
    );
    let close_output = Box::new(store.set(RFQPrivateField::CanClose.key()).make_public());
    let execution = FheExecution::build_returning(store.id(), move |fhe| {
        let payout = user_payout_value(fhe, &inputs, asset)?;
        fhe.output(payout, *payout_output)?;
        if asset {
            let claimed_next = fhe.trivial_encrypt(Scalar::<Bool>::bool(true))?;
            let all_bids_closed = fhe.eq(closed_bids, Scalar::<Uint<64>>::u64(bid_count))?;
            let can_close = fhe.and(claimed_next, all_bids_closed)?;
            fhe.output(claimed_next, *claimed_output)?;
            fhe.output(can_close, *close_output)?;
        }
        Ok(payout)
    })
    .map_err(invalid_fhe)?;
    cpi::invoke_returning(
        execution,
        zama_fhe::ExecutionCpiAccounts {
            payer: ctx.accounts.user.to_account_info(),
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
        &[seeds],
    )
}

struct UserPayoutInputs {
    buyer: FheHandle<Bool>,
    claimed: FheHandle<Bool>,
    best_offer: FheHandle<Uint<64>>,
    best_maker: FheHandle<Uint<64>>,
    size: FheHandle<Uint<64>>,
    limit: FheHandle<Uint<64>>,
}

// Keep payout arithmetic in its own SBF frame, separate from execution assembly.
#[inline(never)]
fn user_payout_value<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &UserPayoutInputs,
    asset: bool,
) -> zama_fhe::Result<Encrypted<'id, Uint<64>>> {
    let not_claimed = fhe.not(input.claimed)?;
    let has_winner = fhe.ne(input.best_maker, Scalar::<Uint<64>>::u64(0))?;
    let zero = fhe.trivial_encrypt_u64(0)?;
    let due = if asset {
        let winning = fhe.if_then_else(input.buyer, input.size, zero)?;
        let no_winner = fhe.if_then_else(input.buyer, zero, input.size)?;
        fhe.if_then_else(has_winner, winning, no_winner)?
    } else {
        let buyer_change = fhe.sub(input.limit, input.best_offer)?;
        let winning = fhe.if_then_else(input.buyer, buyer_change, input.best_offer)?;
        let no_winner = fhe.if_then_else(input.buyer, input.limit, zero)?;
        fhe.if_then_else(has_winner, winning, no_winner)?
    };
    fhe.if_then_else(not_claimed, due, zero)
}

fn invalid_fhe(error: zama_fhe::FheExecutionBuildError) -> anchor_lang::error::Error {
    msg!("invalid RFQ claim FHE execution: {:?}", error);
    error!(ConfidentialRfqError::InvalidFheExecution)
}
