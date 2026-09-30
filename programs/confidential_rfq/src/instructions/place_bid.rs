//! Submit or replace both confidential maker quotes and adjust their collateral.

use crate::{
    ConfidentialRfqError, CurrentAccountVersion,
    state::{
        market::Market,
        rfq::{
            MakerPrivateField, MakerStore, RFQ, RFQPrivateField, RFQState, RFQStore, invalid_fhe,
        },
    },
    util::{
        InputExt,
        bid_cpi::{
            BidContinuation, invoke_asset_bid_execution, invoke_returning_bid_execution,
            refund_collateral, transfer_deposit,
        },
        pda::{maker_store_signer_seeds, rfq_funder_signer_seeds, rfq_state_signer_seeds},
        rfq::{read_encrypted_store, validate_rfq_store},
        token_side::*,
    },
};
use anchor_lang::prelude::*;
use confidential_token as ct;
use confidential_token::program::ConfidentialToken;
use zama_fhe::{
    Bool, Encrypted, FheExecution, FheExecutionBuilder, FheHandle, ReturningFheExecution, Scalar,
    Store, StoreId, Uint,
};
use zama_host::{CoprocessorInputAttestation, EncryptedStore, program::ZamaHost};

#[derive(Accounts)]
#[instruction(maker_id: u32)]
pub struct PlaceBid<'info> {
    /// Active market maker and authority for deposits from their token accounts.
    pub maker: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub rfq: AccountLoader<'info, RFQ>,
    #[account(mut, seeds = [b"rfq_funder", rfq.key().as_ref()], bump = rfq.load()?.funder_bump,
        constraint = rfq_funder.data_is_empty() @ ConfidentialRfqError::InvalidRfqAccounts)]
    pub rfq_funder: SystemAccount<'info>,
    #[account(mut)]
    pub rfq_store: Box<Account<'info, EncryptedStore>>,
    /// CHECK: canonical RFQ/maker authority; signs every write to the maker store.
    #[account(seeds = [b"rfq_maker_store", rfq.key().as_ref(), &maker_id.to_le_bytes()], bump)]
    pub maker_authority: UncheckedAccount<'info>,
    /// CHECK: canonical host store, created on first use and validated in the handler.
    #[account(mut)]
    pub maker_store: UncheckedAccount<'info>,
    #[account(constraint = { asset.validate(maker.key(), rfq.key())?; true })]
    pub asset: TokenSide<'info>,
    #[account(constraint = { basis.validate(maker.key(), rfq.key())?; true })]
    pub basis: TokenSide<'info>,
    /// CHECK: validated by the confidential-token event CPI.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ConfidentialToken>,
    /// CHECK: validated by the host event CPI.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: shared transaction journal, validated by the host.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: Instructions sysvar, validated by the host.
    pub instructions: UncheckedAccount<'info>,
    /// CHECK: each confidential-token and host CPI validates the canonical config.
    pub host_config: UncheckedAccount<'info>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
    /// CHECK: optional RFQ application meter, validated by the host.
    #[account(mut)]
    pub hcu_block_meter: Option<UncheckedAccount<'info>>,
    /// CHECK: optional RFQ trust record, validated by the host.
    pub hcu_trusted_app_record: Option<UncheckedAccount<'info>>,
}

