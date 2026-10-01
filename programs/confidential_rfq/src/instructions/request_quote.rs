//! Create an unfunded RFQ and its escrow accounts without moving user collateral.

use crate::{
    CurrentAccountVersion,
    errors::ConfidentialRfqError,
    state::{
        market::{Market, MarketExt},
        rfq::{MAXIMUM_TIMEOUT, RFQ, RFQPrivateField, RFQState, invalid_fhe},
    },
    util::{
        ConfidentialTokenEventAuthority, Contains, HostConfig, InputExt, InstructionsAccount,
        TransientStore, ZamaEventAuthority,
        pda::{rfq_seeds, rfq_signer_seeds},
        request_quote_cpi::{AssetOrBasis, initialize_escrow},
        rfq::read_encrypted_store as read_state,
        token_side::*,
    },
};
use anchor_lang::prelude::*;
use confidential_token::program::ConfidentialToken;
use zama_fhe::{Bool, Encrypted, FheExecution, FheExecutionBuilder, Scalar, Store, StoreId, Uint};
use zama_host::{CoprocessorInputAttestation, program::ZamaHost};
use zama_solana_acl::{EncryptedStore, MAX_MMR_PEAKS};

/// Three slots: MakerPrivateField::{Buy, Sell, Sequence}, plus maximum history.
const MAKER_STORE_BYTES: usize = EncryptedStore::account_size(3, MAX_MMR_PEAKS);
/// Twelve RFQPrivateField slots, including the CanClose predicate, plus maximum history.
const RFQ_STORE_BYTES: usize = EncryptedStore::account_size(12, MAX_MMR_PEAKS);
/// Maker and RFQ escrow token balance stores for each of the asset and basis mints.
const TOKEN_BALANCE_STORE_COUNT: u64 = 4;
/// Conservatively budget a full maker-sized store for growth of each token balance store.
const TOKEN_BALANCE_STORE_RESERVE_BYTES: usize = MAKER_STORE_BYTES;

