//! Settle one maker's bid after the public RFQ timeout.
//!
//! Failed bids are refunded and counted closed during placement. A live bid has
//! a positive MakerBuy value; setting MakerBuy and MakerSell to zero on claim is
//! the encrypted act-once marker. Both payouts use transient grants to the
//! confidential token balance Stores, so no payout slots are left in the RFQ.

use crate::{
    ConfidentialRfqError, CurrentAccountVersion,
    instructions::place_bid::BidReceipt,
    state::rfq::{RFQ, RFQPrivateField},
    util::{
        cpi,
        pda::bid_receipt_seeds,
        token_side::{
            __client_accounts_token_side, __cpi_client_accounts_token_side, TokenSide,
            TokenSideBumps,
        },
    },
};
use anchor_lang::prelude::*;
use confidential_token as ct;
use std::num::NonZeroU64;
use zama_fhe::{
    Bool, Encrypted, ExecutionCpiAccounts, FheExecution, FheExecutionBuilder, FheHandle, Scalar,
    Store, StoreId, Uint,
};
use zama_host::program::ZamaHost;

#[derive(Accounts)]
#[instruction(maker_id: u64, bid_index: u64)]
pub struct ClaimRfqMaker<'info> {
    /// Only the bidder may claim, including after market removal.
    #[account(mut)]
    pub maker: Signer<'info>,
    pub rfq: AccountLoader<'info, RFQ>,
    /// CHECK: verified against the nonce-bound RFQ PDA and stored bump.
    pub rfq_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        close = maker,
        seeds = [
            bid_receipt_seeds(&rfq.key(), &bid_index.to_le_bytes())[0],
            bid_receipt_seeds(&rfq.key(), &bid_index.to_le_bytes())[1],
            bid_receipt_seeds(&rfq.key(), &bid_index.to_le_bytes())[2],
        ],
        bump = bid_receipt.bump,
    )]
    pub bid_receipt: Box<Account<'info, BidReceipt>>,
    #[account(mut)]
    pub rfq_store: Box<Account<'info, zama_host::EncryptedStore>>,

    pub asset: TokenSide<'info>,
    pub basis: TokenSide<'info>,

    /// CHECK: verified by the token event CPI.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ct::program::ConfidentialToken>,
    /// CHECK: verified by the host event CPI.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: shared journal verified by the host and token program.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: instructions sysvar verified by the host and token program.
    pub instructions: UncheckedAccount<'info>,
    pub host_config: Box<Account<'info, zama_host::HostConfig>>,
    pub zama_program: Program<'info, ZamaHost>,
    pub system_program: Program<'info, System>,
    /// CHECK: optional RFQ meter, validated by the host.
    #[account(mut)]
    pub hcu_block_meter: Option<UncheckedAccount<'info>>,
    /// CHECK: optional RFQ trust record, validated by the host.
    pub hcu_trusted_app_record: Option<UncheckedAccount<'info>>,
}

