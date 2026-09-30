//! Scan a maker's accepted quote once and deterministically settle the auction winner.

use anchor_lang::prelude::*;
use zama_fhe::{
    Bool, Encrypted, ExecutionCpiAccounts, FheExecution, FheExecutionBuilder, FheHandle,
    ReturningFheExecution, Scalar, Store, Uint,
};
use zama_host::{EncryptedStore, program::ZamaHost};

use crate::{
    ConfidentialRfqError,
    state::rfq::{MakerPrivateField, RFQ, RFQPrivateField, RFQState, invalid_fhe},
    util::{
        cpi,
        pda::{maker_store_address, maker_store_signer_seeds, rfq_state_signer_seeds},
        rfq::{validate_maker_store, validate_rfq_store},
    },
};

#[derive(Accounts)]
pub struct CalculateWinner<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,
    pub rfq: AccountLoader<'info, RFQ>,
    #[account(mut)]
    pub rfq_store: Box<Account<'info, EncryptedStore>>,
    /// CHECK: validated against the maker's RFQ-specific authority PDA in the handler.
    pub maker_store_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub maker_store: Box<Account<'info, EncryptedStore>>,
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

pub fn calculate_winner<'info>(
    ctx: Context<'info, CalculateWinner<'info>>,
    maker_id: u32,
) -> Result<()> {
    let rfq_key = ctx.accounts.rfq.key();
    let rfq = *ctx.accounts.rfq.load()?;
    require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
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
    let rfq_seeds = rfq_state_signer_seeds(&rfq, &rfq.nonce);
    let id_bytes = maker_id.to_le_bytes();
    let maker_seeds = maker_store_signer_seeds(&rfq_key, &id_bytes, &maker_bump);
    // Price selection stays below the host's 32-step execution limit and only
    // grants a private replacement flag. The commit phase consumes it atomically.
    let selection = winner_selection_execution(&ctx.accounts.rfq_store, &ctx.accounts.maker_store)?;
    let replace_handle = cpi::invoke_returning(
        selection,
        ctx.accounts.execution_accounts(ctx.remaining_accounts),
        [
            ctx.accounts.rfq_store.to_account_info(),
            ctx.accounts.maker_store.to_account_info(),
        ],
        [
            ctx.accounts.rfq.to_account_info(),
            ctx.accounts.maker_store_authority.to_account_info(),
        ],
        &[&rfq_seeds, &maker_seeds],
    )?;
    let execution = winner_commit_execution(
        &ctx.accounts.rfq_store,
        &ctx.accounts.maker_store,
        maker_id,
        replace_handle,
    )?;
    cpi::invoke(
        execution,
        ctx.accounts.execution_accounts(ctx.remaining_accounts),
        [
            ctx.accounts.rfq_store.to_account_info(),
            ctx.accounts.maker_store.to_account_info(),
        ],
        [
            ctx.accounts.rfq.to_account_info(),
            ctx.accounts.maker_store_authority.to_account_info(),
        ],
        &[&rfq_seeds, &maker_seeds],
    )
}