#[derive(Accounts)]
#[instruction(amounts: crate::util::EncryptedInput)]
pub struct RequestQuote<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(constraint = market.version == Market::VERSION @ ConfidentialRfqError::InvalidRfqAccounts)]
    pub market: Box<Account<'info, Market>>,
    #[account(
        init,
        payer = user,
        space = 8 + RFQ::INIT_SPACE,
        seeds = &rfq_seeds(&market, user.to_account_info().key, &amounts.input_handle),
        bump
    )]
    pub rfq: AccountLoader<'info, RFQ>,
    /// System-owned, data-empty reserve for subsequent bid storage.
    #[account(
        mut,
        seeds = [b"rfq_funder", rfq.key().as_ref()],
        bump,
        constraint = rfq_funder.data_is_empty() @ ConfidentialRfqError::InvalidRfqAccounts,
    )]
    pub rfq_funder: SystemAccount<'info>,
    /// CHECK: canonical host EncryptedStore created in this instruction.
    #[account(
        mut,
        address = StoreId::new(crate::ID, rfq.key(), amounts.input_handle).address()
            @ ConfidentialRfqError::InvalidRfqAccounts,
    )]
    pub rfq_store: UncheckedAccount<'info>,
    #[account(constraint = { asset.validate(user.key(), rfq.key())?; true })]
    pub asset: TokenSide<'info>,
    #[account(
        constraint = asset.confidential_mint.key() != basis.confidential_mint.key()
            @ ConfidentialRfqError::InvalidRfqInput,
        constraint = { basis.validate(user.key(), rfq.key())?; true },
    )]
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
/// - `timeout` encrypts Unix seconds as Uint64; past values are accepted without disclosure.
/// - `bid_capacity` determines the initial rent reserve; additional funding is a plain transfer.
///
/// The attested `amounts.input_handle` is the RFQ nonce. Creation validates the private terms
/// and prepares both canonical escrows, accepting any already initialized canonical accounts.
/// Valid terms enter Unfunded; `fund_quote` later deposits collateral atomically before bidding.
pub fn request_quote<'info>(
    ctx: Context<'info, RequestQuote<'info>>,
    amounts: Box<CoprocessorInputAttestation>,
    user_buyer: Box<CoprocessorInputAttestation>,
    timeout: Box<CoprocessorInputAttestation>,
    bid_capacity: u32,
) -> Result<()> {
    let clock = Clock::get()?;
    let latest_expiry = u64::try_from(clock.unix_timestamp)
        .map_err(|_| error!(ConfidentialRfqError::InvalidRfqInput))?
        .checked_add(MAXIMUM_TIMEOUT)
        .ok_or(ConfidentialRfqError::InvalidRfqInput)?;
    // Validate inputs.
    let user = ctx.accounts.user.key;
    for input in [&amounts, &user_buyer, &timeout] {
        input.validate(*user, crate::ID)?;
    }

    let scope = amounts.input_handle;
    *ctx.accounts.rfq.load_init()? = RFQ {
        version: RFQ::VERSION,
        market: ctx.accounts.market.key(),
        bump: ctx.bumps.rfq,
        user: *user,
        nonce: scope,
        funder_bump: ctx.bumps.rfq_funder,
        open_stores: 1,
        asset_mint: ctx.accounts.asset.confidential_mint.key(),
        basis_mint: ctx.accounts.basis.confidential_mint.key(),
    };

    // Reserve each maker store's maximum MMR footprint, plus shared bookkeeping and
    // token-store growth. The user may top up this PDA directly for further bids.
    let rent = Rent::get()?;
    // Shared allowance: the RFQ's private fields and growth of the four token balance stores.
    let shared_reserve = rent
        .minimum_balance(RFQ_STORE_BYTES)
        .checked_add(
            TOKEN_BALANCE_STORE_COUNT * rent.minimum_balance(TOKEN_BALANCE_STORE_RESERVE_BYTES),
        )
        .ok_or(ConfidentialRfqError::InvalidRfqInput)?;
    // Each requested bid-capacity unit additionally reserves one maker's private bid store.
    let reserve = rent
        .minimum_balance(MAKER_STORE_BYTES)
        .checked_mul(u64::from(bid_capacity))
        .and_then(|amount| amount.checked_add(shared_reserve))
        .ok_or(ConfidentialRfqError::InvalidRfqInput)?;
    anchor_lang::system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.key(),
            anchor_lang::system_program::Transfer {
                from: ctx.accounts.user.to_account_info(),
                to: ctx.accounts.rfq_funder.to_account_info(),
            },
        ),
        reserve,
    )?;

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
    // Persist private terms and their encrypted validity before preparing zero-balance escrows.
    initialize_fields(
        &ctx,
        amounts,
        user_buyer,
        timeout,
        maker_group,
        latest_expiry,
        rfq_seeds,
    )?;
    initialize_escrow(&ctx, AssetOrBasis::Asset)?;
    initialize_escrow(&ctx, AssetOrBasis::Basis)?;

    Ok(())
}

/// Persist private terms, derive Unfunded/Invalid in FHE, and initialize all bookkeeping.
#[inline(never)]
fn initialize_fields<'info>(
    ctx: &Context<'info, RequestQuote<'info>>,
    amounts: Box<CoprocessorInputAttestation>,
    user_buyer: Box<CoprocessorInputAttestation>,
    timeout: Box<CoprocessorInputAttestation>,
    maker_group: Pubkey,
    latest_expiry: u64,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    let state = read_state(&ctx.accounts.rfq_store)?;
    let store = Store::new(&state);
    let user = ctx.accounts.user.key();
    let execution = FheExecution::build(
        store.id(),
        #[inline(never)]
        |fhe| {
            let terms =
                initialize_terms(fhe, &store, amounts, user_buyer, timeout, user, maker_group)?;
            initialize_bookkeeping(fhe, &store, &terms, latest_expiry)
        },
    )
    .map_err(invalid_fhe)?;
    crate::util::request_quote_cpi::execute_initialization(ctx, execution, authority_seeds)
}

