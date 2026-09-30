//! An approved maker commits two private prices and escrows both tokens.
//!
//! The RFQ direction is private, so a maker quotes both the price at which it
//! buys the asset and the price at which it sells it. The token program's
//! confidential transfer is all-or-zero on an insufficient balance. Its actual
//! transfer handles are granted to the RFQ store and compared to the promised
//! collateral inside FHE before either price can become the winning offer.

use crate::{
    ConfidentialRfqError, CurrentAccountVersion,
    state::{
        market::Market,
        rfq::{RFQ, RFQPrivateField},
    },
    util::{
        bid_cpi::{
            invoke_bid_execution, invoke_returning_bid_execution, refund_token, transfer_token,
        },
        pda::{bid_receipt_seeds, rfq_state_signer_seeds},
        rfq::validate_rfq_store,
    },
};
use anchor_lang::prelude::*;
use confidential_token as ct;
use confidential_token::program::ConfidentialToken;
use std::num::NonZeroU64;
use zama_fhe::{Bool, FheExecution, Scalar, Store, StoreId, Uint};
use zama_host::{CoprocessorInputAttestation, program::ZamaHost};

/// Survives maker removal from the market, so an old bid can still be claimed
/// only by the maker that submitted it.
///
/// TODO: Remove this, it's an AI hallucination.
#[account]
pub struct BidReceipt {
    pub rfq: Pubkey,
    pub maker_id: u64,
    pub maker: Pubkey,
    pub bump: u8,
}

impl BidReceipt {
    pub const SPACE: usize = 32 + 8 + 32 + 1;
}

#[derive(Accounts)]
pub struct PlaceBid<'info> {
    /// Approved maker, transfer authority, and rent payer.
    #[account(mut)]
    pub maker: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub rfq: AccountLoader<'info, RFQ>,
    #[account(
        init,
        payer = maker,
        space = 8 + BidReceipt::SPACE,
        seeds = [
            bid_receipt_seeds(&rfq, &rfq.load()?.bid_count.to_le_bytes())[0],
            bid_receipt_seeds(&rfq, &rfq.load()?.bid_count.to_le_bytes())[1],
            bid_receipt_seeds(&rfq, &rfq.load()?.bid_count.to_le_bytes())[2],
        ],
        bump,
    )]
    pub bid_receipt: Box<Account<'info, BidReceipt>>,
    /// Host state under the nonce-bound RFQ authority. Its key and metadata
    /// are checked before a transfer or execution can use it.
    #[account(mut)]
    pub rfq_store: Box<Account<'info, zama_host::EncryptedStore>>,

    pub asset_confidential_mint: Box<Account<'info, ct::ConfidentialMint>>,
    pub basis_confidential_mint: Box<Account<'info, ct::ConfidentialMint>>,
    /// CHECK: underlying mint and associated token accounts are checked by
    /// confidential-token during each transfer.
    pub asset_underlying_mint: UncheckedAccount<'info>,
    /// CHECK: see asset_underlying_mint.
    pub basis_underlying_mint: UncheckedAccount<'info>,
    /// CHECK: see asset_underlying_mint.
    pub maker_asset_ata: UncheckedAccount<'info>,
    /// CHECK: see asset_underlying_mint.
    pub maker_basis_ata: UncheckedAccount<'info>,
    /// CHECK: see asset_underlying_mint.
    pub rfq_asset_ata: UncheckedAccount<'info>,
    /// CHECK: see asset_underlying_mint.
    pub rfq_basis_ata: UncheckedAccount<'info>,

    /// CHECK: canonical confidential token accounts, checked here and by the
    /// token CPI. The CPI validates their owners and mints.
    #[account(mut)]
    pub maker_asset_token_account: UncheckedAccount<'info>,
    /// CHECK: see maker_asset_token_account.
    #[account(mut)]
    pub maker_basis_token_account: UncheckedAccount<'info>,
    /// CHECK: see maker_asset_token_account.
    #[account(mut)]
    pub rfq_asset_token_account: UncheckedAccount<'info>,
    /// CHECK: see maker_asset_token_account.
    #[account(mut)]
    pub rfq_basis_token_account: UncheckedAccount<'info>,
    /// CHECK: the confidential-token CPI checks each store's canonical key.
    #[account(mut)]
    pub maker_asset_balance_store: UncheckedAccount<'info>,
    /// CHECK: see maker_asset_balance_store.
    #[account(mut)]
    pub maker_basis_balance_store: UncheckedAccount<'info>,
    /// CHECK: see maker_asset_balance_store.
    #[account(mut)]
    pub rfq_asset_balance_store: UncheckedAccount<'info>,
    /// CHECK: see maker_asset_balance_store.
    #[account(mut)]
    pub rfq_basis_balance_store: UncheckedAccount<'info>,

    /// CHECK: validated by the confidential-token event CPI.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ConfidentialToken>,
    /// CHECK: validated by ZamaHost.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: validated by ZamaHost, including its final close instruction.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: validated by ZamaHost.
    pub instructions: UncheckedAccount<'info>,
    pub host_config: Box<Account<'info, zama_host::HostConfig>>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
    /// CHECK: optional RFQ application meter, validated by ZamaHost if used.
    #[account(mut)]
    pub hcu_block_meter: Option<UncheckedAccount<'info>>,
    /// CHECK: optional RFQ trust record, validated by ZamaHost if used.
    pub hcu_trusted_app_record: Option<UncheckedAccount<'info>>,
}

