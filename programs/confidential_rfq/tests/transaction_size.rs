mod support;

use solana_sdk::{
    hash::Hash,
    message::{AddressLookupTableAccount, VersionedMessage, v0},
    pubkey::Pubkey,
};
use support::{DEADLINE, RfqFixture};

#[test]
fn creation_and_funding_with_lookup_table_fit_separate_solana_packets() {
    let fixture = RfqFixture::new(true, 10, 100, DEADLINE);
    for (name, instruction) in [
        ("creation", fixture.request.clone()),
        ("funding", fixture.funding.clone()),
    ] {
        let mut instructions =
            zama_solana_test_kit::transaction::fhe_transaction(fixture.user, [instruction]);
        // ComputeBudgetInstruction::SetComputeUnitLimit(1_400_000).
        let mut budget_data = vec![2];
        budget_data.extend_from_slice(&1_400_000u32.to_le_bytes());
        instructions.insert(
            0,
            solana_sdk::instruction::Instruction {
                program_id: solana_sdk::pubkey!("ComputeBudget111111111111111111111111111111"),
                accounts: vec![],
                data: budget_data,
            },
        );
        let mut addresses = instructions
            .iter()
            .flat_map(|ix| {
                ix.accounts
                    .iter()
                    .filter(|account| !account.is_signer)
                    .map(|account| account.pubkey)
            })
            .collect::<Vec<_>>();
        addresses.sort_unstable();
        addresses.dedup();
        let table = AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses,
        };
        let message = VersionedMessage::V0(
            v0::Message::try_compile(&fixture.user, &instructions, &[table], Hash::default())
                .unwrap(),
        );
        // Compact signature-vector length, signatures, then the serialized versioned message.
        let packet_size =
            1 + 64 * message.header().num_required_signatures as usize + message.serialize().len();
        assert!(
            packet_size <= 1232,
            "{name} transaction uses {packet_size} bytes"
        );
    }
}