/// Verify and persist the user's terms, retaining their produced handles for validity checks.
#[inline(never)]
#[allow(clippy::boxed_local)]
fn initialize_terms<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    store: &Store<'_>,
    amounts: Box<CoprocessorInputAttestation>,
    user_buyer: Box<CoprocessorInputAttestation>,
    timeout: Box<CoprocessorInputAttestation>,
    user: Pubkey,
    maker_group: Pubkey,
) -> zama_fhe::Result<Box<[Encrypted<'id, Uint<64>>; 3]>> {
    let input = fhe.verified_input::<Uint<128>>(*amounts)?;
    let amounts = fhe.or(input, Scalar::<Uint<128>>::u128(0))?;
    let size_high = fhe.shr(amounts, Scalar::<Uint<128>>::u128(64))?;

    // Store outputs must be produced by this execution, including verified inputs.
    let buyer_input = fhe.verified_input::<Bool>(*user_buyer)?;
    let buyer = fhe.eq(buyer_input, Scalar::<Bool>::bool(true))?;
    let expiry_input = fhe.verified_input::<Uint<64>>(*timeout)?;
    let expiry = fhe.add(expiry_input, Scalar::<Uint<64>>::u64(0))?;
    let limit = fhe.cast::<Uint<128>, Uint<64>>(amounts)?;
    let size = fhe.cast::<Uint<128>, Uint<64>>(size_high)?;

    fhe.output(
        buyer,
        store.set(RFQPrivateField::UserBuyer.key()).allow(user),
    )?;
    fhe.output(
        expiry,
        store
            .set(RFQPrivateField::ExpireTimestamp.key())
            .allow(user)
            .allow(maker_group),
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
    Ok(Box::new([size, limit, expiry]))
}

/// Initialize counters and hidden winner fields, and derive Unfunded/Invalid without disclosure.
#[inline(never)]
fn initialize_bookkeeping<'id>(
    fhe: &mut FheExecutionBuilder<'id>,
    store: &Store<'_>,
    terms: &[Encrypted<'id, Uint<64>>; 3],
    latest_expiry: u64,
) -> zama_fhe::Result<()> {
    let zero32 = fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(0))?;
    let zero64 = fhe.trivial_encrypt_u64(0)?;
    for field in [
        RFQPrivateField::BidCount,
        RFQPrivateField::BidSeq,
        RFQPrivateField::SearchedBids,
    ] {
        fhe.output(zero32, store.set(field.key()).make_public())?;
    }
    // A separate private result keeps winner fields apart from publicly decryptable counters.
    let winner_zero = fhe.trivial_encrypt(Scalar::<Uint<32>>::u32(0))?;
    fhe.output(winner_zero, store.set(RFQPrivateField::BestMaker.key()))?;
    fhe.output(
        winner_zero,
        store.set(RFQPrivateField::BestMakerIndex.key()),
    )?;
    fhe.output(zero64, store.set(RFQPrivateField::BestOffer.key()))?;
    let positive_size = Box::new(fhe.gt(terms[0], Scalar::<Uint<64>>::u64(0))?);
    let positive_limit = Box::new(fhe.gt(terms[1], Scalar::<Uint<64>>::u64(0))?);
    let valid_expiry = Box::new(fhe.lt(terms[2], Scalar::<Uint<64>>::u64(latest_expiry))?);
    let positive = fhe.and(*positive_size, *positive_limit)?;
    let valid = fhe.and(positive, *valid_expiry)?;
    let can_close = fhe.not(valid)?;
    let unfunded = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(RFQState::Unfunded as u8))?;
    let invalid = fhe.trivial_encrypt(Scalar::<Uint<8>>::u8(RFQState::Invalid as u8))?;
    let state = fhe.if_then_else(valid, unfunded, invalid)?;
    fhe.output(state, store.set(RFQPrivateField::State.key()).make_public())?;
    fhe.output(
        can_close,
        store.set(RFQPrivateField::CanClose.key()).make_public(),
    )?;
    Ok(())
}