/// Places one bid for a market maker. `prices` encrypts
/// `(buy_price << 64) | sell_price` as Uint128 and is bound to this RFQ
/// program and maker. The two transfer attestations are separately bound to
/// confidential-token and the same maker.
pub fn place_bid<'info>(
    ctx: Context<'info, PlaceBid<'info>>,
    maker_id: u64,
    prices: CoprocessorInputAttestation,
    asset_transfer_attestation: CoprocessorInputAttestation,
    basis_transfer_attestation: CoprocessorInputAttestation,
) -> Result<()> {
    let id = NonZeroU64::new(maker_id).ok_or(error!(ConfidentialRfqError::InvalidMakerId))?;
    let rfq_key = ctx.accounts.rfq.key();
    let maker_key = ctx.accounts.maker.key();
    let rfq_state = *ctx.accounts.rfq.load()?;
    let RFQ {
        version: rfq_version,
        market: rfq_market,
        bid_count,
        timeout,
        asset_mint,
        basis_mint,
        ..
    } = rfq_state;
    require_eq!(
        rfq_version,
        RFQ::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require!(
        Clock::get()?.unix_timestamp < timeout,
        ConfidentialRfqError::RfqExpired
    );
    require_eq!(
        ctx.accounts.market.version,
        Market::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require_keys_eq!(
        rfq_market,
        ctx.accounts.market.key(),
        ConfidentialRfqError::MarketMismatch
    );
    require_keys_eq!(
        ctx.accounts.market.maker(maker_id).unwrap_or_default(),
        maker_key,
        ConfidentialRfqError::UnauthorizedMaker
    );
    // Eight core RFQ slots and two price slots per bid fill the host's
    // 32-slot EncryptedStore at twelve bids.
    require!(bid_count < 12, ConfidentialRfqError::BidCapacityReached);
    require_keys_eq!(
        asset_mint,
        ctx.accounts.asset_confidential_mint.key(),
        ConfidentialRfqError::MintMismatch
    );
    require_keys_eq!(
        basis_mint,
        ctx.accounts.basis_confidential_mint.key(),
        ConfidentialRfqError::MintMismatch
    );
    require_keys_eq!(
        ctx.accounts.asset_confidential_mint.underlying_mint,
        ctx.accounts.asset_underlying_mint.key(),
        ConfidentialRfqError::MintMismatch
    );
    require_keys_eq!(
        ctx.accounts.basis_confidential_mint.underlying_mint,
        ctx.accounts.basis_underlying_mint.key(),
        ConfidentialRfqError::MintMismatch
    );

    validate_rfq_store(
        rfq_key,
        &rfq_state,
        ctx.accounts.rfq_store.key(),
        &ctx.accounts.rfq_store,
    )?;
    let nonce = ctx.accounts.rfq_store.scope;
    let authority_seeds = &rfq_state_signer_seeds(&rfq_state, &nonce);
    check_token_accounts(&ctx, asset_mint, basis_mint, rfq_key)?;

    for field in [
        RFQPrivateField::MakerBuy(id),
        RFQPrivateField::MakerSell(id),
    ] {
        require!(
            ctx.accounts.rfq_store.get(&field.key()).is_none(),
            ConfidentialRfqError::BidAlreadyExists
        );
    }
    require!(
        prices.user_address == maker_key.to_bytes()
            && prices.contract_address == crate::ID.to_bytes(),
        ConfidentialRfqError::InvalidRfqInput
    );

    // The token program returns the amount actually transferred (zero if the
    // sender's encrypted balance was insufficient) and grants that exact handle
    // to this RFQ store in the shared transient store.
    let actual_asset = transfer_token(&ctx, asset_transfer_attestation, true)?;
    let actual_basis = transfer_token(&ctx, basis_transfer_attestation, false)?;
    ctx.accounts.rfq_store.reload()?;

    record_prices(&ctx, id, prices, authority_seeds)?;
    ctx.accounts.rfq_store.reload()?;
    validate_asset_collateral(&ctx, id, actual_asset, authority_seeds)?;
    ctx.accounts.rfq_store.reload()?;
    validate_basis_collateral(&ctx, id, actual_basis, authority_seeds)?;
    ctx.accounts.rfq_store.reload()?;
    let asset_refund = select_best_and_refund_asset(&ctx, id, actual_asset, authority_seeds)?;
    refund_token(&ctx, asset_refund, true, authority_seeds)?;
    ctx.accounts.rfq_store.reload()?;
    let basis_refund = finalize_bid(&ctx, id, actual_basis, authority_seeds, bid_count)?;
    refund_token(&ctx, basis_refund, false, authority_seeds)?;

    ctx.accounts.bid_receipt.set_inner(BidReceipt {
        rfq: rfq_key,
        maker_id,
        maker: maker_key,
        bump: ctx.bumps.bid_receipt,
    });
    let mut rfq = ctx.accounts.rfq.load_mut()?;
    rfq.bid_count = bid_count
        .checked_add(1)
        .ok_or(error!(ConfidentialRfqError::BidCountOverflow))?;
    Ok(())
}

#[inline(never)]
fn record_prices<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    id: NonZeroU64,
    prices_attestation: CoprocessorInputAttestation,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let state = Store::new(&ctx.accounts.rfq_store);
    let maker = ctx.accounts.maker.key();
    let execution = FheExecution::build(state.id(), |fhe| {
        let prices = fhe.verified_input::<Uint<128>>(prices_attestation)?;
        let buy_high = fhe.shr(prices, Scalar::<Uint<128>>::u128(64))?;
        let buy_price = fhe.cast::<Uint<128>, Uint<64>>(buy_high)?;
        let sell_price = fhe.cast::<Uint<128>, Uint<64>>(prices)?;
        fhe.output(
            buy_price,
            state.set(RFQPrivateField::MakerBuy(id).key()).allow(maker),
        )?;
        fhe.output(
            sell_price,
            state.set(RFQPrivateField::MakerSell(id).key()).allow(maker),
        )?;
        Ok(())
    })
    .map_err(invalid_fhe)?;
    invoke_bid_execution(ctx, execution, authority_seeds)
}

#[inline(never)]
fn validate_asset_collateral<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    id: NonZeroU64,
    actual_asset: [u8; 32],
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let state = Store::new(&ctx.accounts.rfq_store);
    let size = state
        .get::<Uint<64>>(RFQPrivateField::Size.key())
        .map_err(invalid_fhe)?;
    let buy_price = state
        .get::<Uint<64>>(RFQPrivateField::MakerBuy(id).key())
        .map_err(invalid_fhe)?;
    let asset_escrow = state
        .granted::<Uint<64>>(actual_asset)
        .map_err(invalid_fhe)?;
    let maker = ctx.accounts.maker.key();
    let execution = FheExecution::build(state.id(), |fhe| {
        let asset_funded = fhe.eq(asset_escrow, size)?;
        let size_positive = fhe.gt(size, Scalar::<Uint<64>>::u64(0))?;
        let buy_positive = fhe.gt(buy_price, Scalar::<Uint<64>>::u64(0))?;

        // A retained bid must be payable without wrapped u64 arithmetic in
        // the first two-deposit case.
        let asset_sum_safe = fhe.lt(size, Scalar::<Uint<64>>::u64(1_u64 << 63))?;
        let amounts_positive = fhe.and(size_positive, buy_positive)?;
        let collateral_safe = fhe.and(asset_funded, asset_sum_safe)?;
        let valid = fhe.and(collateral_safe, amounts_positive)?;
        let zero = fhe.trivial_encrypt_u64(0)?;
        let accepted_buy = fhe.if_then_else(valid, buy_price, zero)?;
        fhe.output(
            accepted_buy,
            state.set(RFQPrivateField::MakerBuy(id).key()).allow(maker),
        )?;
        Ok(())
    })
    .map_err(invalid_fhe)?;
    invoke_bid_execution(ctx, execution, authority_seeds)
}

