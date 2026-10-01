//! Inspect the permissions and public structure emitted by real RFQ transactions.
//! These offline checks do not model cryptographic security or network timing.

mod support;

use anchor_lang::{AnchorDeserialize, Discriminator, prelude::Pubkey};
use confidential_rfq::state::rfq::{MakerPrivateField, RFQPrivateField as Field};
use std::collections::HashSet;
use support::{DEADLINE, RfqFixture};

/// Follow both RFQ sides through funding, competing bids, selection, and claims.
/// Every primary-store write must grant only the documented recipients: user-only
/// terms, user/group size and expiry, public counters, and private winner fields.
/// Maker prices must remain maker-only, including losing quotes; token balances
/// must remain owner-only. Any extra grant or public amount must fail the test.
#[test]
fn decrypt_permissions() {
    for buyer in [true, false] {
        let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
        let mut results = vec![f.process(f.request.clone()), f.process(f.funding.clone())];
        for (id, buy, sell) in [(1, 120, 80), (2, 110, 90)] {
            let bid = f.bid(id, buy, sell);
            results.push(f.process(bid));
        }
        f.set_clock(DEADLINE);
        for ix in [
            f.expire(),
            f.scan(2),
            f.scan(1),
            f.user_claim(false),
            f.maker_claim(1),
            f.maker_claim(2),
        ] {
            results.push(f.process(ix));
        }

        let group = Pubkey::find_program_address(
            &[b"market_maker_group", f.market.as_ref()],
            &confidential_rfq::ID,
        )
        .0;
        let public_fields = [
            Field::State,
            Field::BidCount,
            Field::BidSeq,
            Field::SearchedBids,
            Field::CanClose,
        ];
        let private_winner = [Field::BestOffer, Field::BestMaker, Field::BestMakerIndex];
        let mut seen = HashSet::new();
        for result in results {
            let message = result.message.as_ref().unwrap();
            for inner in &result.inner_instructions {
                if message.account_keys()[inner.instruction.program_id_index as usize]
                    != zama_host::ID
                {
                    continue;
                }
                let Some(payload) = inner
                    .instruction
                    .data
                    .strip_prefix(zama_host::instruction::FheExecute::DISCRIMINATOR)
                else {
                    continue;
                };
                let args = zama_host::FheExecuteArgs::deserialize(&mut &*payload).unwrap();
                for effect in &args.effects {
                    let store_index = inner.instruction.accounts
                        [zama_host::FHE_EXECUTE_FIXED_ACCOUNTS + effect.store_index as usize]
                        as usize;
                    let store = message.account_keys()[store_index];
                    let mut allows: Vec<_> = effect
                        .allow_indexes
                        .iter()
                        .map(|index| Pubkey::new_from_array(args.dictionary_bytes(*index).unwrap()))
                        .collect();
                    allows.sort_unstable();

                    // Temporary transfer amounts can be disclosed to their parties,
                    // but must not be marked publicly decryptable.
                    let Some(slot) = &effect.slot else {
                        assert!(!effect.make_public, "public temporary amount in {store}");
                        if store == f.rfq_store {
                            assert!(allows.is_empty(), "RFQ intermediate disclosed to a user");
                        }
                        continue;
                    };
                    let key = args.dictionary_bytes(slot.key_index).unwrap();
                    seen.insert((store, key));
                    let (mut expected, public) = if store == f.rfq_store {
                        if [Field::UserBuyer, Field::OfferLimit]
                            .iter()
                            .any(|field| field.key() == key)
                        {
                            (vec![f.user], false)
                        } else if [Field::Size, Field::ExpireTimestamp]
                            .iter()
                            .any(|field| field.key() == key)
                        {
                            (vec![f.user, group], false)
                        } else if public_fields.iter().any(|field| field.key() == key) {
                            (vec![], true)
                        } else {
                            assert!(
                                private_winner.iter().any(|field| field.key() == key),
                                "unexpected RFQ field"
                            );
                            (vec![], false)
                        }
                    } else if let Some(id) = (1..=2).find(|id| f.maker_store(*id).1 == store) {
                        if key == MakerPrivateField::Sequence.key() {
                            (vec![], true)
                        } else {
                            assert!(
                                [MakerPrivateField::Buy, MakerPrivateField::Sell]
                                    .iter()
                                    .any(|field| field.key() == key)
                            );
                            (vec![f.makers[id as usize - 1]], false)
                        }
                    } else {
                        assert_eq!(key, confidential_token::balance_key());
                        let owner = [f.user, f.makers[0], f.makers[1], f.rfq]
                            .into_iter()
                            .find(|owner| {
                                f.asset.store(*owner) == store || f.basis.store(*owner) == store
                            })
                            .expect("known token balance store");
                        (vec![owner], false)
                    };
                    expected.sort_unstable();
                    assert_eq!(
                        allows, expected,
                        "unexpected decryption recipient for {store} / {key:?}"
                    );
                    assert_eq!(
                        effect.make_public, public,
                        "unexpected public permission for {store} / {key:?}"
                    );
                }
            }
        }
        for field in public_fields.into_iter().chain(private_winner).chain([
            Field::UserBuyer,
            Field::OfferLimit,
            Field::Size,
            Field::ExpireTimestamp,
        ]) {
            assert!(
                seen.contains(&(f.rfq_store, field.key())),
                "untested RFQ field {field:?}"
            );
        }
        for id in 1..=2 {
            for field in [
                MakerPrivateField::Buy,
                MakerPrivateField::Sell,
                MakerPrivateField::Sequence,
            ] {
                assert!(seen.contains(&(f.maker_store(id).1, field.key())));
            }
        }
        for owner in [f.user, f.makers[0], f.makers[1], f.rfq] {
            for side in [&f.asset, &f.basis] {
                assert!(seen.contains(&(side.store(owner), confidential_token::balance_key())));
            }
        }
    }
}

