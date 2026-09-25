//! Open and fund an RFQ atomically with confidential bid terms.

use crate::{
    CurrentAccountVersion,
    errors::ConfidentialRfqError,
    state::{
        market::{Market, MarketExt},
        rfq::{RFQ, RFQPrivateField},
    },
    util::{
        ConfidentialTokenEventAuthority, Contains, HostConfig, InputExt, InstructionsAccount,
        TransientStore, ZamaEventAuthority,
        pda::{rfq_seeds, rfq_signer_seeds},
        request_quote_cpi::{AssetOrBasis, refund_to_user, transfer_to_escrow},
        token_side::*,
    },
};
use anchor_lang::prelude::*;
use confidential_token as ct;
use confidential_token::program::ConfidentialToken;
use zama_fhe::{Bool, FheExecution, ReturningFheExecution, Scalar, Store, StoreId, Uint};
use zama_host::{CoprocessorInputAttestation, EncryptedStore, program::ZamaHost};

#[derive(Accounts)]
#[instruction(amounts: CoprocessorInputAttestation)]
pub struct RequestQuote<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(
        init,
        payer = user,
        space = 8 + RFQ::INIT_SPACE,
        seeds = &rfq_seeds(&market, user.to_account_info().key, &amounts.input_handle),
        bump
    )]
    pub rfq: AccountLoader<'info, RFQ>,
    /// CHECK: canonical host EncryptedStore created in this instruction.
    #[account(mut)]
    pub rfq_store: UncheckedAccount<'info>,
    #[account(constraint = { asset.validate(user.key(), rfq.key())?; true })]
    pub asset: TokenSide<'info>,
    #[account(constraint = { basis.validate(user.key(), rfq.key())?; true })]
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
    pub zama_program: Program<'info, ZamaHost>,
    /// CHECK: confidential-token validates its event authority.
    pub confidential_token_event_authority: UncheckedAccount<'info>,
    pub confidential_token_program: Program<'info, ConfidentialToken>,
    pub system_program: Program<'info, System>,
}
impl<'a, 'info> Contains<'a, &'a Program<'info, ConfidentialToken>> for RequestQuote<'info> {
    fn get(&'a self) -> &'a Program<'info, ConfidentialToken> {
        &self.confidential_token_program
    }
}
impl<'a, 'info> Contains<'a, &'a Program<'info, ZamaHost>> for RequestQuote<'info> {
    fn get(&'a self) -> &'a Program<'info, ZamaHost> {
        &self.zama_program
    }
}
impl<'a, 'info> Contains<'a, &'a Program<'info, System>> for RequestQuote<'info> {
    fn get(&'a self) -> &'a Program<'info, System> {
        &self.system_program
    }
}
impl<'a, 'info> Contains<'a, ZamaEventAuthority<'a, 'info>> for RequestQuote<'info> {
    fn get(&'a self) -> ZamaEventAuthority<'a, 'info> {
        ZamaEventAuthority(&self.zama_event_authority)
    }
}
impl<'a, 'info> Contains<'a, ConfidentialTokenEventAuthority<'a, 'info>> for RequestQuote<'info> {
    fn get(&'a self) -> ConfidentialTokenEventAuthority<'a, 'info> {
        ConfidentialTokenEventAuthority(&self.confidential_token_event_authority)
    }
}
impl<'a, 'info> Contains<'a, TransientStore<'a, 'info>> for RequestQuote<'info> {
    fn get(&'a self) -> TransientStore<'a, 'info> {
        TransientStore(&self.transient_store)
    }
}
impl<'a, 'info> Contains<'a, InstructionsAccount<'a, 'info>> for RequestQuote<'info> {
    fn get(&'a self) -> InstructionsAccount<'a, 'info> {
        InstructionsAccount(&self.instructions)
    }
}
impl<'a, 'info> Contains<'a, HostConfig<'a, 'info>> for RequestQuote<'info> {
    fn get(&'a self) -> HostConfig<'a, 'info> {
        HostConfig(&self.host_config)
    }
}