pub fn claim_rfq_maker<'info>(
    ctx: Context<'info, ClaimRfqMaker<'info>>,
    maker_id: u64,
    bid_index: u64,
) -> Result<()> {
    let id = NonZeroU64::new(maker_id).ok_or(error!(ConfidentialRfqError::InvalidMakerId))?;
    let rfq_key = ctx.accounts.rfq.key();
    let maker = ctx.accounts.maker.key();
    let (version, bump, bid_count, timeout, asset_mint, basis_mint) = {
        let rfq = ctx.accounts.rfq.load()?;
        (
            rfq.version,
            rfq.authority_bump,
            rfq.bid_count,
            rfq.timeout,
            rfq.asset_mint,
            rfq.basis_mint,
        )
    };
    require_eq!(
        version,
        RFQ::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require!(
        bid_index < bid_count,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require!(
        ctx.accounts.bid_receipt.rfq == rfq_key
            && ctx.accounts.bid_receipt.maker_id == maker_id
            && ctx.accounts.bid_receipt.maker == maker,
        ConfidentialRfqError::UnauthorizedMaker
    );
    let (authority, canonical_bump) = rfq_authority_address(&rfq_key, &nonce);
    require_eq!(
        canonical_bump,
        bump,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    require_keys_eq!(
        ctx.accounts.rfq_authority.key(),
        authority,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let (store_key, store_bump) = zama_host::encrypted_store_address(crate::ID, authority, nonce);
    require!(
        ctx.accounts.rfq_store.key() == store_key
            && ctx.accounts.rfq_store.program == crate::ID
            && ctx.accounts.rfq_store.authority == authority
            && ctx.accounts.rfq_store.scope == nonce
            && ctx.accounts.rfq_store.bump == store_bump,
        ConfidentialRfqError::RfqStoreMismatch
    );
    for (side, mint) in [
        (&ctx.accounts.asset, asset_mint),
        (&ctx.accounts.basis, basis_mint),
    ] {
        require_keys_eq!(
            side.confidential_mint.key(),
            mint,
            ConfidentialRfqError::MintMismatch
        );
        require_keys_eq!(
            side.confidential_mint.underlying_mint,
            side.underlying_mint.key(),
            ConfidentialRfqError::MintMismatch
        );
        for (owner, token, store) in [
            (authority, &side.rfq_token_account, &side.rfq_balance_store),
            (
                maker,
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
                store.key(),
                ct::encrypted_store_address(mint, expected_token).0,
                ConfidentialRfqError::TokenAccountMismatch
            );
        }
    }

    require!(
        Clock::get()?.unix_timestamp >= timeout,
        ConfidentialRfqError::RfqNotExpired
    );
    let bump_seed = [bump];
    let authority_seeds = &rfq_authority_signer_seeds(&rfq_key, &nonce, &bump_seed);
    let asset_balance_store_id = StoreId::new(
        ct::ID,
        ctx.accounts.asset.rfq_token_account.key(),
        asset_mint.to_bytes(),
    );
    let basis_balance_store_id = StoreId::new(
        ct::ID,
        ctx.accounts.basis.rfq_token_account.key(),
        basis_mint.to_bytes(),
    );

    // The first execution returns only the asset payout handle. Its grant is
    // consumed by the next token CPI and leaves no persistent RFQ state.
    let asset_handle = cpi::invoke_returning(
        build_asset_payout_execution(
            &ctx.accounts.rfq_store,
            asset_balance_store_id,
            id,
            maker_id,
        )?,
        execution_accounts(&ctx),
        [
            ctx.accounts.rfq_store.to_account_info(),
            ctx.accounts.asset.rfq_balance_store.to_account_info(),
        ],
        [ctx.accounts.rfq_authority.to_account_info()],
        &[authority_seeds],
    )?;
    cpi::transfer_maker_payout(&ctx, authority_seeds, asset_handle, cpi::PayoutToken::Asset)?;

    // The second execution sees the original bid values, computes the basis
    // payout, zeroes a claimed bid, increments the encrypted close count once,
    // and seals a new public CanClose handle.
    let basis_handle = cpi::invoke_returning(
        build_basis_payout_execution(
            &ctx.accounts.rfq_store,
            basis_balance_store_id,
            id,
            maker_id,
            bid_count,
            maker,
        )?,
        execution_accounts(&ctx),
        [
            ctx.accounts.rfq_store.to_account_info(),
            ctx.accounts.basis.rfq_balance_store.to_account_info(),
        ],
        [ctx.accounts.rfq_authority.to_account_info()],
        &[authority_seeds],
    )?;
    cpi::transfer_maker_payout(&ctx, authority_seeds, basis_handle, cpi::PayoutToken::Basis)?;
    Ok(())
}

#[inline(never)]
fn build_asset_payout_execution(
    rfq_account: &zama_host::EncryptedStore,
    token_store_id: StoreId,
    id: NonZeroU64,
    maker_id: u64,
) -> Result<zama_fhe::ReturningFheExecution<Uint<64>>> {
    let state = Box::new(Store::new(rfq_account));
    let inputs = Box::new(AssetPayoutInputs {
        buy: state
            .get::<Uint<64>>(RFQPrivateField::MakerBuy(id).key())
            .map_err(invalid_fhe)?,
        user_buyer: state
            .get::<Bool>(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)?,
        best_maker: state
            .get::<Uint<64>>(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)?,
        size: state
            .get::<Uint<64>>(RFQPrivateField::Size.key())
            .map_err(invalid_fhe)?,
    });
    let output = Box::new(state.result().allow_transient(token_store_id));
    FheExecution::build_returning(state.id(), move |fhe| {
        let payout = asset_payout_value(fhe, &inputs, maker_id)?;
        fhe.output(payout, *output)?;
        Ok(payout)
    })
    .map_err(invalid_fhe)
}

struct AssetPayoutInputs {
    buy: FheHandle<Uint<64>>,
    user_buyer: FheHandle<Bool>,
    best_maker: FheHandle<Uint<64>>,
    size: FheHandle<Uint<64>>,
}

#[inline(never)]
fn asset_payout_value<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &AssetPayoutInputs,
    maker_id: u64,
) -> zama_fhe::Result<Encrypted<'id, Uint<64>>> {
    let live_bid = fhe.gt(input.buy, Scalar::<Uint<64>>::u64(0))?;
    let winner = fhe.eq(input.best_maker, Scalar::<Uint<64>>::u64(maker_id))?;
    let zero = fhe.trivial_encrypt_u64(0)?;
    let two_sizes = fhe.add(input.size, input.size)?;
    let winner_amount = fhe.if_then_else(input.user_buyer, zero, two_sizes)?;
    let selected = fhe.if_then_else(winner, winner_amount, input.size)?;
    fhe.if_then_else(live_bid, selected, zero)
}

#[inline(never)]
fn build_basis_payout_execution(
    rfq_account: &zama_host::EncryptedStore,
    token_store_id: StoreId,
    id: NonZeroU64,
    maker_id: u64,
    bid_count: u64,
    maker: Pubkey,
) -> Result<zama_fhe::ReturningFheExecution<Uint<64>>> {
    let state = Box::new(Store::new(rfq_account));
    let inputs = Box::new(BasisPayoutInputs {
        buy: state
            .get::<Uint<64>>(RFQPrivateField::MakerBuy(id).key())
            .map_err(invalid_fhe)?,
        sell: state
            .get::<Uint<64>>(RFQPrivateField::MakerSell(id).key())
            .map_err(invalid_fhe)?,
        user_buyer: state
            .get::<Bool>(RFQPrivateField::UserBuyer.key())
            .map_err(invalid_fhe)?,
        best_maker: state
            .get::<Uint<64>>(RFQPrivateField::BestMaker.key())
            .map_err(invalid_fhe)?,
        best_offer: state
            .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
            .map_err(invalid_fhe)?,
        closed_bids: state
            .get::<Uint<64>>(RFQPrivateField::ClosedBids.key())
            .map_err(invalid_fhe)?,
        user_claimed: state
            .get::<Bool>(RFQPrivateField::UserClaimed.key())
            .map_err(invalid_fhe)?,
    });
    let buy_output = Box::new(state.set(RFQPrivateField::MakerBuy(id).key()).allow(maker));
    let sell_output = Box::new(state.set(RFQPrivateField::MakerSell(id).key()).allow(maker));
    let closed_output = Box::new(state.set(RFQPrivateField::ClosedBids.key()));
    let can_close_output = Box::new(state.set(RFQPrivateField::CanClose.key()).make_public());
    let payout_output = Box::new(state.result().allow_transient(token_store_id));
    FheExecution::build_returning(state.id(), move |fhe| {
        let value = basis_payout_value(fhe, &inputs, maker_id, bid_count)?;
        fhe.output(value.next_buy, *buy_output)?;
        fhe.output(value.next_sell, *sell_output)?;
        fhe.output(value.next_closed_bids, *closed_output)?;
        fhe.output(value.can_close, *can_close_output)?;
        fhe.output(value.payout, *payout_output)?;
        Ok(value.payout)
    })
    .map_err(invalid_fhe)
}

struct BasisPayoutInputs {
    buy: FheHandle<Uint<64>>,
    sell: FheHandle<Uint<64>>,
    user_buyer: FheHandle<Bool>,
    best_maker: FheHandle<Uint<64>>,
    best_offer: FheHandle<Uint<64>>,
    closed_bids: FheHandle<Uint<64>>,
    user_claimed: FheHandle<Bool>,
}

struct BasisPayoutValue<'id> {
    payout: Encrypted<'id, Uint<64>>,
    next_buy: Encrypted<'id, Uint<64>>,
    next_sell: Encrypted<'id, Uint<64>>,
    next_closed_bids: Encrypted<'id, Uint<64>>,
    can_close: Encrypted<'id, Bool>,
}

#[inline(never)]
fn basis_payout_value<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    input: &BasisPayoutInputs,
    maker_id: u64,
    bid_count: u64,
) -> zama_fhe::Result<BasisPayoutValue<'id>> {
    let live_bid = fhe.gt(input.buy, Scalar::<Uint<64>>::u64(0))?;
    let winner = fhe.eq(input.best_maker, Scalar::<Uint<64>>::u64(maker_id))?;
    let zero = fhe.trivial_encrypt_u64(0)?;
    let one = fhe.trivial_encrypt_u64(1)?;
    let buyer_payout = fhe.add(input.buy, input.best_offer)?;
    let seller_payout = fhe.sub(input.buy, input.best_offer)?;
    let winning_amount = fhe.if_then_else(input.user_buyer, buyer_payout, seller_payout)?;
    let selected = fhe.if_then_else(winner, winning_amount, input.buy)?;
    let payout = fhe.if_then_else(live_bid, selected, zero)?;
    let next_buy = fhe.if_then_else(live_bid, zero, input.buy)?;
    let next_sell = fhe.if_then_else(live_bid, zero, input.sell)?;
    let increment = fhe.if_then_else(live_bid, one, zero)?;
    let next_closed_bids = fhe.add(input.closed_bids, increment)?;
    let all_bids_closed = fhe.eq(next_closed_bids, Scalar::<Uint<64>>::u64(bid_count))?;
    let can_close = fhe.and(input.user_claimed, all_bids_closed)?;
    Ok(BasisPayoutValue {
        payout,
        next_buy,
        next_sell,
        next_closed_bids,
        can_close,
    })
}