/// `prices` packs `(maker_buy << 64) | maker_sell`. Both prices are total basis
/// token amounts. Buy collateral is the buy quote; sell collateral is RFQ size.
pub fn place_bid<'info>(
    ctx: Context<'info, PlaceBid<'info>>,
    maker_id: u32,
    prices: Box<CoprocessorInputAttestation>,
    asset_transfer_attestation: Box<CoprocessorInputAttestation>,
    basis_transfer_attestation: Box<CoprocessorInputAttestation>,
) -> Result<()> {
    // Bind the public identities before creating stores or spending any funds.
    require!(maker_id != 0, ConfidentialRfqError::InvalidMakerId);
    let rfq = *ctx.accounts.rfq.load()?;
    let rfq_key = ctx.accounts.rfq.key();
    require_eq!(
        ctx.accounts.market.version,
        Market::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require_keys_eq!(
        rfq.market,
        ctx.accounts.market.key(),
        ConfidentialRfqError::MarketMismatch
    );
    require!(
        ctx.accounts.market.active_maker(maker_id) == Some(ctx.accounts.maker.key()),
        ConfidentialRfqError::UnauthorizedMaker
    );
    require_keys_eq!(
        rfq.asset_mint,
        ctx.accounts.asset.confidential_mint.key(),
        ConfidentialRfqError::MintMismatch
    );
    require_keys_eq!(
        rfq.basis_mint,
        ctx.accounts.basis.confidential_mint.key(),
        ConfidentialRfqError::MintMismatch
    );
    validate_rfq_store(
        rfq_key,
        &rfq,
        ctx.accounts.rfq_store.key(),
        &ctx.accounts.rfq_store,
    )?;
    prices.validate(ctx.accounts.maker.key(), crate::ID)?;
    asset_transfer_attestation.validate(ctx.accounts.maker.key(), ct::ID)?;
    basis_transfer_attestation.validate(ctx.accounts.maker.key(), ct::ID)?;
    let maker_bytes = maker_id.to_le_bytes();
    let rfq_seeds = rfq_state_signer_seeds(&rfq, &rfq.nonce);
    let maker_bump = ctx.bumps.maker_authority;
    let maker_seeds = maker_store_signer_seeds(&rfq_key, &maker_bytes, &maker_bump);
    let funder_seeds = rfq_funder_signer_seeds(&rfq_key, &rfq.funder_bump);
    let signers = BidSigners {
        rfq: &rfq_seeds,
        maker: &maker_seeds,
        funder: &funder_seeds,
    };
    let now = u64::try_from(Clock::get()?.unix_timestamp)
        .map_err(|_| error!(ConfidentialRfqError::InvalidRfqInput))?;

    // Store creation is public bookkeeping. An encrypted state or deadline check
    // never determines whether the native Solana instruction succeeds.
    let initialize = ctx.accounts.maker_store.data_is_empty();
    if initialize {
        create_maker_store(&ctx, &signers)?;
        let mut state = ctx.accounts.rfq.load_mut()?;
        state.open_stores = state
            .open_stores
            .checked_add(1)
            .ok_or(error!(ConfidentialRfqError::BidCountOverflow))?;
    }
    let maker_state = read_encrypted_store(&ctx.accounts.maker_store)?;
    // Anchor already binds the maker authority, and read_encrypted_store verifies
    // the host-owned store's canonical address using its recorded bump.
    require!(
        maker_state.program == crate::ID
            && maker_state.authority == ctx.accounts.maker_authority.key()
            && maker_state.scope == rfq.nonce,
        ConfidentialRfqError::RfqStoreMismatch
    );

    // Each side independently accepts a fully funded quote or retains its old
    // quote. The asset phase verifies and shares packed prices; the basis phase
    // uses the original sell activity to update count and submission priority.
    // Empty maker fields
    // are encrypted zero until these two phases initialize all three fields.
    let rfq_store = RFQStore(Store::new(&ctx.accounts.rfq_store));
    let continuation = adjust_collateral(
        &ctx,
        &rfq_store,
        &maker_state,
        BidPrices::Attested(prices),
        *asset_transfer_attestation,
        true,
        now,
        &signers,
    )?;
    let maker_state = read_encrypted_store(&ctx.accounts.maker_store)?;
    adjust_collateral(
        &ctx,
        &rfq_store,
        &maker_state,
        BidPrices::Granted(continuation),
        *basis_transfer_attestation,
        false,
        now,
        &signers,
    )?;

    Ok(())
}

/// Sign only with the authorities required by each CPI.
struct BidSigners<'a> {
    rfq: &'a [&'a [u8]],
    maker: &'a [&'a [u8]],
    funder: &'a [&'a [u8]],
}

