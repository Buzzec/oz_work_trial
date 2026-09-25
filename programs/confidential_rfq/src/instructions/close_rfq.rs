use crate::ConfidentialRfqError;
use crate::instructions::place_bid::BidReceipt;
use crate::state::{
    CurrentAccountVersion,
    rfq::{RFQ, RFQPrivateField},
};
use crate::util::{close_rfq_cpi, pda::bid_receipt_address, rfq::validate_rfq_store};
use anchor_lang::prelude::*;
use zama_host::{
    EncryptedStore, HostConfig, KmsContext, instructions::MmrInclusionProof, program::ZamaHost,
};

#[derive(Accounts)]
pub struct CloseRfq<'info> {
    /// Anyone may close the RFQ; the rent always goes to the recorded user.
    #[account(mut, close = user)]
    pub rfq: AccountLoader<'info, RFQ>,
    /// CHECK: checked against `rfq.user`; receives the RFQ rent.
    #[account(mut)]
    pub user: UncheckedAccount<'info>,
    /// Current host-owned RFQ store, checked against the nonce-bound RFQ authority.
    pub encrypted_store: Box<Account<'info, EncryptedStore>>,
    pub host_config: Box<Account<'info, HostConfig>>,
    pub kms_context: Box<Account<'info, KmsContext>>,
    pub zama_program: Program<'info, ZamaHost>,
}