fn execution_accounts<'info>(
    ctx: &Context<'info, ClaimRfqMaker<'info>>,
) -> ExecutionCpiAccounts<'info> {
    ExecutionCpiAccounts {
        payer: ctx.accounts.maker.to_account_info(),
        authority: ctx.accounts.rfq_authority.to_account_info(),
        host_config: ctx.accounts.host_config.to_account_info(),
        deny_scope_records: ctx.remaining_accounts.to_vec(),
        system_program: ctx.accounts.system_program.to_account_info(),
        hcu_block_meter: ctx
            .accounts
            .hcu_block_meter
            .as_ref()
            .map(ToAccountInfo::to_account_info),
        hcu_trusted_app_record: ctx
            .accounts
            .hcu_trusted_app_record
            .as_ref()
            .map(ToAccountInfo::to_account_info),
        rand_nonce: None,
        event_authority: ctx.accounts.zama_event_authority.to_account_info(),
        transient_store: ctx.accounts.transient_store.to_account_info(),
        instructions: ctx.accounts.instructions.to_account_info(),
        program: ctx.accounts.zama_program.to_account_info(),
    }
}

fn invalid_fhe(error: zama_fhe::FheExecutionBuildError) -> anchor_lang::error::Error {
    msg!("invalid maker claim FHE execution: {:?}", error);
    error!(ConfidentialRfqError::InvalidFheExecution)
}

