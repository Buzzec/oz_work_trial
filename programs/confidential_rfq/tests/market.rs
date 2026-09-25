use anchor_lang::{AccountDeserialize, prelude::*};
use confidential_rfq::{accounts, instruction, state::market::Market, util::pda};
use mollusk_svm::{Mollusk, result::Check};
use solana_sdk::{account::Account as SolanaAccount, instruction::Instruction};
use zama_solana_test_kit::{
    HostConfigParams, anchor_ix, empty_system_account, funded_system_account, host_config_account,
    system_program_account,
};

fn account(accounts: &[(Pubkey, SolanaAccount)], key: Pubkey) -> &SolanaAccount {
    &accounts
        .iter()
        .find(|(address, _)| *address == key)
        .unwrap()
        .1
}

fn execute(svm: &Mollusk, ix: Instruction, accounts: &mut Vec<(Pubkey, SolanaAccount)>) {
    let result = svm.process_and_validate_instruction(&ix, accounts, &[Check::success()]);
    for (key, updated) in result.resulting_accounts {
        if let Some((_, existing)) = accounts.iter_mut().find(|(address, _)| *address == key) {
            *existing = updated;
        } else {
            accounts.push((key, updated));
        }
    }
}

fn assert_market_size(
    svm: &Mollusk,
    accounts: &[(Pubkey, SolanaAccount)],
    key: Pubkey,
    maker_count: usize,
) {
    let account = account(accounts, key);
    let market = Market::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(market.makers.len(), maker_count);
    assert_eq!(account.data.len(), 46 + 40 * maker_count);
    assert_eq!(
        account.lamports,
        svm.sysvars.rent.minimum_balance(account.data.len())
    );
}

#[test]
fn market_reallocates_and_refunds_rent_as_makers_change() {
    let deploy = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/");
    let mut svm = Mollusk::new(&confidential_rfq::ID, &format!("{deploy}confidential_rfq"));
    svm.add_program(&zama_host::ID, &format!("{deploy}zama_host"));
    svm.warp_to_slot(100);

    let admin = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let (maker_group, _) = pda::market_maker_group_address(&market);
    let (host_config, host_config_data) = host_config_account(&HostConfigParams::new(admin));
    let mut accounts = vec![
        (admin, funded_system_account()),
        (market, empty_system_account()),
        (maker_group, empty_system_account()),
        (host_config, host_config_data),
        (
            zama_host::ID,
            mollusk_svm::program::create_program_account_loader_v3(&zama_host::ID),
        ),
        (System::id(), system_program_account()),
    ];
    execute(
        &svm,
        anchor_ix(
            confidential_rfq::ID,
            accounts::CreateMarket {
                admin,
                market,
                system_program: System::id(),
            },
            instruction::CreateMarket {},
        ),
        &mut accounts,
    );
    assert_market_size(&svm, &accounts, market, 0);

    let makers = [Pubkey::new_unique(), Pubkey::new_unique()];
    let wildcard = Pubkey::new_from_array(zama_host::WILDCARD_AUTHORITY_BYTES);
    for (index, maker) in makers.iter().copied().enumerate() {
        let delegation_record =
            zama_host::user_decryption_delegation_address(maker_group, maker, wildcard).0;
        accounts.push((delegation_record, empty_system_account()));
        let admin_before = account(&accounts, admin).lamports;
        let rent_before = account(&accounts, market).lamports;
        execute(
            &svm,
            anchor_ix(
                confidential_rfq::ID,
                accounts::AddMaker {
                    admin,
                    market,
                    maker_group,
                    host_config,
                    delegation_record,
                    zama_program: zama_host::ID,
                    system_program: System::id(),
                },
                instruction::AddMaker {
                    maker_id: index as u64 + 1,
                    maker,
                },
            ),
            &mut accounts,
        );
        assert_market_size(&svm, &accounts, market, index + 1);
        assert_eq!(
            admin_before - account(&accounts, admin).lamports,
            account(&accounts, market).lamports - rent_before
                + account(&accounts, delegation_record).lamports
        );
        let delegation = zama_host::UserDecryptionDelegation::try_deserialize(
            &mut account(&accounts, delegation_record).data.as_slice(),
        )
        .unwrap();
        assert!(!delegation.revoked);
        assert_eq!(delegation.delegator, maker_group);
        assert_eq!(delegation.delegate, maker);
    }

    // The host requires revocation to occur after the grant's slot.
    svm.warp_to_slot(101);
    for (index, maker) in makers.iter().copied().enumerate().rev() {
        let delegation_record =
            zama_host::user_decryption_delegation_address(maker_group, maker, wildcard).0;
        let admin_before = account(&accounts, admin).lamports;
        let rent_before = account(&accounts, market).lamports;
        execute(
            &svm,
            anchor_ix(
                confidential_rfq::ID,
                accounts::RemoveMaker {
                    admin,
                    market,
                    maker_group,
                    host_config,
                    delegation_record,
                    zama_program: zama_host::ID,
                    system_program: System::id(),
                },
                instruction::RemoveMaker {
                    maker_id: index as u64 + 1,
                },
            ),
            &mut accounts,
        );
        assert_market_size(&svm, &accounts, market, index);
        assert_eq!(
            account(&accounts, admin).lamports - admin_before,
            rent_before - account(&accounts, market).lamports
        );
        let delegation = zama_host::UserDecryptionDelegation::try_deserialize(
            &mut account(&accounts, delegation_record).data.as_slice(),
        )
        .unwrap();
        assert!(delegation.revoked);
    }
}