impl<'info> CalculateWinner<'info> {
    /// Shared CPI plumbing for the two batches, funded by the submitting caller.
    fn execution_accounts(&self, remaining: &[AccountInfo<'info>]) -> ExecutionCpiAccounts<'info> {
        ExecutionCpiAccounts {
            payer: self.caller.to_account_info(),
            authority: self.rfq.to_account_info(),
            host_config: self.host_config.to_account_info(),
            deny_scope_records: remaining.to_vec(),
            system_program: self.system_program.to_account_info(),
            hcu_block_meter: None,
            hcu_trusted_app_record: None,
            rand_nonce: None,
            event_authority: self.zama_event_authority.to_account_info(),
            transient_store: self.transient_store.to_account_info(),
            instructions: self.instructions.to_account_info(),
            program: self.zama_program.to_account_info(),
        }
    }
}

/// Select a better eligible quote, including the first nonzero quote and earlier
/// sequence on ties. This phase does not change either persistent store.
#[inline(never)]
fn winner_selection_execution(
    account: &EncryptedStore,
    maker_account: &EncryptedStore,
) -> Result<ReturningFheExecution<Bool>> {
    let store = Box::new(Store::new(account));
    let maker = Box::new(Store::new(maker_account));
    let input = Box::new(WinnerSelectionInputs {
        buyer: store
            .get::<Bool>(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)?,
        limit: store
            .get::<Uint<64>>(RFQPrivateField::OfferLimit.key())
            .map_err(invalid_fhe)?,
        best: store
            .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
            .map_err(invalid_fhe)?,
        best_maker: store
            .get::<Uint<32>>(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)?,
        best_index: store
            .get::<Uint<32>>(RFQPrivateField::BestMakerIndex.key())
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
            let offer = fhe.if_then_else(input.buyer, input.sell, input.buy)?;
            let positive = fhe.ne(offer, Scalar::<Uint<64>>::u64(0))?;
            let below_limit = fhe.lt(offer, input.limit)?;
            let above_limit = fhe.gt(offer, input.limit)?;
            let meets_limit = fhe.if_then_else(input.buyer, below_limit, above_limit)?;
            let eligible = fhe.and(positive, meets_limit)?;
            let lower = fhe.lt(offer, input.best)?;
            let higher = fhe.gt(offer, input.best)?;
            let better_price = fhe.if_then_else(input.buyer, lower, higher)?;
            let equal_price = fhe.eq(offer, input.best)?;
            let earlier = fhe.lt(input.sequence, input.best_index)?;
            let wins_tie = fhe.and(equal_price, earlier)?;
            let improves = fhe.or(better_price, wins_tie)?;
            let first = fhe.eq(input.best_maker, Scalar::<Uint<32>>::u32(0))?;
            let improves = fhe.or(first, improves)?;
            let replace = fhe.and(eligible, improves)?;
            fhe.output(replace, store.result().allow_transient(store.id()))?;
            Ok(replace)
        },
    )
    .map_err(invalid_fhe)
}

/// Consume the sequence once for each active bid and persist the selected winner.
/// Only an expired RFQ can advance; finalizing the last bid unlocks user claims.
#[inline(never)]
fn winner_commit_execution(
    account: &EncryptedStore,
    maker_account: &EncryptedStore,
    maker_id: u32,
    replacement: [u8; 32],
) -> Result<FheExecution> {
    let store = Box::new(Store::new(account));
    let maker = Box::new(Store::new(maker_account));
    let input = Box::new(WinnerCommitInputs {
        state: store
            .get::<Uint<8>>(RFQPrivateField::State.key())
            .map_err(invalid_fhe)?,
        buyer: store
            .get::<Bool>(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)?,
        best: store
            .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
            .map_err(invalid_fhe)?,
        best_maker: store
            .get::<Uint<32>>(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)?,
        best_index: store
            .get::<Uint<32>>(RFQPrivateField::BestMakerIndex.key())
            .map_err(invalid_fhe)?,
        searched: store
            .get::<Uint<32>>(RFQPrivateField::SearchedBids.key())
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
        replacement: store.granted::<Bool>(replacement).map_err(invalid_fhe)?,
    });
    FheExecution::build(
        store.id(),
        #[inline(never)]
        move |fhe| {
            // Keep arithmetic and effect assembly in separate bounded SBF frames.
            let scanned = winner_scan_values(fhe, &input)?;
            let values = winner_commit_values(fhe, &input, &scanned, maker_id)?;
            write_winner_commit(fhe, &store, &maker, &values)
        },
    )
    .map_err(invalid_fhe)
}

/// Consume only active, unsearched bids and carry their private scan predicates forward.
#[inline(never)]
fn winner_scan_values<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &WinnerCommitInputs,
) -> zama_fhe::Result<Box<WinnerScanValues<'id>>> {
    let expired = fhe.eq(input.state, Scalar::<Uint<8>>::u8(RFQState::Expired as u8))?;
    let has_buy = fhe.ne(input.buy, Scalar::<Uint<64>>::u64(0))?;
    let has_sell = fhe.ne(input.sell, Scalar::<Uint<64>>::u64(0))?;
    let live = fhe.or(has_buy, has_sell)?;
    let unsearched = fhe.ne(input.sequence, Scalar::<Uint<32>>::u32(0))?;
    let scan = fhe.and(live, unsearched)?;
    let scan = fhe.and(expired, scan)?;
    let zero = fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(0))?;
    let one = fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(1))?;
    let increment = fhe.if_then_else(scan, one, zero)?;
    let next_searched = fhe.add(input.searched, increment)?;
    let next_sequence = fhe.if_then_else(scan, zero, input.sequence)?;
    Ok(Box::new(WinnerScanValues {
        expired,
        scan,
        next_searched,
        next_sequence,
    }))
}

