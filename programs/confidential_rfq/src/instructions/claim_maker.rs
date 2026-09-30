//! Permissionless settlement of one maker's original collateral and trade proceeds.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{
    Bool, Encrypted, ExecutionCpiAccounts, FheExecution, FheExecutionBuilder, FheHandle,
    ReturningFheExecution, Scalar, Store, StoreId, Uint,
};
use zama_host::{EncryptedStore, program::ZamaHost};

use crate::{
    ConfidentialRfqError, CurrentAccountVersion,
    state::{
        market::Market,
        rfq::{MakerPrivateField, RFQ, RFQPrivateField, RFQState, invalid_fhe},
    },
    util::{
        cpi,
        pda::{maker_store_address, maker_store_signer_seeds, rfq_state_signer_seeds},
        rfq::{validate_maker_store, validate_rfq_store},
        token_side::{
            __client_accounts_token_side, __cpi_client_accounts_token_side, TokenSide,
            TokenSideBumps,
        },
    },
};

#[derive(Accounts)]
pub struct ClaimRfqMaker<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,
    /// CHECK: matched to the market's permanent maker identity, including disabled makers.
    pub maker: UncheckedAccount<'info>,
    pub market: Box<Account<'info, Market>>,
    pub rfq: AccountLoader<'info, RFQ>,
    #[account(mut)]
    pub rfq_store: Box<Account<'info, EncryptedStore>>,
    /// CHECK: validated against the RFQ-specific maker authority PDA.
    pub maker_store_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub maker_store: Box<Account<'info, EncryptedStore>>,
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