/// Closes the RFQ when its *current* `can_close` handle has a valid public-decrypt
/// certificate for true. The RFQ account is the act-once guard: replay fails after
/// Anchor closes it, and a replacement RFQ has a new nonce-bound Store identity.
pub fn close_rfq<'info>(
    ctx: Context<'info, CloseRfq<'info>>,
    signatures: Vec<[u8; 65]>,
    extra_data: Vec<u8>,
    proof: MmrInclusionProof,
) -> Result<()> {
    let rfq_key = ctx.accounts.rfq.key();
    let (user, handle, bid_count, timeout) = {
        let rfq = ctx.accounts.rfq.load()?;
        let handle = current_can_close_handle(
            rfq_key,
            &rfq,
            ctx.accounts.encrypted_store.key(),
            &ctx.accounts.encrypted_store,
        )?;
        (rfq.user, handle, rfq.bid_count, rfq.timeout)
    };
    require_keys_eq!(
        ctx.accounts.user.key(),
        user,
        ConfidentialRfqError::InvalidUser
    );
    require!(
        Clock::get()?.unix_timestamp >= timeout,
        ConfidentialRfqError::RfqNotExpired
    );

    close_rfq_cpi::verify_can_close(
        ctx.accounts.zama_program.key(),
        zama_host::cpi::accounts::VerifyPublicDecrypt {
            host_config: ctx.accounts.host_config.to_account_info(),
            kms_context: ctx.accounts.kms_context.to_account_info(),
            encrypted_store: ctx.accounts.encrypted_store.to_account_info(),
        },
        handle,
        signatures,
        extra_data,
        proof,
    )?;

    // An ordinal receipt PDA exists for every placed bid. Claimed makers have
    // already closed their receipts; failed bids may leave theirs open. Return
    // any remaining maker-paid receipt rent to its recorded maker.
    let expected_accounts = usize::try_from(bid_count)
        .ok()
        .and_then(|count| count.checked_mul(2))
        .ok_or(error!(ConfidentialRfqError::InvalidRfqAccounts))?;
    require!(
        ctx.remaining_accounts.len() == expected_accounts,
        ConfidentialRfqError::InvalidRfqAccounts
    );
    for (index, pair) in ctx.remaining_accounts.chunks_exact(2).enumerate() {
        let receipt_info = &pair[0];
        let maker_destination = &pair[1];
        require!(
            receipt_info.is_writable && maker_destination.is_writable,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        let index =
            u64::try_from(index).map_err(|_| error!(ConfidentialRfqError::InvalidRfqAccounts))?;
        let (expected_receipt, expected_bump) = bid_receipt_address(&ctx.accounts.rfq, index);
        require_keys_eq!(
            receipt_info.key(),
            expected_receipt,
            ConfidentialRfqError::InvalidRfqAccounts
        );
        if receipt_info.owner == &crate::ID {
            let receipt = Account::<BidReceipt>::try_from(receipt_info)?;
            require!(
                receipt.rfq == rfq_key
                    && receipt.maker_id != 0
                    && receipt.maker != Pubkey::default()
                    && receipt.bump == expected_bump,
                ConfidentialRfqError::InvalidRfqAccounts
            );
            require_keys_eq!(
                maker_destination.key(),
                receipt.maker,
                ConfidentialRfqError::InvalidRfqAccounts
            );
            receipt.close(maker_destination.clone())?;
        } else {
            require!(
                *receipt_info.owner == anchor_lang::solana_program::system_program::ID
                    && receipt_info.data_is_empty()
                    && receipt_info.lamports() == 0,
                ConfidentialRfqError::InvalidRfqAccounts
            );
        }
    }
    Ok(())
}

fn current_can_close_handle(
    rfq_key: Pubkey,
    rfq: &RFQ,
    encrypted_store_key: Pubkey,
    encrypted_store: &EncryptedStore,
) -> Result<[u8; 32]> {
    require!(
        rfq.version == RFQ::VERSION,
        ConfidentialRfqError::InvalidRfqVersion
    );
    validate_rfq_store(rfq_key, rfq, encrypted_store_key, encrypted_store)
        .map_err(|_| error!(ConfidentialRfqError::InvalidEncryptedStore))?;
    encrypted_store
        .get(&RFQPrivateField::CanClose.key())
        .ok_or(error!(ConfidentialRfqError::CanCloseMissing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::close_rfq_cpi::{bool_true_cleartext, verify_close_result};
    use crate::util::pda::{rfq_seeds_from_keys, rfq_state_signer_seeds};
    use anchor_lang::{InstructionData, ToAccountMetas};
    use solana_sdk::{
        hash::Hash,
        instruction::{AccountMeta, Instruction},
        message::{AddressLookupTableAccount, v0},
    };
    use zama_host::EncryptedSlot;
    use zama_host::instructions::PublicDecryptReturnData;

    fn fixture() -> (Pubkey, RFQ, Pubkey, EncryptedStore, [u8; 32]) {
        let market = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let nonce = [7; 32];
        let (rfq_key, bump) =
            Pubkey::find_program_address(&rfq_seeds_from_keys(&market, &user, &nonce), &crate::ID);
        let scope = nonce;
        let (store_key, store_bump) = zama_host::encrypted_store_address(crate::ID, rfq_key, scope);
        let handle = [9; 32];
        let rfq = RFQ {
            version: RFQ::VERSION,
            market,
            user,
            bump,
            timeout: 1_000,
            bid_count: 0,
            asset_mint: Pubkey::new_unique(),
            basis_mint: Pubkey::new_unique(),
        };
        let store = EncryptedStore {
            program: crate::ID,
            authority: rfq_key,
            scope,
            slots: vec![EncryptedSlot {
                key: RFQPrivateField::CanClose.key(),
                handle,
            }],
            leaf_count: 0,
            peaks: vec![],
            bump: store_bump,
        };
        (rfq_key, rfq, store_key, store, handle)
    }

    #[test]
    fn current_can_close_handle_is_bound_to_rfq_identity_and_slot() {
        let (key, rfq, store_key, store, handle) = fixture();
        assert_eq!(
            current_can_close_handle(key, &rfq, store_key, &store).unwrap(),
            handle
        );
        assert!(current_can_close_handle(Pubkey::new_unique(), &rfq, store_key, &store).is_err());
        let mut replacement = rfq;
        replacement.user = Pubkey::new_unique();
        assert!(current_can_close_handle(key, &replacement, store_key, &store).is_err());
        replacement = rfq;
        replacement.market = Pubkey::new_unique();
        assert!(current_can_close_handle(key, &replacement, store_key, &store).is_err());
        replacement = rfq;
        replacement.bump = rfq.bump.wrapping_add(1);
        assert!(current_can_close_handle(key, &replacement, store_key, &store).is_err());
        let mut wrong_slot = store;
        wrong_slot.slots[0].key = RFQPrivateField::UserClaimed.key();
        assert!(current_can_close_handle(key, &rfq, store_key, &wrong_slot).is_err());
    }

    #[test]
    fn store_scope_recovers_the_rfq_signer_and_rejects_other_stores() {
        let (key, rfq, store_key, store, _) = fixture();
        assert_eq!(
            Pubkey::create_program_address(&rfq_state_signer_seeds(&rfq, &store.scope), &crate::ID)
                .unwrap(),
            key
        );
        assert!(current_can_close_handle(key, &rfq, Pubkey::new_unique(), &store).is_err());
        let mut wrong_store = store.clone();
        wrong_store.scope = [8; 32];
        assert!(current_can_close_handle(key, &rfq, store_key, &wrong_store).is_err());
        wrong_store = store.clone();
        wrong_store.authority = Pubkey::new_unique();
        assert!(current_can_close_handle(key, &rfq, store_key, &wrong_store).is_err());
        wrong_store = store.clone();
        wrong_store.program = Pubkey::new_unique();
        assert!(current_can_close_handle(key, &rfq, store_key, &wrong_store).is_err());
        wrong_store = store.clone();
        wrong_store.bump = store.bump.wrapping_add(1);
        assert!(current_can_close_handle(key, &rfq, store_key, &wrong_store).is_err());
    }

    #[test]
    fn verifier_result_must_certify_current_handle_and_true() {
        let (_, _, _, _, handle) = fixture();
        let mut returned = PublicDecryptReturnData {
            handle,
            cleartext: bool_true_cleartext(),
            context_id: [3; 32],
        };
        verify_close_result(zama_host::ID, handle, &returned).unwrap();
        assert!(verify_close_result(Pubkey::new_unique(), handle, &returned).is_err());
        assert!(verify_close_result(zama_host::ID, [4; 32], &returned).is_err());
        returned.cleartext = [0; 32];
        assert!(verify_close_result(zama_host::ID, handle, &returned).is_err());
    }

    #[test]
    fn ordinal_receipt_pairs_fit_v0_close_packet() {
        let payer = Pubkey::new_unique();
        let rfq = Pubkey::new_unique();
        let next = || Pubkey::new_unique();
        let accounts = crate::accounts::CloseRfq {
            rfq,
            user: next(),
            encrypted_store: next(),
            host_config: next(),
            kms_context: next(),
            zama_program: zama_host::ID,
        };
        let mut metas = accounts.to_account_metas(None);
        for index in 0_u64..12 {
            let receipt = Pubkey::find_program_address(
                &[b"bid_receipt", rfq.as_ref(), &index.to_le_bytes()],
                &crate::ID,
            )
            .0;
            metas.push(AccountMeta::new(receipt, false));
            metas.push(AccountMeta::new(next(), false));
        }
        let instruction = Instruction {
            program_id: crate::ID,
            accounts: metas.clone(),
            data: crate::instruction::CloseRfq {
                signatures: vec![[0; 65]; 2],
                extra_data: vec![0; 32],
                proof: MmrInclusionProof {
                    leaf_index: 0,
                    siblings: vec![[0; 32]; 8],
                },
            }
            .data(),
        };
        let table = AddressLookupTableAccount {
            key: next(),
            addresses: metas.iter().map(|meta| meta.pubkey).collect(),
        };
        let message = v0::Message::try_compile(&payer, &[instruction], &[table], Hash::default())
            .expect("v0 close message should compile");
        let wire_size = 1
            + 64 * usize::from(message.header.num_required_signatures)
            + message.serialize().len();
        assert!(wire_size <= 1_232, "close transaction is {wire_size} bytes");
    }
}