#[inline(never)]
fn validate_basis_collateral<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    id: NonZeroU64,
    actual_basis: [u8; 32],
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let execution = build_basis_collateral_execution(
        &ctx.accounts.rfq_store,
        id,
        actual_basis,
        ctx.accounts.maker.key(),
    )?;
    invoke_bid_execution(ctx, execution, authority_seeds)
}

// Kept in its own SBF stack frame so FHE construction does not share a frame
// with the CPI account assembly.
#[inline(never)]
fn build_basis_collateral_execution(
    rfq_store: &zama_host::EncryptedStore,
    id: NonZeroU64,
    actual_basis: [u8; 32],
    maker: Pubkey,
) -> Result<FheExecution> {
    let state = Store::new(rfq_store);
    let limit = state
        .get::<Uint<64>>(RFQPrivateField::OfferLimit.key())
        .map_err(invalid_fhe)?;
    let accepted_asset_buy = state
        .get::<Uint<64>>(RFQPrivateField::MakerBuy(id).key())
        .map_err(invalid_fhe)?;
    let sell_price = state
        .get::<Uint<64>>(RFQPrivateField::MakerSell(id).key())
        .map_err(invalid_fhe)?;
    let basis_escrow = state
        .granted::<Uint<64>>(actual_basis)
        .map_err(invalid_fhe)?;
    FheExecution::build(state.id(), |fhe| {
        // The first validation writes zero to MakerBuy when asset funding or
        // size fails. This second validation writes zero to both
        // prices on any failure, which drives exact refunds below.
        let asset_valid = fhe.gt(accepted_asset_buy, Scalar::<Uint<64>>::u64(0))?;
        let basis_funded = fhe.eq(basis_escrow, accepted_asset_buy)?;
        let basis_total = fhe.add(accepted_asset_buy, limit)?;
        let basis_sum_safe = fhe.ge(basis_total, accepted_asset_buy)?;
        let collateral_valid = fhe.and(asset_valid, basis_funded)?;
        let valid = fhe.and(collateral_valid, basis_sum_safe)?;
        let zero = fhe.trivial_encrypt_u64(0)?;
        let accepted_buy = fhe.if_then_else(valid, accepted_asset_buy, zero)?;
        let accepted_sell = fhe.if_then_else(valid, sell_price, zero)?;
        fhe.output(
            accepted_buy,
            state.set(RFQPrivateField::MakerBuy(id).key()).allow(maker),
        )?;
        fhe.output(
            accepted_sell,
            state.set(RFQPrivateField::MakerSell(id).key()).allow(maker),
        )?;
        Ok(())
    })
    .map_err(invalid_fhe)
}

