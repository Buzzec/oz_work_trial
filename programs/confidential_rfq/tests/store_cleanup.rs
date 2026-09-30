//! Exercise certified RFQ cleanup while all host-owned encrypted stores remain intact.

use anchor_lang::{AccountDeserialize, Discriminator, prelude::*};
use confidential_rfq::{
    accounts, instruction,
    state::{
        CurrentAccountVersion,
        rfq::{RFQ, RFQPrivateField},
    },
    util::pda::{maker_store_address, rfq_funder_address, rfq_seeds_from_keys},
};
use mollusk_svm::{Mollusk, result::Check};
use solana_sdk::{
    account::Account as SolanaAccount,
    instruction::{AccountMeta, Instruction},
};
use zama_host::{EncryptedSlot, EncryptedStore, instructions::MmrInclusionProof};
use zama_solana_test_kit::{
    DECRYPTION_CONTRACT, GATEWAY_CHAIN_ID, HostConfigParams, anchor_ix, empty_system_account,
    encrypted_store_account, funded_system_account, host_config_account, kms_context_account,
    signing, system_program_account, u256_be,
};

fn svm() -> Mollusk {
    let deploy = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/");
    let mut svm = Mollusk::new(&confidential_rfq::ID, &format!("{deploy}confidential_rfq"));
    svm.add_program(&zama_host::ID, &format!("{deploy}zama_host"));
    svm.compute_budget.compute_unit_limit = 1_400_000;
    svm
}

fn store(authority: Pubkey, scope: [u8; 32]) -> (Pubkey, EncryptedStore) {
    let (key, bump) = zama_host::encrypted_store_address(confidential_rfq::ID, authority, scope);
    (
        key,
        EncryptedStore {
            program: confidential_rfq::ID,
            authority,
            scope,
            bump,
            slots: vec![],
            leaf_count: 0,
            peaks: vec![],
        },
    )
}

fn execute(svm: &Mollusk, ix: &Instruction, accounts: &mut Vec<(Pubkey, SolanaAccount)>) {
    let result = svm.process_and_validate_instruction(ix, accounts, &[Check::success()]);
    for (key, updated) in result.resulting_accounts {
        if let Some((_, existing)) = accounts.iter_mut().find(|(address, _)| *address == key) {
            *existing = updated;
        } else {
            accounts.push((key, updated));
        }
    }
}

fn get(accounts: &[(Pubkey, SolanaAccount)], key: Pubkey) -> &SolanaAccount {
    &accounts
        .iter()
        .find(|(address, _)| *address == key)
        .unwrap()
        .1
}

struct Fixture {
    accounts: Vec<(Pubkey, SolanaAccount)>,
    rfq: Pubkey,
    user: Pubkey,
    funder: Pubkey,
    primary: Pubkey,
    host_config: Pubkey,
    kms_context: Pubkey,
    handle: [u8; 32],
}

