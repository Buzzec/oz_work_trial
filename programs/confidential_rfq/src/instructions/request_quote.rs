//! Open and fund an RFQ atomically with confidential bid terms.

use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{Bool, FheExecution, ReturningFheExecution, Scalar, Store, StoreId, Uint};
use zama_host::{CoprocessorInputAttestation, EncryptedStore};

use crate::CurrentAccountVersion;
use crate::errors::ConfidentialRfqError;
use crate::state::market::Market;
use crate::state::rfq::{RFQ, RFQPrivateField};
use crate::util::pda::{
    market_maker_group_signer_seeds, rfq_authority_seeds, rfq_authority_signer_seeds, rfq_seeds,
};
use crate::util::token_side::{
    __client_accounts_token_side, __cpi_client_accounts_token_side, TokenSide, TokenSideBumps,
};

#[derive(Accounts)]
#[instruction(amounts: CoprocessorInputAttestation)]
pub struct RequestQuote<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(
        init,
        payer = user,
        space = 8 + core::mem::size_of::<RFQ>(),
        seeds = &rfq_seeds(market.to_account_info().key, user.to_account_info().key, &amounts.input_handle),
        bump
    )]
    pub rfq: AccountLoader<'info, RFQ>,
    /// CHECK: canonical RFQ PDA, which signs host state and owns escrow token accounts.
    #[account(seeds = &rfq_authority_seeds(rfq.to_account_info().key, &amounts.input_handle), bump)]
    pub rfq_authority: UncheckedAccount<'info>,
    /// CHECK: canonical host EncryptedStore created in this instruction.
    #[account(mut)]
    pub rfq_store: UncheckedAccount<'info>,
    pub asset: TokenSide<'info>,
    pub basis: TokenSide<'info>,

    /// CHECK: host validates the canonical config.
    pub host_config: UncheckedAccount<'info>,
    /// CHECK: host validates its event authority.
    pub zama_event_authority: UncheckedAccount<'info>,
    /// CHECK: shared transaction journal, validated by host.
    #[account(mut)]
    pub transient_store: UncheckedAccount<'info>,
    /// CHECK: host validates the Instructions sysvar and final close.
    pub instructions: UncheckedAccount<'info>,
    pub zama_program: Program<'info, zama_host::program::ZamaHost>,
    /// CHECK: confidential-token validates its event authority.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ct::program::ConfidentialToken>,
    pub system_program: Program<'info, System>,
}