/// Create the maker dictionary using the RFQ's prefunded payer.
#[inline(never)]
fn create_maker_store<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    signers: &BidSigners<'_>,
) -> Result<()> {
    zama_host::cpi::create_encrypted_store(
        CpiContext::new_with_signer(
            ctx.accounts.zama_program.key(),
            zama_host::cpi::accounts::CreateEncryptedStore {
                payer: ctx.accounts.rfq_funder.to_account_info(),
                authority: ctx.accounts.maker_authority.to_account_info(),
                encrypted_store: ctx.accounts.maker_store.to_account_info(),
                host_config: ctx.accounts.host_config.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
            },
            &[signers.maker, signers.funder],
        ),
        zama_host::instructions::CreateEncryptedStoreArgs {
            program: crate::ID,
            scope: ctx.accounts.rfq_store.scope,
            authority_seeds: signers.maker.iter().map(|seed| seed.to_vec()).collect(),
        },
    )?;
    Ok(())
}

/// Prices are attested once, then shared with the original sell activity.
enum BidPrices {
    Attested(Box<CoprocessorInputAttestation>),
    Granted(BidContinuation),
}

/// Transfer the maker's attested deposit, accept only the exact required delta,
/// and refund decreases or the entire deposit when the attempted change fails.
#[inline(never)]
fn adjust_collateral<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    rfq_store: &RFQStore<'_>,
    maker_state: &EncryptedStore,
    prices: BidPrices,
    transfer_attestation: CoprocessorInputAttestation,
    asset: bool,
    now: u64,
    signers: &BidSigners<'_>,
) -> Result<BidContinuation> {
    let supplied_continuation = match &prices {
        BidPrices::Attested(_) => None,
        BidPrices::Granted(continuation) => Some(*continuation),
    };
    let transferred = transfer_deposit(ctx, transfer_attestation, asset, &[signers.funder])?;
    // Deposits and the asset phase only grant results to the RFQ; its stored
    // fields remain unchanged until the basis phase commits count and sequence.
    let side = if asset {
        &ctx.accounts.asset
    } else {
        &ctx.accounts.basis
    };
    let refund_target =
        ct::balance_slot(side.confidential_mint.key(), side.rfq_token_account.key()).0;
    let execution = build_collateral_adjustment(
        rfq_store,
        maker_state,
        prices,
        asset,
        now,
        ctx.accounts.maker.key(),
        refund_target,
        transferred,
    )?;
    let execution_signers = &[signers.rfq, signers.maker, signers.funder];
    let (refund_handle, continuation) = if let Some(continuation) = supplied_continuation {
        (
            invoke_returning_bid_execution(ctx, execution, execution_signers)?,
            continuation,
        )
    } else {
        invoke_asset_bid_execution(ctx, execution, execution_signers)?
    };
    refund_collateral(ctx, refund_handle, asset, &[signers.rfq, signers.funder])?;
    Ok(continuation)
}

/// Heap-owned operands keep each collateral circuit within the SBF stack limit.
struct CollateralInputs {
    state: FheHandle<Uint<8>>,
    expiry: FheHandle<Uint<64>>,
    sequence: FheHandle<Uint<32>>,
    maker_sequence: Option<FheHandle<Uint<32>>>,
    count: FheHandle<Uint<32>>,
    size: FheHandle<Uint<64>>,
    old_quote: Option<FheHandle<Uint<64>>>,
    other_quote: Option<FheHandle<Uint<64>>>,
    previous_sell_positive: Option<FheHandle<Bool>>,
    actual: FheHandle<Uint<64>>,
}