impl Fixture {
    fn new(makers: &[u32]) -> Self {
        let market = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let nonce = [4; 32];
        let (rfq_key, bump) = Pubkey::find_program_address(
            &rfq_seeds_from_keys(&market, &user, &nonce),
            &confidential_rfq::ID,
        );
        let (funder, funder_bump) = rfq_funder_address(rfq_key);
        let rfq = RFQ {
            version: RFQ::VERSION,
            nonce,
            market,
            user,
            bump,
            funder_bump,
            open_stores: makers.len() as u32 + 1,
            asset_mint: Pubkey::new_unique(),
            basis_mint: Pubkey::new_unique(),
        };
        let mut data = RFQ::DISCRIMINATOR.to_vec();
        data.extend_from_slice(bytemuck::bytes_of(&rfq));
        let (primary, mut state) = store(rfq_key, nonce);
        let handle = [9; 32];
        state.slots.push(EncryptedSlot {
            key: RFQPrivateField::CanClose.key(),
            handle,
        });
        let leaf = zama_solana_acl::public_decrypt_leaf_commitment(primary.to_bytes(), 0, handle);
        zama_solana_acl::mmr_append(&mut state.peaks, &mut state.leaf_count, leaf).unwrap();
        let context_id = zama_solana_test_kit::canonical_test_context_id(1);
        let mut config = HostConfigParams::new(Pubkey::new_unique());
        config.current_kms_context_id = context_id;
        let (host_config, host_account) = host_config_account(&config);
        let (kms_context, kms_account) = kms_context_account(
            context_id,
            vec![signing::secp_evm_address(&signing::kms_signing_key())],
            1,
        );
        let mut accounts = vec![
            (
                rfq_key,
                SolanaAccount {
                    lamports: 10_000_000,
                    data,
                    owner: confidential_rfq::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            ),
            (user, funded_system_account()),
            (funder, funded_system_account()),
            (primary, encrypted_store_account(&state)),
            (host_config, host_account),
            (kms_context, kms_account),
            (
                zama_host::ID,
                mollusk_svm::program::create_program_account_loader_v3(&zama_host::ID),
            ),
            (System::id(), system_program_account()),
        ];
        for maker_id in makers {
            let (authority, _) = maker_store_address(rfq_key, *maker_id);
            let (key, state) = store(authority, nonce);
            accounts.push((authority, empty_system_account()));
            accounts.push((key, encrypted_store_account(&state)));
        }
        Self {
            accounts,
            rfq: rfq_key,
            user,
            funder,
            primary,
            host_config,
            kms_context,
            handle,
        }
    }

    fn close_rfq(&self) -> Instruction {
        self.close_rfq_with(1, MmrInclusionProof::default())
    }

    fn close_rfq_with(&self, cleartext: u64, proof: MmrInclusionProof) -> Instruction {
        let extra_data = vec![0];
        let signatures = signing::kms_public_decrypt_cert(
            self.handle,
            u256_be(cleartext),
            GATEWAY_CHAIN_ID,
            &DECRYPTION_CONTRACT,
            &extra_data,
        );
        anchor_ix(
            confidential_rfq::ID,
            accounts::CloseRfq {
                rfq: self.rfq,
                user: self.user,
                rfq_funder: self.funder,
                rfq_store: self.primary,
                host_config: self.host_config,
                kms_context: self.kms_context,
                zama_program: zama_host::ID,
                system_program: System::id(),
            },
            instruction::CloseRfq {
                signatures,
                extra_data,
                proof,
            },
        )
    }

    fn open_stores(&self) -> u32 {
        let bytes = &get(&self.accounts, self.rfq).data[RFQ::DISCRIMINATOR.len()..];
        bytemuck::pod_read_unaligned::<RFQ>(bytes).open_stores
    }
}

#[test]
fn certified_close_refunds_only_rfq_and_funder_and_retains_encrypted_stores() {
    let svm = svm();
    let mut fixture = Fixture::new(&[1, 2]);
    let expected_refund = get(&fixture.accounts, fixture.rfq).lamports
        + get(&fixture.accounts, fixture.funder).lamports;
    let before = get(&fixture.accounts, fixture.user).lamports;
    let retained: Vec<_> = fixture
        .accounts
        .iter()
        .filter(|(_, account)| account.owner == zama_host::ID)
        .cloned()
        .collect();
    assert_eq!(fixture.open_stores(), 3);
    let close = fixture.close_rfq();
    execute(&svm, &close, &mut fixture.accounts);
    assert_eq!(get(&fixture.accounts, fixture.rfq).lamports, 0);
    assert!(get(&fixture.accounts, fixture.rfq).data.is_empty());
    assert_eq!(get(&fixture.accounts, fixture.funder).lamports, 0);
    assert_eq!(
        get(&fixture.accounts, fixture.user).lamports,
        before + expected_refund
    );
    for (key, account) in retained {
        assert_eq!(get(&fixture.accounts, key), &account);
    }
    assert!(
        svm.process_instruction(&close, &fixture.accounts)
            .program_result
            .is_err()
    );
}

#[test]
fn close_rejects_wrong_beneficiary_funder_foreign_store_and_trailing_accounts() {
    let svm = svm();
    let mut fixture = Fixture::new(&[1]);
    let other = Fixture::new(&[]);
    fixture
        .accounts
        .push((other.primary, get(&other.accounts, other.primary).clone()));
    let original = fixture.close_rfq();
    for (index, replacement) in [(1, fixture.funder), (2, fixture.user), (3, other.primary)] {
        let mut ix = original.clone();
        ix.accounts[index].pubkey = replacement;
        assert!(
            svm.process_instruction(&ix, &fixture.accounts)
                .program_result
                .is_err()
        );
    }
    let mut trailing = original.clone();
    trailing
        .accounts
        .push(AccountMeta::new_readonly(fixture.primary, false));
    assert!(
        svm.process_instruction(&trailing, &fixture.accounts)
            .program_result
            .is_err()
    );
    assert_eq!(fixture.open_stores(), 2);
    execute(&svm, &original, &mut fixture.accounts);
}

#[test]
fn close_requires_current_true_certificate_and_valid_public_inclusion_proof() {
    let svm = svm();
    let mut fixture = Fixture::new(&[]);
    let false_certificate = fixture.close_rfq_with(0, MmrInclusionProof::default());
    assert!(
        svm.process_instruction(&false_certificate, &fixture.accounts)
            .program_result
            .is_err()
    );
    let bad_proof = fixture.close_rfq_with(
        1,
        MmrInclusionProof {
            leaf_index: 0,
            siblings: vec![[6; 32]],
        },
    );
    assert!(
        svm.process_instruction(&bad_proof, &fixture.accounts)
            .program_result
            .is_err()
    );
    let primary_index = fixture
        .accounts
        .iter()
        .position(|(key, _)| *key == fixture.primary)
        .unwrap();
    let original = fixture.accounts[primary_index].1.clone();
    let mut state = EncryptedStore::try_deserialize(&mut original.data.as_slice()).unwrap();
    // A historical true certificate stays provable in the MMR but cannot close a newer state.
    state.slots[0].handle = [10; 32];
    fixture.accounts[primary_index].1 = encrypted_store_account(&state);
    assert!(
        svm.process_instruction(&fixture.close_rfq(), &fixture.accounts)
            .program_result
            .is_err()
    );
    state.slots.clear();
    fixture.accounts[primary_index].1 = encrypted_store_account(&state);
    assert!(
        svm.process_instruction(&fixture.close_rfq(), &fixture.accounts)
            .program_result
            .is_err()
    );
    fixture.accounts[primary_index].1 = original;
    let close = fixture.close_rfq();
    execute(&svm, &close, &mut fixture.accounts);
}

#[test]
fn close_handles_an_empty_funder_without_touching_the_primary_store() {
    let svm = svm();
    let mut fixture = Fixture::new(&[]);
    let funder_index = fixture
        .accounts
        .iter()
        .position(|(key, _)| *key == fixture.funder)
        .unwrap();
    fixture.accounts[funder_index].1 = empty_system_account();
    let before = get(&fixture.accounts, fixture.user).lamports;
    let rfq_rent = get(&fixture.accounts, fixture.rfq).lamports;
    let store_before = get(&fixture.accounts, fixture.primary).clone();
    let close = fixture.close_rfq();
    execute(&svm, &close, &mut fixture.accounts);
    assert_eq!(
        get(&fixture.accounts, fixture.user).lamports,
        before + rfq_rent
    );
    assert_eq!(get(&fixture.accounts, fixture.primary), &store_before);
}