#[cfg(test)]
mod packet_tests {
    use super::*;
    use anchor_lang::{InstructionData, ToAccountMetas};
    use solana_sdk::{
        hash::Hash,
        instruction::Instruction,
        message::{AddressLookupTableAccount, v0},
    };
    use std::str::FromStr;

    #[test]
    fn maker_claim_fits_v0_transaction_packet() {
        let maker = Pubkey::new_unique();
        let next = || Pubkey::new_unique();
        let accounts = crate::accounts::ClaimRfqMaker {
            maker,
            rfq: next(),
            rfq_authority: next(),
            bid_receipt: next(),
            rfq_store: next(),
            asset: crate::accounts::TokenSide {
                confidential_mint: next(),
                underlying_mint: next(),
                participant_ata: next(),
                rfq_ata: next(),
                participant_token_account: next(),
                rfq_token_account: next(),
                participant_balance_store: next(),
                rfq_balance_store: next(),
            },
            basis: crate::accounts::TokenSide {
                confidential_mint: next(),
                underlying_mint: next(),
                participant_ata: next(),
                rfq_ata: next(),
                participant_token_account: next(),
                rfq_token_account: next(),
                participant_balance_store: next(),
                rfq_balance_store: next(),
            },
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
            data: crate::instruction::ClaimRfqMaker {
                maker_id: 1,
                bid_index: 0,
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
            .expect("v0 maker claim message should compile");
        let wire_size = 1
            + 64 * usize::from(message.header.num_required_signatures)
            + message.serialize().len();
        eprintln!("maker claim wire size: {wire_size} bytes");
        assert!(wire_size <= 1_232);
    }
}