/// Accept a side only if its attested transfer matches the required increase.
/// Failed, expired, and malformed changes refund the entire actual transfer.
/// The basis phase commits one active-count transition across both side updates.
#[inline(never)]
fn build_collateral_adjustment(
    rfq: &RFQStore<'_>,
    maker_state: &EncryptedStore,
    prices: BidPrices,
    asset: bool,
    now: u64,
    maker: Pubkey,
    transfer_target: StoreId,
    transferred: [u8; 32],
) -> Result<ReturningFheExecution<Uint<64>>> {
    let bid = Box::new(MakerStore(Store::new(maker_state)));
    // Keep the typed handle operands out of the SBF stack frame. Each handle
    // includes its store provenance, so a phase's inputs are larger than hashes.
    let inputs = Box::new(CollateralInputs {
        state: rfq.state()?,
        expiry: rfq.expire_timestamp()?,
        sequence: rfq.bid_seq()?,
        maker_sequence: maker_state
            .get(&MakerPrivateField::Sequence.key())
            .map(|_| bid.sequence())
            .transpose()?,
        count: rfq.bid_count()?,
        size: rfq.size()?,
        old_quote: maker_state
            .get(
                &if asset {
                    MakerPrivateField::Sell
                } else {
                    MakerPrivateField::Buy
                }
                .key(),
            )
            .map(|_| if asset { bid.sell() } else { bid.buy() })
            .transpose()?,
        other_quote: maker_state
            .get(
                &if asset {
                    MakerPrivateField::Buy
                } else {
                    MakerPrivateField::Sell
                }
                .key(),
            )
            .map(|_| if asset { bid.buy() } else { bid.sell() })
            .transpose()?,
        previous_sell_positive: match &prices {
            BidPrices::Attested(_) => None,
            BidPrices::Granted(continuation) => Some(
                rfq.granted::<Bool>(continuation.previous_sell_positive)
                    .map_err(invalid_fhe)?,
            ),
        },
        actual: rfq.granted::<Uint<64>>(transferred).map_err(invalid_fhe)?,
    });
    FheExecution::build_returning(
        rfq.id(),
        #[inline(never)]
        move |fhe| {
            // Keep the three circuit phases in separate SBF stack frames while
            // appending to one builder, so this remains a single host execution.
            let prices = match prices {
                BidPrices::Attested(attestation) => {
                    let input = fhe.verified_input::<Uint<128>>(*attestation)?;
                    // The checked journal reader takes this first produced result.
                    let prices = fhe.or(input, Scalar::<Uint<128>>::u128(0))?;
                    prices
                }
                BidPrices::Granted(continuation) => {
                    rfq.granted::<Uint<128>>(continuation.prices)?.into()
                }
            };
            let terms = collateral_terms(fhe, &inputs, &prices, asset, now)?;
            let outcome = collateral_outcome(fhe, &inputs, &terms)?;
            let field = if asset {
                MakerPrivateField::Sell
            } else {
                MakerPrivateField::Buy
            };
            fhe.output(outcome.accepted, bid.set(field.key()).allow(maker))?;
            fhe.output(
                outcome.refund,
                rfq.result().allow_transient(transfer_target),
            )?;
            if asset {
                // The checked journal reader takes this final produced result.
                // Every produced result is intrinsically available to its RFQ
                // producer, so these continuation values need no self-grants.
                fhe.or(terms.old_positive, Scalar::<Bool>::bool(false))?;
            } else {
                let next_count = active_bid_count(fhe, &inputs, &terms, &outcome)?;
                fhe.output(
                    next_count,
                    rfq.set(RFQPrivateField::BidCount.key()).make_public(),
                )?;
                advance_bid_sequence(fhe, &inputs, &terms, rfq, &bid)?;
            }
            Ok(outcome.refund)
        },
    )
    .map_err(invalid_fhe)
}