pub fn claim_rfq_maker<'info>(
    ctx: Context<'info, ClaimRfqMaker<'info>>,
    maker_id: u32,
) -> Result<()> {
    let rfq_key = ctx.accounts.rfq.key();
    let rfq = *ctx.accounts.rfq.load()?;
    require_eq!(
        ctx.accounts.market.version,
        Market::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
    require_keys_eq!(
        ctx.accounts.market.key(),
        rfq.market,
        ConfidentialRfqError::MarketMismatch
    );
    require!(
        ctx.accounts.market.maker(maker_id) == Some(ctx.accounts.maker.key()),
        ConfidentialRfqError::UnauthorizedMaker
    );
    validate_rfq_store(
        rfq_key,
        &rfq,
        ctx.accounts.rfq_store.key(),
        &ctx.accounts.rfq_store,
    )?;
    validate_maker_store(
        rfq_key,
        &rfq,
        maker_id,
        ctx.accounts.maker_store.key(),
        &ctx.accounts.maker_store,
    )?;
    let (maker_authority, maker_bump) = maker_store_address(rfq_key, maker_id);
    require_keys_eq!(
        ctx.accounts.maker_store_authority.key(),
        maker_authority,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    for (side, mint) in [
        (&ctx.accounts.asset, rfq.asset_mint),
        (&ctx.accounts.basis, rfq.basis_mint),
    ] {
        require_keys_eq!(
            side.confidential_mint.key(),
            mint,
            ConfidentialRfqError::MintMismatch
        );
        side.validate(ctx.accounts.maker.key(), rfq_key)?;
    }
    let rfq_seeds = rfq_state_signer_seeds(&rfq, &rfq.nonce);
    let id_bytes = maker_id.to_le_bytes();
    let maker_seeds = maker_store_signer_seeds(&rfq_key, &id_bytes, &maker_bump);
    let signers: &[&[&[u8]]] = &[&rfq_seeds, &maker_seeds];

    // Both sides observe the original position. The second batch consumes it, so
    // repeated calls cannot pay again or decrement the active count twice.
    for asset in [true, false] {
        let side = if asset {
            &ctx.accounts.asset
        } else {
            &ctx.accounts.basis
        };
        let target = ct::balance_slot(side.confidential_mint.key(), side.rfq_token_account.key()).0;
        let execution = maker_payout_execution(
            &ctx.accounts.rfq_store,
            &ctx.accounts.maker_store,
            target,
            maker_id,
            ctx.accounts.maker.key(),
            asset,
        )?;
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
                ctx.accounts.maker_store.to_account_info(),
                side.rfq_balance_store.to_account_info(),
            ],
            [
                ctx.accounts.rfq.to_account_info(),
                ctx.accounts.maker_store_authority.to_account_info(),
            ],
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

/// Refund the unused quote side and replace the winning side's collateral with its
/// proceeds. A sell-only position is live; zeroing both quotes is the act-once guard.
#[inline(never)]
fn maker_payout_execution(
    account: &EncryptedStore,
    maker_account: &EncryptedStore,
    target: StoreId,
    maker_id: u32,
    maker_key: Pubkey,
    asset: bool,
) -> Result<ReturningFheExecution<Uint<64>>> {
    let store = Box::new(Store::new(account));
    let maker = Box::new(Store::new(maker_account));
    let input = Box::new(MakerPayoutInputs {
        state: store
            .get::<Uint<8>>(RFQPrivateField::State.key())
            .map_err(invalid_fhe)?,
        buyer: store
            .get::<Bool>(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)?,
        size: store
            .get::<Uint<64>>(RFQPrivateField::Size.key())
            .map_err(invalid_fhe)?,
        best: store
            .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
            .map_err(invalid_fhe)?,
        best_maker: store
            .get::<Uint<32>>(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)?,
        count: store
            .get::<Uint<32>>(RFQPrivateField::BidCount.key())
            .map_err(invalid_fhe)?,
        buy: maker
            .get::<Uint<64>>(MakerPrivateField::Buy.key())
            .map_err(invalid_fhe)?,
        sell: maker
            .get::<Uint<64>>(MakerPrivateField::Sell.key())
            .map_err(invalid_fhe)?,
        sequence: maker
            .get::<Uint<32>>(MakerPrivateField::Sequence.key())
            .map_err(invalid_fhe)?,
    });
    FheExecution::build_returning(
        store.id(),
        #[inline(never)]
        move |fhe| {
            // Compute proceeds/refunds from the original bid, then consume it once.
            let conditions = maker_claim_conditions(fhe, &input, maker_id)?;
            let values = maker_payout_amount(fhe, &input, &conditions, asset)?;
            fhe.output(values.payout, store.result().allow_transient(target))?;
            if !asset {
                finalize_maker_claim(fhe, &store, &maker, &input, &values, maker_key)?;
            }
            Ok(values.payout)
        },
    )
    .map_err(invalid_fhe)
}

/// Evaluate the private state and live-position predicates before payout arithmetic.
#[inline(never)]
fn maker_claim_conditions<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &MakerPayoutInputs,
    maker_id: u32,
) -> zama_fhe::Result<Box<MakerClaimConditions<'id>>> {
    let claimed = fhe.eq(input.state, Scalar::<Uint<8>>::u8(RFQState::Claimed as u8))?;
    let claimable = fhe.eq(
        input.state,
        Scalar::<Uint<8>>::u8(RFQState::Claimable as u8),
    )?;
    let canceled = fhe.eq(input.state, Scalar::<Uint<8>>::u8(RFQState::Canceled as u8))?;
    let invalid = fhe.eq(input.state, Scalar::<Uint<8>>::u8(RFQState::Invalid as u8))?;
    let terminal = fhe.or(claimed, canceled)?;
    let terminal = fhe.or(terminal, invalid)?;
    let settled = fhe.or(terminal, claimable)?;
    let has_buy = fhe.ne(input.buy, Scalar::<Uint<64>>::u64(0))?;
    let has_sell = fhe.ne(input.sell, Scalar::<Uint<64>>::u64(0))?;
    let live = fhe.or(has_buy, has_sell)?;
    let claim = fhe.and(settled, live)?;
    let winner = fhe.eq(input.best_maker, Scalar::<Uint<32>>::u32(maker_id))?;
    let zero = fhe.trivial_encrypt_u64(0)?;

    Ok(Box::new(MakerClaimConditions {
        claim,
        terminal,
        winner,
        has_sell,
        zero,
    }))
}

/// Calculate trade proceeds and unused collateral under the private claim predicates.
#[inline(never)]
fn maker_payout_amount<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &MakerPayoutInputs,
    conditions: &MakerClaimConditions<'id>,
    asset: bool,
) -> zama_fhe::Result<Box<MakerPayoutValues<'id>>> {
    let due = if asset {
        let collateral = fhe.if_then_else(conditions.has_sell, input.size, conditions.zero)?;
        let bought_and_refunded = fhe.add(input.size, collateral)?;
        let winning = fhe.if_then_else(input.buyer, conditions.zero, bought_and_refunded)?;
        fhe.if_then_else(conditions.winner, winning, collateral)?
    } else {
        let sold_and_refunded = fhe.add(input.best, input.buy)?;
        let purchase_change = fhe.sub(input.buy, input.best)?;
        let winning = fhe.if_then_else(input.buyer, sold_and_refunded, purchase_change)?;
        fhe.if_then_else(conditions.winner, winning, input.buy)?
    };
    let payout = fhe.if_then_else(conditions.claim, due, conditions.zero)?;
    Ok(Box::new(MakerPayoutValues {
        payout,
        claim: conditions.claim,
        terminal: conditions.terminal,
        zero: conditions.zero,
    }))
}