#[inline(never)]
fn select_best_and_refund_asset<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    id: NonZeroU64,
    actual_asset: [u8; 32],
    authority_seeds: &[&[u8]],
) -> Result<[u8; 32]> {
    let state = Store::new(&ctx.accounts.rfq_store);
    let user_buyer = state
        .get::<Bool>(RFQPrivateField::UserBuyer.key())
        .map_err(invalid_fhe)?;
    let best_offer = state
        .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
        .map_err(invalid_fhe)?;
    let best_maker = state
        .get::<Uint<64>>(RFQPrivateField::BestMaker.key())
        .map_err(invalid_fhe)?;
    let saved_buy = state
        .get::<Uint<64>>(RFQPrivateField::MakerBuy(id).key())
        .map_err(invalid_fhe)?;
    let saved_sell = state
        .get::<Uint<64>>(RFQPrivateField::MakerSell(id).key())
        .map_err(invalid_fhe)?;
    let asset_escrow = state
        .granted::<Uint<64>>(actual_asset)
        .map_err(invalid_fhe)?;
    let asset_store = StoreId::new(
        ct::ID,
        ctx.accounts.rfq_asset_token_account.key(),
        ctx.accounts.asset_confidential_mint.key().to_bytes(),
    );
    let execution = FheExecution::build_returning(state.id(), |fhe| {
        // Stage one stores a positive BuyPrice iff all collateral and term
        // checks succeeded. A valid losing bid remains claimable after expiry.
        let valid = fhe.gt(saved_buy, Scalar::<Uint<64>>::u64(0))?;
        let buyer_better = fhe.lt(saved_sell, best_offer)?;
        let seller_better = fhe.gt(saved_buy, best_offer)?;
        let better = fhe.if_then_else(user_buyer, buyer_better, seller_better)?;
        let candidate = fhe.if_then_else(user_buyer, saved_sell, saved_buy)?;
        let wins = fhe.and(valid, better)?;
        let next_best_offer = fhe.if_then_else(wins, candidate, best_offer)?;
        let encrypted_id = fhe.trivial_encrypt_u64(id.get())?;
        let next_best_maker = fhe.if_then_else(wins, encrypted_id, best_maker)?;
        let zero = fhe.trivial_encrypt_u64(0)?;
        let refund_asset = fhe.if_then_else(valid, zero, asset_escrow)?;
        fhe.output(next_best_offer, state.set(RFQPrivateField::BestOffer.key()))?;
        fhe.output(next_best_maker, state.set(RFQPrivateField::BestMaker.key()))?;
        fhe.output(refund_asset, state.result().allow_transient(asset_store))?;
        Ok(refund_asset)
    })
    .map_err(invalid_fhe)?;
    invoke_returning_bid_execution(ctx, execution, authority_seeds)
}

