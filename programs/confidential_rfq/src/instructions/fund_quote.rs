//! Fund an existing RFQ atomically, refunding every deposit that cannot activate it.

use anchor_lang::prelude::*;
use confidential_token::{self as ct, program::ConfidentialToken};
use zama_fhe::{
    Bool, Encrypted, FheExecution, FheExecutionBuilder, FheHandle, Scalar, Store, Uint,
};
use zama_host::{CoprocessorInputAttestation, program::ZamaHost};

use crate::{
    ConfidentialRfqError,
    state::rfq::{RFQ, RFQPrivateField, RFQState, RFQStore, invalid_fhe},
    util::{
        InputExt,
        pda::rfq_state_signer_seeds,
        request_quote_cpi::{AssetOrBasis, refund_to_user, transfer_to_escrow},
        rfq::{read_encrypted_store, validate_rfq_store},
        token_side::*,
    },
};

#[derive(Accounts)]
pub struct FundQuote<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(has_one = user @ ConfidentialRfqError::InvalidUser)]
    pub rfq: AccountLoader<'info, RFQ>,
    /// CHECK: the constraint validates ownership and RFQ identity; reread after CPI mutations.
    #[account(
        mut,
        constraint = {
            let state = read_encrypted_store(&rfq_store)?;
            validate_rfq_store(rfq.key(), &*rfq.load()?, rfq_store.key(), &state)?;
            true
        },
    )]
    pub rfq_store: UncheckedAccount<'info>,
    #[account(
        constraint = asset.confidential_mint.key() == rfq.load()?.asset_mint
            @ ConfidentialRfqError::InvalidRfqAccounts,
        constraint = {
            asset.validate(user.key(), rfq.key())?;
            asset.validate_prepared_escrow(rfq.key())?;
            true
        },
    )]
    pub asset: TokenSide<'info>,
    #[account(
        constraint = basis.confidential_mint.key() == rfq.load()?.basis_mint
            @ ConfidentialRfqError::InvalidRfqAccounts,
        constraint = {
            basis.validate(user.key(), rfq.key())?;
            basis.validate_prepared_escrow(rfq.key())?;
            true
        },
    )]
    pub basis: TokenSide<'info>,
    /// CHECK: host validates the canonical config.
    pub host_config: UncheckedAccount<'info>,
    /// CHECK: host validates its event authority.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: host validates the shared transaction journal.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: host validates the Instructions sysvar and final close.
    pub instructions: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    /// CHECK: confidential-token validates its event authority.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ConfidentialToken>,
    pub system_program: Program<'info, System>,
}

/// Deposit both encrypted amounts and decide activation entirely in FHE. Only Unfunded
/// transitions to Valid or Invalid; every other state is preserved and all new deposits refunded.
pub fn fund_quote<'info>(
    ctx: Context<'info, FundQuote<'info>>,
    asset_escrow: Box<CoprocessorInputAttestation>,
    basis_escrow: Box<CoprocessorInputAttestation>,
) -> Result<()> {
    let rfq = *ctx.accounts.rfq.load()?;
    for input in [&asset_escrow, &basis_escrow] {
        input.validate(ctx.accounts.user.key(), ct::ID)?;
    }
    let seeds = rfq_state_signer_seeds(&rfq, &rfq.nonce);

    // Both directions always execute. Actual transferred amounts drive validation and refunds.
    let asset = transfer_to_escrow(&ctx, *asset_escrow, AssetOrBasis::Asset)?;
    let basis = transfer_to_escrow(&ctx, *basis_escrow, AssetOrBasis::Basis)?;
    let [asset_refund, basis_refund] = validate_and_prepare_refunds(&ctx, asset, basis, &seeds)?;
    refund_to_user(&ctx, asset_refund, true, &seeds)?;
    refund_to_user(&ctx, basis_refund, false, &seeds)?;
    Ok(())
}