/// - `amounts`: encrypts `(size << 64) | offer_limit`
/// - `user_buyer` encrypts the user's trade direction.
///
/// The attested `amounts.input_handle` is the RFQ nonce. Both token directions are always exercised.
pub fn request_quote<'info>(
    ctx: Context<'info, RequestQuote<'info>>,
    amounts: Box<CoprocessorInputAttestation>,
    user_buyer: Box<CoprocessorInputAttestation>,
    timeout: i64,
    asset_escrow: Box<CoprocessorInputAttestation>,
    basis_escrow: Box<CoprocessorInputAttestation>,
) -> Result<()> {
    let clock = Clock::get()?;
    // Check timestamp is in the future.
    require!(
        timeout > clock.unix_timestamp,
        ConfidentialRfqError::InvalidRfqInput
    );
    // Check mints are not the same.
    require_keys_neq!(
        ctx.accounts.asset.confidential_mint.key(),
        ctx.accounts.basis.confidential_mint.key(),
        ConfidentialRfqError::InvalidRfqInput
    );

    // Validate inputs.
    let user = ctx.accounts.user.key;
    for input in [&amounts, &user_buyer] {
        input.validate(*user, crate::ID)?;
    }
    for input in [&asset_escrow, &basis_escrow] {
        input.validate(*user, ct::ID)?;
    }

    let scope = amounts.input_handle;
    let store_id = StoreId::new(crate::ID, ctx.accounts.rfq.key(), scope);
    require_keys_eq!(
        ctx.accounts.rfq_store.key(),
        store_id.address(),
        ConfidentialRfqError::InvalidRfqAccounts
    );

    *ctx.accounts.rfq.load_init()? = RFQ {
        version: RFQ::VERSION,
        market: ctx.accounts.market.key(),
        bump: ctx.bumps.rfq,
        user: *user,
        timeout,
        bid_count: 0,
        asset_mint: ctx.accounts.asset.confidential_mint.key(),
        basis_mint: ctx.accounts.basis.confidential_mint.key(),
    };

    let maker_group = ctx.accounts.market.maker_group()?;
    let nonce = amounts.input_handle;
    let rfq_seeds = &rfq_signer_seeds(&ctx.accounts.market, user, &nonce, &ctx.bumps.rfq);

    // create the encrypted store for the RFQ
    zama_host::cpi::create_encrypted_store(
        CpiContext::new_with_signer(
            ctx.accounts.zama_program.key(),
            zama_host::cpi::accounts::CreateEncryptedStore {
                payer: ctx.accounts.user.to_account_info(),
                authority: ctx.accounts.rfq.to_account_info(),
                encrypted_store: ctx.accounts.rfq_store.to_account_info(),
                host_config: ctx.accounts.host_config.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
            },
            &[rfq_seeds],
        ),
        zama_host::instructions::CreateEncryptedStoreArgs {
            program: crate::ID,
            scope: nonce,
            authority_seeds: rfq_seeds.iter().map(|seed| seed.to_vec()).collect(),
        },
    )?;
    // Initialize the private fields
    initialize_fields(&ctx, amounts, user_buyer, maker_group, rfq_seeds)?;

    // Intitialize the escrow accounts
    ctx.accounts.asset.initialize_escrow_account(
        &*ctx.accounts,
        &ctx.accounts.user,
        &ctx.accounts.rfq,
        rfq_seeds,
    )?;
    ctx.accounts.basis.initialize_escrow_account(
        &*ctx.accounts,
        &ctx.accounts.user,
        &ctx.accounts.rfq,
        rfq_seeds,
    )?;

    let asset_transferred = transfer_to_escrow(&ctx, *asset_escrow, AssetOrBasis::Asset)?;
    let basis_transferred = transfer_to_escrow(&ctx, *basis_escrow, AssetOrBasis::Basis)?;

    let asset_refund = calculate_refund(
        &ctx,
        asset_transferred,
        basis_transferred,
        AssetOrBasis::Asset,
        rfq_seeds,
    )?;
    let basis_refund = calculate_refund(
        &ctx,
        asset_transferred,
        basis_transferred,
        AssetOrBasis::Basis,
        rfq_seeds,
    )?;

    activate_quote(
        &ctx,
        asset_transferred,
        basis_transferred,
        maker_group,
        rfq_seeds,
    )?;
    refund_to_user(&ctx, asset_refund, true, rfq_seeds)?;
    refund_to_user(&ctx, basis_refund, false, rfq_seeds)?;

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

#[inline(never)]
fn initialize_fields<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    amounts: Box<CoprocessorInputAttestation>,
    user_buyer: Box<CoprocessorInputAttestation>,
    maker_group: Pubkey,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let state = read_state(&ctx.accounts.rfq_store)?;
    let store = Store::new(&state);
    let user = ctx.accounts.user.key();
    let execution = FheExecution::build(store.id(), |fhe| {
        let amounts = fhe.verified_input::<Uint<128>>(*amounts)?;
        let size_high = fhe.shr(amounts, Scalar::<Uint<128>>::u128(64))?;

        let buyer = fhe.verified_input::<Bool>(*user_buyer)?;
        let claimed = fhe.trivial_encrypt(Scalar::<Bool>::bool(false))?;
        let limit = fhe.cast::<Uint<128>, Uint<64>>(amounts)?;
        let size = fhe.cast::<Uint<128>, Uint<64>>(size_high)?;
        let best_offer = fhe.trivial_encrypt_u64(0)?;
        let best_maker = fhe.trivial_encrypt_u64(0)?;
        let closed_bids = fhe.trivial_encrypt_u64(0)?;
        let can_close = fhe.trivial_encrypt(Scalar::<Bool>::bool(false))?;

        fhe.output(
            buyer,
            store.set(RFQPrivateField::UserBuyer.key()).allow(user),
        )?;
        fhe.output(
            claimed,
            store.set(RFQPrivateField::UserClaimed.key()).allow(user),
        )?;
        fhe.output(
            limit,
            store.set(RFQPrivateField::OfferLimit.key()).allow(user),
        )?;
        fhe.output(
            size,
            store
                .set(RFQPrivateField::Size.key())
                .allow(user)
                .allow(maker_group),
        )?;
        fhe.output(best_offer, store.set(RFQPrivateField::BestOffer.key()))?;
        fhe.output(best_maker, store.set(RFQPrivateField::BestMaker.key()))?;
        fhe.output(closed_bids, store.set(RFQPrivateField::ClosedBids.key()))?;
        fhe.output(
            can_close,
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
    asset_or_basis: AssetOrBasis,
    authority_seeds: &[&[u8]],
) -> Result<[u8; 32]> {
    let token_side = match asset_or_basis {
        AssetOrBasis::Asset => &ctx.accounts.asset,
        AssetOrBasis::Basis => &ctx.accounts.basis,
    };

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

    let target = ct::balance_slot(
        token_side.confidential_mint.key(),
        token_side.rfq_token_account.key(),
    )
    .0;
    let target_account = token_side.rfq_balance_store.to_account_info();
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
            let actual = match asset_or_basis {
                AssetOrBasis::Asset => asset,
                AssetOrBasis::Basis => basis,
            };
            let refund = fhe.if_then_else(valid, zero, actual)?;
            fhe.output(refund, store.result().allow_transient(target))?;
            Ok(refund)
        })
        .map_err(invalid_fhe)?;
    crate::util::cpi::invoke_returning(
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
        [ctx.accounts.rfq_store.to_account_info(), target_account],
        [ctx.accounts.rfq.to_account_info()],
        &[authority_seeds],
    )
}

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
        &[authority_seeds],
    )
}

fn invalid_fhe(error: zama_fhe::FheExecutionBuildError) -> Error {
    msg!("invalid FHE execution: {:?}", error);
    error!(ConfidentialRfqError::InvalidFheExecution)
}