/// Apply the eligible winning quote and detect completion after this scan.
#[inline(never)]
fn winner_commit_values<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &WinnerCommitInputs,
    scanned: &WinnerScanValues<'id>,
    maker_id: u32,
) -> zama_fhe::Result<Box<WinnerCommitValues<'id>>> {
    let replace = fhe.and(scanned.scan, input.replacement)?;
    let offer = fhe.if_then_else(input.buyer, input.sell, input.buy)?;
    let id = fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(maker_id))?;
    let next_best = fhe.if_then_else(replace, offer, input.best)?;
    let next_maker = fhe.if_then_else(replace, id, input.best_maker)?;
    let next_index = fhe.if_then_else(replace, input.sequence, input.best_index)?;
    let finished = fhe.eq(scanned.next_searched, input.count)?;
    let finished = fhe.and(scanned.expired, finished)?;
    let claimable = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(RFQState::Claimable as u8))?;
    let next_state = fhe.if_then_else(finished, claimable, input.state)?;
    Ok(Box::new(WinnerCommitValues {
        next_searched: scanned.next_searched,
        next_sequence: scanned.next_sequence,
        next_best,
        next_maker,
        next_index,
        next_state,
    }))
}

/// Publish only the counters/state while keeping the winning price and identity private.
#[inline(never)]
fn write_winner_commit<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    store: &Store,
    maker: &Store,
    values: &WinnerCommitValues<'id>,
) -> zama_fhe::Result<()> {
    fhe.output(
        values.next_searched,
        store.set(RFQPrivateField::SearchedBids.key()).make_public(),
    )?;
    fhe.output(
        values.next_sequence,
        maker.set(MakerPrivateField::Sequence.key()).make_public(),
    )?;
    fhe.output(
        values.next_best,
        store.set(RFQPrivateField::BestOffer.key()),
    )?;
    fhe.output(
        values.next_maker,
        store.set(RFQPrivateField::BestMaker.key()),
    )?;
    fhe.output(
        values.next_index,
        store.set(RFQPrivateField::BestMakerIndex.key()),
    )?;
    fhe.output(
        values.next_state,
        store.set(RFQPrivateField::State.key()).make_public(),
    )?;
    Ok(())
}

struct WinnerScanValues<'id> {
    expired: Encrypted<'id, Bool>,
    scan: Encrypted<'id, Bool>,
    next_searched: Encrypted<'id, Uint<32>>,
    next_sequence: Encrypted<'id, Uint<32>>,
}

struct WinnerCommitValues<'id> {
    next_searched: Encrypted<'id, Uint<32>>,
    next_sequence: Encrypted<'id, Uint<32>>,
    next_best: Encrypted<'id, Uint<64>>,
    next_maker: Encrypted<'id, Uint<32>>,
    next_index: Encrypted<'id, Uint<32>>,
    next_state: Encrypted<'id, Uint<8>>,
}

struct WinnerSelectionInputs {
    buyer: FheHandle<Bool>,
    limit: FheHandle<Uint<64>>,
    best: FheHandle<Uint<64>>,
    best_maker: FheHandle<Uint<32>>,
    best_index: FheHandle<Uint<32>>,
    buy: FheHandle<Uint<64>>,
    sell: FheHandle<Uint<64>>,
    sequence: FheHandle<Uint<32>>,
}

struct WinnerCommitInputs {
    state: FheHandle<Uint<8>>,
    buyer: FheHandle<Bool>,
    best: FheHandle<Uint<64>>,
    best_maker: FheHandle<Uint<32>>,
    best_index: FheHandle<Uint<32>>,
    searched: FheHandle<Uint<32>>,
    count: FheHandle<Uint<32>>,
    buy: FheHandle<Uint<64>>,
    sell: FheHandle<Uint<64>>,
    sequence: FheHandle<Uint<32>>,
    replacement: FheHandle<Bool>,
}