/// Clear both quote sides and decrement the count only for a live, eligible claim.
/// The same FHE batch seals the updated closure predicate after the final payout.
#[inline(never)]
fn finalize_maker_claim<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    store: &Store,
    maker: &Store,
    input: &MakerPayoutInputs,
    values: &MakerPayoutValues<'id>,
    maker_key: Pubkey,
) -> zama_fhe::Result<()> {
    let next_buy = fhe.if_then_else(values.claim, values.zero, input.buy)?;
    let next_sell = fhe.if_then_else(values.claim, values.zero, input.sell)?;
    let zero_count = fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(0))?;
    let one = fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(1))?;
    let decrement = fhe.if_then_else(values.claim, one, zero_count)?;
    let next_count = fhe.sub(input.count, decrement)?;
    let next_sequence = fhe.if_then_else(values.claim, zero_count, input.sequence)?;
    let empty = fhe.eq(next_count, Scalar::<Uint<32>>::u32(0))?;
    let can_close = fhe.and(values.terminal, empty)?;
    fhe.output(
        next_buy,
        maker.set(MakerPrivateField::Buy.key()).allow(maker_key),
    )?;
    fhe.output(
        next_sell,
        maker.set(MakerPrivateField::Sell.key()).allow(maker_key),
    )?;
    fhe.output(
        next_sequence,
        maker.set(MakerPrivateField::Sequence.key()).make_public(),
    )?;
    fhe.output(
        next_count,
        store.set(RFQPrivateField::BidCount.key()).make_public(),
    )?;
    fhe.output(
        can_close,
        store.set(RFQPrivateField::CanClose.key()).make_public(),
    )?;
    Ok(())
}

struct MakerClaimConditions<'id> {
    claim: Encrypted<'id, Bool>,
    terminal: Encrypted<'id, Bool>,
    winner: Encrypted<'id, Bool>,
    has_sell: Encrypted<'id, Bool>,
    zero: Encrypted<'id, Uint<64>>,
}

struct MakerPayoutValues<'id> {
    payout: Encrypted<'id, Uint<64>>,
    claim: Encrypted<'id, Bool>,
    terminal: Encrypted<'id, Bool>,
    zero: Encrypted<'id, Uint<64>>,
}

struct MakerPayoutInputs {
    state: FheHandle<Uint<8>>,
    buyer: FheHandle<Bool>,
    size: FheHandle<Uint<64>>,
    best: FheHandle<Uint<64>>,
    best_maker: FheHandle<Uint<32>>,
    count: FheHandle<Uint<32>>,
    buy: FheHandle<Uint<64>>,
    sell: FheHandle<Uint<64>>,
    sequence: FheHandle<Uint<32>>,
}