/// Produced operands shared between the collateral phases of one FHE execution.
struct CollateralTerms<'id> {
    quote: Encrypted<'id, Uint<64>>,
    old_quote: Encrypted<'id, Uint<64>>,
    can_change: Encrypted<'id, Bool>,
    advance_sequence: Encrypted<'id, Bool>,
    zero: Encrypted<'id, Uint<64>>,
    old_positive: Encrypted<'id, Bool>,
    old_collateral: Encrypted<'id, Uint<64>>,
    next_collateral: Encrypted<'id, Uint<64>>,
}

struct CollateralOutcome<'id> {
    accepted: Encrypted<'id, Uint<64>>,
    refund: Encrypted<'id, Uint<64>>,
}

/// Interpret the quote and privately require a live RFQ, an unelapsed deadline,
/// and sequence capacity. Sell quote prices never determine asset collateral.
#[inline(never)]
fn collateral_terms<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    inputs: &CollateralInputs,
    prices: &Encrypted<'id, Uint<128>>,
    asset: bool,
    now: u64,
) -> zama_fhe::Result<Box<CollateralTerms<'id>>> {
    let quote = if asset {
        fhe.cast::<Uint<128>, Uint<64>>(*prices)?
    } else {
        let high = fhe.shr(*prices, Scalar::<Uint<128>>::u128(64))?;
        fhe.cast::<Uint<128>, Uint<64>>(high)?
    };
    let valid = fhe.eq(inputs.state, Scalar::<Uint<8>>::u8(RFQState::Valid as u8))?;
    let before_expiry = fhe.gt(inputs.expiry, Scalar::<Uint<64>>::u64(now))?;
    let sequence_available = fhe.lt(inputs.sequence, Scalar::<Uint<32>>::u32(u32::MAX))?;
    let advance_sequence = fhe.and(valid, sequence_available)?;
    let can_change = fhe.and(advance_sequence, before_expiry)?;
    let zero = fhe.trivial_encrypt_u64(0)?;
    let old_quote = inputs.old_quote.map(Encrypted::from).unwrap_or(zero);
    let old_positive = fhe.gt(old_quote, Scalar::<Uint<64>>::u64(0))?;
    let (old_collateral, next_collateral) = if asset {
        let quoted = fhe.gt(quote, Scalar::<Uint<64>>::u64(0))?;
        (
            fhe.if_then_else(old_positive, inputs.size, zero)?,
            fhe.if_then_else(quoted, inputs.size, zero)?,
        )
    } else {
        (old_quote, quote)
    };
    Ok(Box::new(CollateralTerms {
        quote,
        old_quote,
        can_change,
        advance_sequence,
        zero,
        old_positive,
        old_collateral,
        next_collateral,
    }))
}

/// Accept only an exact collateral increase. Otherwise preserve the old quote
/// and return everything actually transferred; valid decreases return excess.
#[inline(never)]
fn collateral_outcome<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    inputs: &CollateralInputs,
    terms: &CollateralTerms<'id>,
) -> zama_fhe::Result<Box<CollateralOutcome<'id>>> {
    let increases = fhe.gt(terms.next_collateral, terms.old_collateral)?;
    let increase = fhe.sub(terms.next_collateral, terms.old_collateral)?;
    let deposit = fhe.if_then_else(increases, increase, terms.zero)?;
    let fully_funded = fhe.eq(inputs.actual, deposit)?;
    let accept = fhe.and(terms.can_change, fully_funded)?;
    let accepted = fhe.if_then_else(accept, terms.quote, terms.old_quote)?;
    // Return the deposit plus previous collateral less the collateral retained.
    // Exact funding makes accepted increases return zero; failed changes retain
    // old collateral and return the full deposit, including after modular add.
    let retained = fhe.if_then_else(accept, terms.next_collateral, terms.old_collateral)?;
    let available = fhe.add(inputs.actual, terms.old_collateral)?;
    let refund = fhe.sub(available, retained)?;
    Ok(Box::new(CollateralOutcome { accepted, refund }))
}