/// `amounts` encrypts `(size << 64) | offer_limit`; `user_buyer` encrypts
/// the user's trade direction. The attested `amounts.input_handle` is the RFQ
/// nonce. Both token directions are always exercised.
pub fn request_quote<'info>(
    ctx: Context<'info, RequestQuote<'info>>,
    amounts: CoprocessorInputAttestation,
    user_buyer: CoprocessorInputAttestation,
    timeout: i64,
    asset_escrow: CoprocessorInputAttestation,
    basis_escrow: CoprocessorInputAttestation,
) -> Result<()> {
    let nonce = amounts.input_handle;
    require!(nonce != [0; 32], ConfidentialRfqError::InvalidRfqInput);
    require!(
        timeout > Clock::get()?.unix_timestamp,
        ConfidentialRfqError::InvalidRfqInput
    );
    require!(
        ctx.accounts.market.version == Market::VERSION,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let market_key = ctx.accounts.market.key();
    let maker_group_bump = ctx.accounts.market.maker_group_bump;
    let maker_group = Pubkey::create_program_address(
        &market_maker_group_signer_seeds(&market_key, &maker_group_bump),
        &crate::ID,
    )
    .map_err(|_| error!(ConfidentialRfqError::InvalidRfqAccounts))?;
    require_keys_neq!(
        ctx.accounts.asset.confidential_mint.key(),
        ctx.accounts.basis.confidential_mint.key(),
        ConfidentialRfqError::InvalidRfqInput
    );
    let user = ctx.accounts.user.key();
    for input in [&amounts, &user_buyer] {
        require!(
            input.user_address == user.to_bytes() && input.contract_address == crate::ID.to_bytes(),
            ConfidentialRfqError::InvalidRfqInput
        );
    }
    for input in [&asset_escrow, &basis_escrow] {
        require!(
            input.user_address == user.to_bytes() && input.contract_address == ct::ID.to_bytes(),
            ConfidentialRfqError::InvalidRfqInput
        );
    }

    let rfq_key = ctx.accounts.rfq.key();
    let authority_key = ctx.accounts.rfq_authority.key();
    let scope = nonce;
    let store_id = StoreId::new(crate::ID, authority_key, scope);
    require_keys_eq!(
        ctx.accounts.rfq_store.key(),
        store_id.address(),
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let asset_mint = ctx.accounts.asset.confidential_mint.key();
    let basis_mint = ctx.accounts.basis.confidential_mint.key();
    check_token_addresses(&ctx, asset_mint, basis_mint, user, authority_key)?;
    {
        let mut rfq = ctx.accounts.rfq.load_init()?;
        rfq.version = RFQ::VERSION;
        rfq.market = market_key;
        rfq.nonce = nonce;
        rfq.authority_bump = ctx.bumps.rfq_authority;
        rfq.user = user;
        rfq.timeout = timeout;
        rfq.bid_count = 0;
        rfq.asset_mint = asset_mint;
        rfq.basis_mint = basis_mint;
    }

    let bump = [ctx.bumps.rfq_authority];
    let authority_seeds = &rfq_authority_signer_seeds(&rfq_key, &nonce, &bump);
    crate::util::request_quote_cpi::create_quote_store(&ctx, scope, authority_seeds)?;
    initialize_packed_fields(
        &ctx,
        Box::new(amounts),
        Box::new(user_buyer),
        authority_seeds,
    )?;
    initialize_inactive_fields(&ctx, maker_group, authority_seeds)?;
    fund_created_quote(
        &ctx,
        asset_escrow,
        basis_escrow,
        maker_group,
        authority_seeds,
    )
}

// Keep account-heavy token CPIs out of request_quote's SBF stack frame.
#[inline(never)]
fn fund_created_quote<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    asset_escrow: CoprocessorInputAttestation,
    basis_escrow: CoprocessorInputAttestation,
    maker_group: Pubkey,
    seeds: &[&[u8]],
) -> Result<()> {
    crate::util::request_quote_cpi::initialize_escrow_accounts(ctx, seeds)?;
    let asset_transferred =
        crate::util::request_quote_cpi::transfer_to_escrow(ctx, asset_escrow, true)?;
    let basis_transferred =
        crate::util::request_quote_cpi::transfer_to_escrow(ctx, basis_escrow, false)?;
    let asset_refund = calculate_refund(ctx, asset_transferred, basis_transferred, true, seeds)?;
    let basis_refund = calculate_refund(ctx, asset_transferred, basis_transferred, false, seeds)?;
    activate_quote(
        ctx,
        asset_transferred,
        basis_transferred,
        maker_group,
        seeds,
    )?;
    crate::util::request_quote_cpi::refund_to_user(ctx, asset_refund, true, seeds)?;
    crate::util::request_quote_cpi::refund_to_user(ctx, basis_refund, false, seeds)
}

fn check_token_addresses(
    ctx: &Context<RequestQuote>,
    asset_mint: Pubkey,
    basis_mint: Pubkey,
    user: Pubkey,
    authority: Pubkey,
) -> Result<()> {
    for (mint, owner, token, store) in [
        (
            asset_mint,
            user,
            &ctx.accounts.asset.participant_token_account,
            &ctx.accounts.asset.participant_balance_store,
        ),
        (
            basis_mint,
            user,
            &ctx.accounts.basis.participant_token_account,
            &ctx.accounts.basis.participant_balance_store,
        ),
        (
            asset_mint,
            authority,
            &ctx.accounts.asset.rfq_token_account,
            &ctx.accounts.asset.rfq_balance_store,
        ),
        (
            basis_mint,
            authority,
            &ctx.accounts.basis.rfq_token_account,
            &ctx.accounts.basis.rfq_balance_store,
        ),
    ] {
        let expected_token = ct::token_account_address(mint, owner).0;
        require_keys_eq!(
            token.key(),
            expected_token,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        require_keys_eq!(
            store.key(),
            ct::encrypted_store_address(mint, expected_token).0,
            ConfidentialRfqError::InvalidRfqAccounts
        );
    }
    Ok(())
}

fn read_state(info: &AccountInfo) -> Result<EncryptedStore> {
    require_keys_eq!(
        *info.owner,
        zama_host::ID,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    let state = EncryptedStore::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    require_keys_eq!(
        info.key(),
        state.canonical_address().0,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    Ok(state)
}

// Kept in a separate SBF frame: two attestations plus the FHE plan exceed the
// 4 KiB frame limit when combined with account creation.
#[inline(never)]
fn initialize_packed_fields<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    amounts: Box<CoprocessorInputAttestation>,
    user_buyer: Box<CoprocessorInputAttestation>,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let state = read_state(&ctx.accounts.rfq_store)?;
    let store = Store::new(&state);
    let user = ctx.accounts.user.key();
    let execution = FheExecution::build(store.id(), |fhe| {
        let amounts = fhe.verified_input::<Uint<128>>(*amounts)?;
        let buyer = fhe.verified_input::<Bool>(*user_buyer)?;
        let size_high = fhe.shr(amounts, Scalar::<Uint<128>>::u128(64))?;
        let size = fhe.cast::<Uint<128>, Uint<64>>(size_high)?;
        let limit = fhe.cast::<Uint<128>, Uint<64>>(amounts)?;
        fhe.output(
            buyer,
            store.set(RFQPrivateField::UserBuyer.key()).allow(user),
        )?;
        // The requested values use these private slots until escrow checks
        // rewrite them to their normal BestOffer/BestMaker meaning below.
        fhe.output(limit, store.set(RFQPrivateField::BestOffer.key()))?;
        fhe.output(size, store.set(RFQPrivateField::BestMaker.key()))?;
        Ok(())
    })
    .map_err(invalid_fhe)?;
    crate::util::request_quote_cpi::execute_initialization(ctx, execution, authority_seeds)
}

#[inline(never)]
fn initialize_inactive_fields<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    maker_group: Pubkey,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let state = read_state(&ctx.accounts.rfq_store)?;
    let store = Store::new(&state);
    let user = ctx.accounts.user.key();
    let execution = FheExecution::build(store.id(), |fhe| {
        let claimed = fhe.trivial_encrypt(Scalar::<Bool>::bool(false))?;
        let inactive_size = fhe.trivial_encrypt_u64(0)?;
        let inactive_limit = fhe.trivial_encrypt_u64(0)?;
        let no_closed_bids = fhe.trivial_encrypt_u64(0)?;
        let cannot_close = fhe.trivial_encrypt(Scalar::<Bool>::bool(false))?;
        fhe.output(
            claimed,
            store.set(RFQPrivateField::UserClaimed.key()).allow(user),
        )?;
        fhe.output(
            inactive_size,
            store
                .set(RFQPrivateField::Size.key())
                .allow(user)
                .allow(maker_group),
        )?;
        fhe.output(
            inactive_limit,
            store.set(RFQPrivateField::OfferLimit.key()).allow(user),
        )?;
        fhe.output(no_closed_bids, store.set(RFQPrivateField::ClosedBids.key()))?;
        fhe.output(
            cannot_close,
            store.set(RFQPrivateField::CanClose.key()).make_public(),
        )?;
        Ok(())
    })
    .map_err(invalid_fhe)?;
    crate::util::request_quote_cpi::execute_initialization(ctx, execution, authority_seeds)
}

/// Reuses both transfer-result grants to return one refund handle. Neither
/// refund is stored persistently; each is granted to the token balance Store.
#[inline(never)]
fn calculate_refund<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    asset_handle: [u8; 32],
    basis_handle: [u8; 32],
    asset_refund: bool,
    authority_seeds: &[&[u8]],
) -> Result<[u8; 32]> {
    let state = read_state(&ctx.accounts.rfq_store)?;
    let store = Store::new(&state);
    let buyer = store
        .get::<Bool>(RFQPrivateField::UserBuyer.key())
        .map_err(invalid_fhe)?;
    let requested_size = store
        .get::<Uint<64>>(RFQPrivateField::BestMaker.key())
        .map_err(invalid_fhe)?;
    let requested_limit = store
        .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
        .map_err(invalid_fhe)?;
    let asset = store
        .granted::<Uint<64>>(asset_handle)
        .map_err(invalid_fhe)?;
    let basis = store
        .granted::<Uint<64>>(basis_handle)
        .map_err(invalid_fhe)?;
    let target = if asset_refund {
        ct::balance_slot(
            ctx.accounts.asset.confidential_mint.key(),
            ctx.accounts.asset.rfq_token_account.key(),
        )
        .0
    } else {
        ct::balance_slot(
            ctx.accounts.basis.confidential_mint.key(),
            ctx.accounts.basis.rfq_token_account.key(),
        )
        .0
    };
    let target_account = if asset_refund {
        ctx.accounts.asset.rfq_balance_store.to_account_info()
    } else {
        ctx.accounts.basis.rfq_balance_store.to_account_info()
    };
    let execution: ReturningFheExecution<Uint<64>> =
        FheExecution::build_returning(store.id(), |fhe| {
            let zero = fhe.trivial_encrypt_u64(0)?;
            let expected_asset = fhe.if_then_else(buyer, zero, requested_size)?;
            let expected_basis = fhe.if_then_else(buyer, requested_limit, zero)?;
            let asset_exact = fhe.eq(asset, expected_asset)?;
            let basis_exact = fhe.eq(basis, expected_basis)?;
            let positive_size = fhe.gt(requested_size, Scalar::<Uint<64>>::u64(0))?;
            let both_exact = fhe.and(asset_exact, basis_exact)?;
            let valid = fhe.and(both_exact, positive_size)?;
            let actual = if asset_refund { asset } else { basis };
            let refund = fhe.if_then_else(valid, zero, actual)?;
            fhe.output(refund, store.result().allow_transient(target))?;
            Ok(refund)
        })
        .map_err(invalid_fhe)?;
    crate::util::cpi::invoke_returning(
        execution,
        zama_fhe::ExecutionCpiAccounts {
            payer: ctx.accounts.user.to_account_info(),
            authority: ctx.accounts.rfq_authority.to_account_info(),
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
        [ctx.accounts.rfq_store.to_account_info(), target_account],
        [ctx.accounts.rfq_authority.to_account_info()],
        &[authority_seeds],
    )
}

// Separate from the refund plans to keep each FHE builder within the SBF frame limit.
#[inline(never)]
fn activate_quote<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    asset_handle: [u8; 32],
    basis_handle: [u8; 32],
    maker_group: Pubkey,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let state = read_state(&ctx.accounts.rfq_store)?;
    let store = Store::new(&state);
    let buyer = store
        .get::<Bool>(RFQPrivateField::UserBuyer.key())
        .map_err(invalid_fhe)?;
    let requested_size = store
        .get::<Uint<64>>(RFQPrivateField::BestMaker.key())
        .map_err(invalid_fhe)?;
    let requested_limit = store
        .get::<Uint<64>>(RFQPrivateField::BestOffer.key())
        .map_err(invalid_fhe)?;
    let asset = store
        .granted::<Uint<64>>(asset_handle)
        .map_err(invalid_fhe)?;
    let basis = store
        .granted::<Uint<64>>(basis_handle)
        .map_err(invalid_fhe)?;
    let user = ctx.accounts.user.key();
    let execution = FheExecution::build(store.id(), |fhe| {
        let zero = fhe.trivial_encrypt_u64(0)?;
        let expected_asset = fhe.if_then_else(buyer, zero, requested_size)?;
        let expected_basis = fhe.if_then_else(buyer, requested_limit, zero)?;
        let asset_exact = fhe.eq(asset, expected_asset)?;
        let basis_exact = fhe.eq(basis, expected_basis)?;
        let positive_size = fhe.gt(requested_size, Scalar::<Uint<64>>::u64(0))?;
        let both_exact = fhe.and(asset_exact, basis_exact)?;
        let valid = fhe.and(both_exact, positive_size)?;
        let active_size = fhe.if_then_else(valid, requested_size, zero)?;
        let active_limit = fhe.if_then_else(valid, requested_limit, zero)?;
        let best_offer = fhe.add(active_limit, Scalar::<Uint<64>>::u64(0))?;
        let no_maker = fhe.trivial_encrypt_u64(0)?;
        fhe.output(
            active_size,
            store
                .set(RFQPrivateField::Size.key())
                .allow(user)
                .allow(maker_group),
        )?;
        fhe.output(
            active_limit,
            store.set(RFQPrivateField::OfferLimit.key()).allow(user),
        )?;
        fhe.output(best_offer, store.set(RFQPrivateField::BestOffer.key()))?;
        fhe.output(no_maker, store.set(RFQPrivateField::BestMaker.key()))?;
        Ok(())
    })
    .map_err(invalid_fhe)?;
    crate::util::cpi::invoke(
        execution,
        zama_fhe::ExecutionCpiAccounts {
            payer: ctx.accounts.user.to_account_info(),
            authority: ctx.accounts.rfq_authority.to_account_info(),
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
        [ctx.accounts.rfq_authority.to_account_info()],
        &[authority_seeds],
    )
}

fn invalid_fhe(error: zama_fhe::FheExecutionBuildError) -> anchor_lang::error::Error {
    msg!("invalid FHE execution: {:?}", error);
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
    fn request_quote_minimal_proof_fits_v0_transaction_packet() {
        let user = Pubkey::new_unique();
        let next = || Pubkey::new_unique();
        let market = next();
        let input_handle = [1; 32];
        let (rfq, _) = crate::util::pda::rfq_address(&market, &user, &input_handle);
        let (rfq_authority, _) = crate::util::pda::rfq_authority_address(&rfq, &input_handle);
        let accounts = crate::accounts::RequestQuote {
            user,
            market,
            rfq,
            rfq_authority,
            rfq_store: StoreId::new(crate::ID, rfq_authority, input_handle).address(),
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
            host_config: next(),
            zama_event_authority: next(),
            transient_store: zama_host::transient_store_address(user).0,
            instructions: Pubkey::from_str("Sysvar1nstructions1111111111111111111111111").unwrap(),
            zama_program: zama_host::ID,
            confidential_token_event_authority: next(),
            confidential_token_program: ct::ID,
            system_program: System::id(),
        };
        let instruction = Instruction {
            program_id: crate::ID,
            accounts: accounts.to_account_metas(None),
            data: crate::instruction::RequestQuote {
                amounts: minimal_attestation(user, crate::ID),
                user_buyer: minimal_attestation(user, crate::ID),
                timeout: 100,
                asset_escrow: minimal_attestation(user, ct::ID),
                basis_escrow: minimal_attestation(user, ct::ID),
            }
            .data(),
        };
        let envelope = zama_solana_test_kit::transaction::fhe_transaction(user, [instruction]);
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
        let message = v0::Message::try_compile(&user, &envelope, &[table], Hash::default())
            .expect("v0 message should compile");
        let wire_size = 1
            + 64 * usize::from(message.header.num_required_signatures)
            + message.serialize().len();
        assert!(
            wire_size <= 1_232,
            "minimal signed RFQ transaction is {wire_size} bytes"
        );
    }
}