/// Activate only an unfunded quote with exact collateral, preserving every other state.
/// The final two arithmetic results are authenticated journal entries consumed by the refunds.
#[inline(never)]
fn validate_and_prepare_refunds<'info>(
    ctx: &Context<'info, FundQuote<'info>>,
    asset_handle: [u8; 32],
    basis_handle: [u8; 32],
    authority_seeds: &[&[u8]],
) -> Result<[[u8; 32]; 2]> {
    let state = read_encrypted_store(&ctx.accounts.rfq_store)?;
    let store = Box::new(RFQStore(Store::new(&state)));
    let buyer = Box::new(store.user_buyer()?);
    let size = Box::new(store.size()?);
    let limit = Box::new(store.offer_limit()?);
    let previous_state = Box::new(store.state()?);
    let previous_can_close = Box::new(store.can_close()?);
    let asset = store
        .granted::<Uint<64>>(asset_handle)
        .map_err(invalid_fhe)?;
    let basis = store
        .granted::<Uint<64>>(basis_handle)
        .map_err(invalid_fhe)?;
    let targets = [
        ct::balance_slot(
            ctx.accounts.asset.confidential_mint.key(),
            ctx.accounts.asset.rfq_token_account.key(),
        )
        .0,
        ct::balance_slot(
            ctx.accounts.basis.confidential_mint.key(),
            ctx.accounts.basis.rfq_token_account.key(),
        )
        .0,
    ];
    let execution = FheExecution::build(
        store.id(),
        #[inline(never)]
        |fhe| {
            // Separate Rust frames keep the large typed inputs within Solana's stack limit.
            let exact = Box::new(escrow_matches_quote(
                fhe, &buyer, &size, &limit, &asset, &basis,
            )?);
            let unfunded = Box::new(fhe.eq(
                *previous_state,
                Scalar::<Uint<8>>::u8(RFQState::Unfunded as u8),
            )?);
            let activated = fhe.and(*unfunded, *exact)?;
            let refund = fhe.not(activated)?;
            let valid = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(RFQState::Valid as u8))?;
            let invalid = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(RFQState::Invalid as u8))?;
            let funding_state = fhe.if_then_else(*exact, valid, invalid)?;
            let next_state = fhe.if_then_else(*unfunded, funding_state, *previous_state)?;
            let failed = fhe.not(*exact)?;
            let next_can_close = fhe.if_then_else(*unfunded, failed, *previous_can_close)?;
            fhe.output(
                next_state,
                store.set(RFQPrivateField::State.key()).make_public(),
            )?;
            fhe.output(
                next_can_close,
                store.set(RFQPrivateField::CanClose.key()).make_public(),
            )?;

            // These must remain the final two arithmetic steps for exact journal extraction.
            let zero = fhe.trivial_encrypt_u64(0)?;
            let asset_refund = fhe.if_then_else(refund, asset, zero)?;
            let basis_refund = fhe.if_then_else(refund, basis, zero)?;
            fhe.output(asset_refund, store.result().allow_transient(targets[0]))?;
            fhe.output(basis_refund, store.result().allow_transient(targets[1]))?;
            Ok(())
        },
    )
    .map_err(invalid_fhe)?;
    crate::util::request_quote_cpi::execute_refund_preparation(
        ctx,
        execution,
        targets,
        authority_seeds,
    )
}

/// Compare both actual escrow amounts against the user's private side and terms.
#[inline(never)]
fn escrow_matches_quote<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    buyer: &FheHandle<Bool>,
    size: &FheHandle<Uint<64>>,
    limit: &FheHandle<Uint<64>>,
    asset: &FheHandle<Uint<64>>,
    basis: &FheHandle<Uint<64>>,
) -> zama_fhe::Result<Encrypted<'id, Bool>> {
    let zero = fhe.trivial_encrypt_u64(0)?;
    let expected_asset = fhe.if_then_else(*buyer, zero, *size)?;
    let expected_basis = fhe.if_then_else(*buyer, *limit, zero)?;
    let asset_exact = fhe.eq(*asset, expected_asset)?;
    let basis_exact = fhe.eq(*basis, expected_basis)?;
    fhe.and(asset_exact, basis_exact)
}