/// Replace the maker's contribution once, after both collateral phases. The
/// transient flag records its old sell activity; the stored sell quote is final.
#[inline(never)]
fn active_bid_count<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    inputs: &CollateralInputs,
    terms: &CollateralTerms<'id>,
    outcome: &CollateralOutcome<'id>,
) -> zama_fhe::Result<Encrypted<'id, Uint<32>>> {
    let other_quote = inputs
        .other_quote
        .map(Encrypted::from)
        .unwrap_or(terms.zero);
    let other_positive = fhe.gt(other_quote, Scalar::<Uint<64>>::u64(0))?;
    let previous_sell_positive = inputs
        .previous_sell_positive
        .ok_or(zama_fhe::FheExecutionBuildError::MissingStoreSlot)?;
    let old_active = fhe.or(terms.old_positive, previous_sell_positive)?;
    let accepted_positive = fhe.gt(outcome.accepted, Scalar::<Uint<64>>::u64(0))?;
    let next_active = fhe.or(accepted_positive, other_positive)?;
    let old_active = fhe.cast::<Bool, Uint<32>>(old_active)?;
    let next_active = fhe.cast::<Bool, Uint<32>>(next_active)?;
    let without_old = fhe.sub(inputs.count, old_active)?;
    fhe.add(without_old, next_active)
}