#[inline(never)]
fn finalize_bid<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    id: NonZeroU64,
    actual_basis: [u8; 32],
    authority_seeds: &[&[u8]],
    bid_count: u64,
) -> Result<[u8; 32]> {
    let basis_store = StoreId::new(
        ct::ID,
        ctx.accounts.rfq_basis_token_account.key(),
        ctx.accounts.basis_confidential_mint.key().to_bytes(),
    );
    let next_bid_count = bid_count
        .checked_add(1)
        .ok_or(error!(ConfidentialRfqError::BidCountOverflow))?;
    let execution = build_bid_finalization_execution(
        &ctx.accounts.rfq_store,
        id,
        actual_basis,
        basis_store,
        next_bid_count,
    )?;
    invoke_returning_bid_execution(ctx, execution, authority_seeds)
}

// Separating the FHE builder from the returning CPI avoids SBF stack-frame
// overflow while preserving one atomic PlaceBid instruction.
#[inline(never)]
fn build_bid_finalization_execution(
    rfq_store: &zama_host::EncryptedStore,
    id: NonZeroU64,
    actual_basis: [u8; 32],
    basis_store: StoreId,
    next_bid_count: u64,
) -> Result<zama_fhe::ReturningFheExecution<Uint<64>>> {
    let state = Store::new(rfq_store);
    let saved_buy = state
        .get::<Uint<64>>(RFQPrivateField::MakerBuy(id).key())
        .map_err(invalid_fhe)?;
    let closed_bids = state
        .get::<Uint<64>>(RFQPrivateField::ClosedBids.key())
        .map_err(invalid_fhe)?;
    let user_claimed = state
        .get::<Bool>(RFQPrivateField::UserClaimed.key())
        .map_err(invalid_fhe)?;
    let basis_escrow = state
        .granted::<Uint<64>>(actual_basis)
        .map_err(invalid_fhe)?;
    FheExecution::build_returning(state.id(), |fhe| {
        // BuyPrice is positive for every retained bid, and zero for a failed
        // bid. The exact actual basis transfer is refunded on failure.
        let failed = fhe.eq(saved_buy, Scalar::<Uint<64>>::u64(0))?;
        let zero = fhe.trivial_encrypt_u64(0)?;
        let refund_basis = fhe.if_then_else(failed, basis_escrow, zero)?;
        let failed_u64 = fhe.cast::<Bool, Uint<64>>(failed)?;
        let next_closed = fhe.add(closed_bids, failed_u64)?;
        let all_closed = fhe.eq(next_closed, Scalar::<Uint<64>>::u64(next_bid_count))?;
        let can_close = fhe.and(all_closed, user_claimed)?;
        fhe.output(next_closed, state.set(RFQPrivateField::ClosedBids.key()))?;
        fhe.output(
            can_close,
            state.set(RFQPrivateField::CanClose.key()).make_public(),
        )?;
        fhe.output(refund_basis, state.result().allow_transient(basis_store))?;
        Ok(refund_basis)
    })
    .map_err(invalid_fhe)
}