/// Change private side, amounts, and whether the deadline has already passed.
/// The same submitted operations must create the same account shapes and emit
/// the same CPI sequence and payload lengths: encrypted decisions must not choose
/// different native branches. Public identities, transaction timing, and the fact
/// that a maker submitted a bid remain observable and are not hidden by this check.
#[test]
fn private_branch_shape() {
    let mut baseline = None;
    for (buyer, size, limit, deadline) in [
        (true, 10, 100, DEADLINE),
        (false, 10, 100, DEADLINE),
        (true, 23, 157, DEADLINE),
        (false, 23, 157, support::NOW - 1),
    ] {
        let mut f = RfqFixture::new(buyer, size, limit, deadline);
        let mut shapes = Vec::new();
        for step in 0..8 {
            let ix = match step {
                0 => f.request.clone(),
                1 => f.funding.clone(),
                2 => f.bid(1, 120, 80),
                3 => {
                    f.set_clock(DEADLINE);
                    f.expire()
                }
                4 => f.scan(1),
                5 => f.user_claim(false),
                6 => f.maker_claim(1),
                7 => f.user_claim(true),
                _ => unreachable!(),
            };
            let result = f.process(ix);
            let message = result.message.as_ref().unwrap();
            let cpis: Vec<_> = result
                .inner_instructions
                .iter()
                .map(|inner| {
                    (
                        message.account_keys()[inner.instruction.program_id_index as usize],
                        inner.stack_height,
                        inner.instruction.accounts.len(),
                        inner.instruction.data.len(),
                        inner
                            .instruction
                            .data
                            .iter()
                            .take(8)
                            .copied()
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            let accounts = f.context.account_store.borrow();
            let sizes: Vec<_> = [
                f.rfq,
                f.rfq_store,
                f.rfq_funder,
                f.maker_store(1).1,
                f.asset.token(f.rfq),
                f.basis.token(f.rfq),
                f.asset.store(f.rfq),
                f.basis.store(f.rfq),
                f.asset.store(f.user),
                f.basis.store(f.user),
                f.asset.store(f.makers[0]),
                f.basis.store(f.makers[0]),
            ]
            .map(|address| {
                accounts
                    .get(&address)
                    .map(|account| (account.owner, account.data.len()))
            })
            .into();
            shapes.push((cpis, sizes));
        }
        if let Some(expected) = &baseline {
            assert_eq!(
                &shapes, expected,
                "private branch changed public structure: buyer={buyer}, size={size}, deadline={deadline}"
            );
        } else {
            baseline = Some(shapes);
        }
    }
}