/// Finish the submission's ordering in the basis batch, after both sides are
/// accepted. Failed funding and late submissions still advance a Valid RFQ's
/// sequence; saturating at u32::MAX prevents priority collisions from wrapping.
#[inline(never)]
fn advance_bid_sequence<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    inputs: &CollateralInputs,
    terms: &CollateralTerms<'id>,
    rfq: &RFQStore<'_>,
    maker: &MakerStore<'_>,
) -> zama_fhe::Result<()> {
    let incremented = fhe.add(inputs.sequence, Scalar::<Uint<32>>::u32(1))?;
    let next = fhe.if_then_else(terms.advance_sequence, incremented, inputs.sequence)?;
    let previous = if let Some(previous) = inputs.maker_sequence {
        previous.into()
    } else {
        fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(0))?
    };
    let maker_next = fhe.if_then_else(terms.advance_sequence, next, previous)?;
    fhe.output(next, rfq.set(RFQPrivateField::BidSeq.key()).make_public())?;
    fhe.output(
        maker_next,
        maker.set(MakerPrivateField::Sequence.key()).make_public(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zama_host::EncryptedSlot;
    use zama_solana_test_kit::handle_for_chain;

    fn store(authority: Pubkey, fields: &[([u8; 32], u8)]) -> EncryptedStore {
        EncryptedStore {
            program: crate::ID,
            authority,
            scope: [4; 32],
            slots: fields
                .iter()
                .enumerate()
                .map(|(i, (key, fhe_type))| EncryptedSlot {
                    key: *key,
                    handle: handle_for_chain(i as u8 + 1, *fhe_type),
                })
                .collect(),
            leaf_count: fields.len() as u64,
            peaks: Vec::new(),
            bump: 0,
        }
    }

    #[test]
    fn collateral_circuits_fit_host_execution_limits() {
        let rfq = store(
            Pubkey::new_unique(),
            &[
                (RFQPrivateField::State.key(), 2),
                (RFQPrivateField::ExpireTimestamp.key(), 5),
                (RFQPrivateField::BidSeq.key(), 4),
                (RFQPrivateField::BidCount.key(), 4),
                (RFQPrivateField::Size.key(), 5),
            ],
        );
        let maker = store(
            Pubkey::new_unique(),
            &[
                (MakerPrivateField::Buy.key(), 5),
                (MakerPrivateField::Sell.key(), 5),
                (MakerPrivateField::Sequence.key(), 4),
            ],
        );
        let prices = handle_for_chain(100, 6);
        let transferred = handle_for_chain(101, 5);
        let empty_maker = store(Pubkey::new_unique(), &[]);
        for (initialized, maker) in [(true, &maker), (false, &empty_maker)] {
            for asset in [true, false] {
                let source = if asset {
                    BidPrices::Attested(Box::new(CoprocessorInputAttestation {
                        input_handle: prices,
                        ct_handles: vec![prices],
                        handle_index: 0,
                        user_address: Pubkey::new_unique().to_bytes(),
                        contract_address: crate::ID.to_bytes(),
                        contract_chain_id: 1,
                        extra_data: Vec::new(),
                        signatures: vec![[0; 65]],
                    }))
                } else {
                    BidPrices::Granted(BidContinuation {
                        prices,
                        previous_sell_positive: handle_for_chain(242, 0),
                    })
                };
                let execution = build_collateral_adjustment(
                    &RFQStore(Store::new(&rfq)),
                    maker,
                    source,
                    asset,
                    100,
                    Pubkey::new_unique(),
                    StoreId::new(ct::ID, Pubkey::new_unique(), [0; 32]),
                    transferred,
                )
                .unwrap();
                let cost = execution.execution().cost();
                eprintln!("initialized={initialized} asset={asset} cost={cost:?}");
                assert!(cost.steps <= zama_host::MAX_FHE_EXECUTION_STEPS);
            }
        }
    }
    #[test]
    fn three_attestation_bid_fits_v0_transaction_with_lookup_table() {
        use anchor_lang::{InstructionData, ToAccountMetas};
        use solana_sdk::{
            hash::Hash,
            instruction::Instruction,
            message::{AddressLookupTableAccount, v0},
        };
        let maker = Pubkey::new_unique();
        let next = Pubkey::new_unique;
        let token_side = || crate::accounts::TokenSide {
            confidential_mint: next(),
            underlying_mint: next(),
            participant_ata: next(),
            rfq_ata: next(),
            participant_token_account: next(),
            rfq_token_account: next(),
            participant_balance_store: next(),
            rfq_balance_store: next(),
        };
        let proof = |program: Pubkey| CoprocessorInputAttestation {
            input_handle: [1; 32],
            ct_handles: vec![[1; 32]],
            handle_index: 0,
            user_address: maker.to_bytes(),
            contract_address: program.to_bytes(),
            contract_chain_id: 1,
            extra_data: Vec::new(),
            signatures: vec![[0; 65]],
        };
        for fee_payer in [maker, next()] {
            let accounts = crate::accounts::PlaceBid {
                maker,
                market: next(),
                rfq: next(),
                rfq_funder: next(),
                rfq_store: next(),
                maker_authority: next(),
                maker_store: next(),
                asset: token_side(),
                basis: token_side(),
                confidential_token_event_authority: next(),
                confidential_token_program: ct::ID,
                zama_event_authority: next(),
                transient_store: zama_host::transient_store_address(fee_payer).0,
                instructions: solana_sdk::sysvar::instructions::ID,
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
                    prices: proof(crate::ID).into(),
                    asset_transfer_attestation: proof(ct::ID).into(),
                    basis_transfer_attestation: proof(ct::ID).into(),
                }
                .data(),
            };
            let envelope =
                zama_solana_test_kit::transaction::fhe_transaction(fee_payer, [instruction]);
            let mut addresses: Vec<_> = envelope
                .iter()
                .flat_map(|ix| ix.accounts.iter())
                .filter(|meta| !meta.is_signer)
                .map(|meta| meta.pubkey)
                .collect();
            addresses.sort_unstable();
            addresses.dedup();
            let lookup = AddressLookupTableAccount {
                key: next(),
                addresses,
            };
            let message =
                v0::Message::try_compile(&fee_payer, &envelope, &[lookup], Hash::default())
                    .unwrap();
            let wire_size = 1
                + 64 * usize::from(message.header.num_required_signatures)
                + message.serialize().len();
            assert!(
                wire_size <= 1_232,
                "three-proof bid wire size is {wire_size} bytes"
            );
            eprintln!(
                "bid has {} signatures and {wire_size} wire bytes",
                message.header.num_required_signatures
            );
        }
    }
}