fn check_token_accounts(
    ctx: &Context<PlaceBid>,
    asset: Pubkey,
    basis: Pubkey,
    authority: Pubkey,
) -> Result<()> {
    let maker = ctx.accounts.maker.key();
    for (mint, owner, token, balance) in [
        (
            asset,
            maker,
            &ctx.accounts.maker_asset_token_account,
            &ctx.accounts.maker_asset_balance_store,
        ),
        (
            basis,
            maker,
            &ctx.accounts.maker_basis_token_account,
            &ctx.accounts.maker_basis_balance_store,
        ),
        (
            asset,
            authority,
            &ctx.accounts.rfq_asset_token_account,
            &ctx.accounts.rfq_asset_balance_store,
        ),
        (
            basis,
            authority,
            &ctx.accounts.rfq_basis_token_account,
            &ctx.accounts.rfq_basis_balance_store,
        ),
    ] {
        let expected_token = ct::token_account_address(mint, owner).0;
        require_keys_eq!(
            token.key(),
            expected_token,
            ConfidentialRfqError::TokenAccountMismatch
        );
        require_keys_eq!(
            balance.key(),
            ct::encrypted_store_address(mint, expected_token).0,
            ConfidentialRfqError::TokenAccountMismatch
        );
    }
    Ok(())
}

fn invalid_fhe(error: zama_fhe::FheExecutionBuildError) -> anchor_lang::error::Error {
    msg!("invalid bid FHE execution: {:?}", error);
    error!(ConfidentialRfqError::InvalidFheExecution)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{InstructionData, ToAccountMetas};
    use solana_sdk::{
        hash::Hash,
        instruction::Instruction,
        message::{AddressLookupTableAccount, v0},
    };
    use std::str::FromStr;

    fn minimal_attestation(user: Pubkey, contract: Pubkey) -> CoprocessorInputAttestation {
        CoprocessorInputAttestation {
            input_handle: [1; 32],
            ct_handles: vec![[1; 32]],
            handle_index: 0,
            user_address: user.to_bytes(),
            contract_address: contract.to_bytes(),
            contract_chain_id: 1,
            extra_data: Vec::new(),
            signatures: vec![[0; 65]],
        }
    }

    #[test]
    fn packed_bid_minimal_proof_fits_v0_transaction_packet() {
        let maker = Pubkey::new_unique();
        let next = || Pubkey::new_unique();
        let accounts = crate::accounts::PlaceBid {
            maker,
            market: next(),
            rfq: next(),
            bid_receipt: next(),
            rfq_store: next(),
            asset_confidential_mint: next(),
            basis_confidential_mint: next(),
            asset_underlying_mint: next(),
            basis_underlying_mint: next(),
            maker_asset_ata: next(),
            maker_basis_ata: next(),
            rfq_asset_ata: next(),
            rfq_basis_ata: next(),
            maker_asset_token_account: next(),
            maker_basis_token_account: next(),
            rfq_asset_token_account: next(),
            rfq_basis_token_account: next(),
            maker_asset_balance_store: next(),
            maker_basis_balance_store: next(),
            rfq_asset_balance_store: next(),
            rfq_basis_balance_store: next(),
            confidential_token_event_authority: next(),
            confidential_token_program: ct::ID,
            zama_event_authority: next(),
            transient_store: zama_host::transient_store_address(maker).0,
            instructions: Pubkey::from_str("Sysvar1nstructions1111111111111111111111111").unwrap(),
            host_config: next(),
            zama_program: zama_host::ID,
            system_program: System::id(),
            hcu_block_meter: None,
            hcu_trusted_app_record: None,
        };
        let instruction = Instruction {
            program_id: crate::ID,
            accounts: accounts.to_account_metas(None),
            data: crate::instruction::PlaceBid {
                maker_id: 1,
                prices: minimal_attestation(maker, crate::ID),
                asset_transfer_attestation: minimal_attestation(maker, ct::ID),
                basis_transfer_attestation: minimal_attestation(maker, ct::ID),
            }
            .data(),
        };
        let envelope = zama_solana_test_kit::transaction::fhe_transaction(maker, [instruction]);
        let mut lookup_addresses: Vec<Pubkey> = envelope
            .iter()
            .flat_map(|ix| ix.accounts.iter())
            .filter(|meta| !meta.is_signer)
            .map(|meta| meta.pubkey)
            .collect();
        lookup_addresses.sort_unstable();
        lookup_addresses.dedup();
        let table = AddressLookupTableAccount {
            key: next(),
            addresses: lookup_addresses,
        };
        let message = v0::Message::try_compile(&maker, &envelope, &[table], Hash::default())
            .expect("v0 message should compile");
        let wire_size = 1
            + 64 * usize::from(message.header.num_required_signatures)
            + message.serialize().len();
        eprintln!("packed bid wire size: {wire_size} bytes");
        assert!(
            wire_size <= 1_232,
            "minimal signed packed bid transaction is {wire_size} bytes"
        );
    }
}
