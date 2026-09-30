//! Account assembly shared by the bidding phases. FHE decisions stay in PlaceBid.

use crate::{ConfidentialRfqError, instructions::place_bid::PlaceBid, util::cpi};
use anchor_lang::prelude::*;
use confidential_token as ct;
use zama_fhe::{ExecutionCpiAccounts, FheTyped, ReturningFheExecution, Uint};

pub(crate) fn invoke_returning_bid_execution<'info, T: FheTyped>(
    ctx: &Context<'info, PlaceBid<'info>>,
    execution: ReturningFheExecution<T>,
    signer_seeds: &[&[&[u8]]],
) -> Result<[u8; 32]> {
    // The shared resolver filters these witnesses once against the execution.
    let dynamic = [
        ctx.accounts.rfq_store.to_account_info(),
        ctx.accounts.maker_store.to_account_info(),
        ctx.accounts
            .asset
            .participant_balance_store
            .to_account_info(),
        ctx.accounts.asset.rfq_balance_store.to_account_info(),
        ctx.accounts
            .basis
            .participant_balance_store
            .to_account_info(),
        ctx.accounts.basis.rfq_balance_store.to_account_info(),
    ];
    let authorities = [
        ctx.accounts.rfq.to_account_info(),
        ctx.accounts.maker_authority.to_account_info(),
    ];
    cpi::invoke_returning(
        execution,
        execution_accounts(ctx),
        dynamic,
        authorities,
        signer_seeds,
    )
}

/// Results privately shared from the asset phase to the basis phase.
#[derive(Clone, Copy)]
pub(crate) struct BidContinuation {
    pub prices: [u8; 32],
    pub previous_sell_positive: [u8; 32],
}

/// The asset circuit's first and last results carry verified prices and the
/// previous sell activity. Bind both to the exact journal append, RFQ producer,
/// expected type, and permission before the basis phase consumes them.
#[inline(never)]
pub(crate) fn invoke_asset_bid_execution<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    execution: ReturningFheExecution<Uint<64>>,
    signer_seeds: &[&[&[u8]]],
) -> Result<([u8; 32], BidContinuation)> {
    use crate::util::rfq::read_journal;
    let before = read_journal(ctx.accounts.transient_store.as_ref())?.len();
    let steps = execution.execution().cost().steps;
    require!(steps >= 2, ConfidentialRfqError::InvalidTransferResult);
    let expected_len = before
        .checked_add(steps)
        .ok_or(error!(ConfidentialRfqError::InvalidTransferResult))?;
    let refund = invoke_returning_bid_execution(ctx, execution, signer_seeds)?;
    let journal = read_journal(ctx.accounts.transient_store.as_ref())?;
    require!(
        journal.len() == expected_len,
        ConfidentialRfqError::InvalidTransferResult
    );
    let prices = journal
        .result(before)
        .ok_or(error!(ConfidentialRfqError::InvalidTransferResult))?;
    let previous_sell = journal
        .result(expected_len - 1)
        .ok_or(error!(ConfidentialRfqError::InvalidTransferResult))?;
    for (result, expected_type) in [(prices, 6), (previous_sell, 0)] {
        require!(
            result.producer_store == ctx.accounts.rfq_store.key()
                && result.handle[30] == expected_type
                && journal
                    .authorized_depth(result.handle, ctx.accounts.rfq_store.key())
                    .is_some(),
            ConfidentialRfqError::InvalidTransferResult
        );
    }
    Ok((
        refund,
        BidContinuation {
            prices: prices.handle,
            previous_sell_positive: previous_sell.handle,
        },
    ))
}

fn execution_accounts<'info>(ctx: &Context<'info, PlaceBid<'info>>) -> ExecutionCpiAccounts<'info> {
    ExecutionCpiAccounts {
        payer: ctx.accounts.rfq_funder.to_account_info(),
        authority: ctx.accounts.rfq.to_account_info(),
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

/// Deposit the attested maker amount and grant the actual transferred handle to
/// the RFQ. The FHE acceptance circuit checks the required delta and refunds any
/// deposit that does not fund an admissible change exactly.
#[inline(never)]
pub(crate) fn transfer_deposit<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    attestation: zama_host::CoprocessorInputAttestation,
    asset: bool,
    signer_seeds: &[&[&[u8]]],
) -> Result<[u8; 32]> {
    let side = if asset {
        &ctx.accounts.asset
    } else {
        &ctx.accounts.basis
    };
    ct::cpi::confidential_transfer(
        CpiContext::new_with_signer(
            ctx.accounts.confidential_token_program.key(),
            ct::cpi::accounts::ConfidentialTransfer {
                owner: ctx.accounts.maker.to_account_info(),
                payer: ctx.accounts.rfq_funder.to_account_info(),
                mint: side.confidential_mint.to_account_info(),
                underlying_mint: side.underlying_mint.to_account_info(),
                from_ata: side.participant_ata.to_account_info(),
                to_ata: side.rfq_ata.to_account_info(),
                from_account: side.participant_token_account.to_account_info(),
                to_account: side.rfq_token_account.to_account_info(),
                from_store: side.participant_balance_store.to_account_info(),
                to_store: side.rfq_balance_store.to_account_info(),
                zama_event_authority: ctx.accounts.zama_event_authority.to_account_info(),
                transient_store: ctx.accounts.transient_store.to_account_info(),
                instructions: ctx.accounts.instructions.to_account_info(),
                zama_program: ctx.accounts.zama_program.to_account_info(),
                host_config: ctx.accounts.host_config.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                hcu_block_meter: None,
                hcu_trusted_app_record: None,
                result_store: Some(ctx.accounts.rfq_store.to_account_info()),
                event_authority: ctx
                    .accounts
                    .confidential_token_event_authority
                    .to_account_info(),
                program: ctx.accounts.confidential_token_program.to_account_info(),
            },
            signer_seeds,
        ),
        attestation,
    )?;
    let (producer, data) = anchor_lang::solana_program::program::get_return_data()
        .ok_or(ConfidentialRfqError::InvalidTransferResult)?;
    require_keys_eq!(
        producer,
        ct::ID,
        ConfidentialRfqError::InvalidTransferResult
    );
    data.try_into()
        .map_err(|_| error!(ConfidentialRfqError::InvalidTransferResult))
}

/// Refund a computed amount from RFQ escrow with the RFQ PDA's signature.
#[inline(never)]
pub(crate) fn refund_collateral<'info>(
    ctx: &Context<'info, PlaceBid<'info>>,
    handle: [u8; 32],
    asset: bool,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let side = if asset {
        &ctx.accounts.asset
    } else {
        &ctx.accounts.basis
    };
    cpi::transfer_from_grant(
        ctx.accounts.confidential_token_program.key(),
        ct::cpi::accounts::ConfidentialTransferFromValue {
            owner: ctx.accounts.rfq.to_account_info(),
            payer: ctx.accounts.rfq_funder.to_account_info(),
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
        signer_seeds,
        handle,
    )
}
