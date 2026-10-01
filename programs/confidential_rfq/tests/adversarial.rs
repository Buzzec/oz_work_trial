//! Work-trial scenarios: unauthorized settlement, underfunding, replay, and griefing.
//! Clear amounts exist only in the test oracle; every action executes the real programs.

mod support;

use anchor_lang::{AnchorDeserialize, Discriminator, InstructionData, prelude::Pubkey};
use confidential_rfq::{
    ConfidentialRfqError, instruction,
    state::rfq::{MakerPrivateField, RFQPrivateField as Field, RFQState},
};
use support::{DEADLINE, RfqFixture};
use zama_solana_test_kit as kit;

/// Two funded makers, with maker 1 winning on either RFQ side.
fn ready_to_settle(buyer: bool) -> RfqFixture {
    let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
    f.open();
    for (id, buy, sell) in [(1, 120, 80), (2, 110, 90)] {
        let bid = f.bid(id, buy, sell);
        f.process(bid);
    }
    f.set_clock(DEADLINE);
    f.process(f.expire());
    f.process(f.scan(2));
    f.process(f.scan(1));
    assert_eq!(f.value(Field::State), RFQState::Claimable as u64);
    assert_eq!(f.value(Field::BestMaker), 1);
    f
}

/// A losing maker tries to claim the winner's ID and redirect either payout.
/// Both attempts must fail without changing accounts. Relaying the legitimate
/// claim must succeed, paying the winner and refunding only the loser's collateral.
#[test]
fn non_winner_settlement() {
    for buyer in [true, false] {
        let mut f = ready_to_settle(buyer);
        let loser = f.makers[1];
        let before = f.context.account_store.borrow().clone();

        // Claiming the winner's ID with the loser's identity fails authorization.
        let mut impersonation = f.maker_claim(2);
        impersonation.accounts[0].pubkey = loser;
        impersonation.data = instruction::ClaimRfqMaker { maker_id: 1 }.data();
        kit::transaction::process_fhe_instruction(
            &f.context,
            f.user,
            &impersonation,
            &[kit::anchor_error_check(
                ConfidentialRfqError::UnauthorizedMaker as u32,
            )],
        );
        assert_eq!(*f.context.account_store.borrow(), before);

        // Keeping the winner's identity while redirecting a destination also fails.
        for asset in [true, false] {
            let side = if asset { &f.asset } else { &f.basis };
            let mut redirected = f.maker_claim(1);
            redirected.accounts[0].pubkey = loser;
            for meta in &mut redirected.accounts {
                if meta.pubkey == side.token(f.makers[0]) {
                    meta.pubkey = side.token(loser);
                } else if meta.pubkey == side.store(f.makers[0]) {
                    meta.pubkey = side.store(loser);
                }
            }
            kit::transaction::process_fhe_instruction(
                &f.context,
                f.user,
                &redirected,
                &[kit::anchor_error_check(
                    ConfidentialRfqError::InvalidRfqAccounts as u32,
                )],
            );
            assert_eq!(*f.context.account_store.borrow(), before);
        }

        // Claims are permissionless: the loser may relay the winner's payment,
        // but the loser receives only their own collateral when claiming their ID.
        let mut relayed = f.maker_claim(1);
        relayed.accounts[0].pubkey = loser;
        f.process(relayed);
        f.process(f.maker_claim(2));
        f.process(f.user_claim(false));
        assert_eq!(
            [f.balance(true, loser), f.balance(false, loser)],
            [1_000, 1_000]
        );
        assert_eq!(
            f.balance(true, f.makers[0]),
            if buyer { 990 } else { 1_010 }
        );
        assert_eq!(
            f.balance(false, f.makers[0]),
            if buyer { 1_080 } else { 880 }
        );
        assert_eq!(f.balance(true, f.rfq), 0);
        assert_eq!(f.balance(false, f.rfq), 0);
    }
}

/// Replay each claim both before and after the remaining parties settle.
/// Both claim orders must pay exactly once: consumed bids and the Claimed state
/// make subsequent calls encrypted no-ops, preserving balances and bid counts.
#[test]
fn double_settlement() {
    for buyer in [true, false] {
        for maker_first in [true, false] {
            let mut f = ready_to_settle(buyer);
            let user_claim = f.user_claim(false);
            let winner_claim = f.maker_claim(1);
            let loser_claim = f.maker_claim(2);
            let claims = if maker_first {
                [winner_claim, loser_claim, user_claim]
            } else {
                [user_claim, winner_claim, loser_claim]
            };
            for claim in &claims {
                f.process(claim.clone());
                let balances: Vec<_> = [f.user, f.makers[0], f.makers[1], f.rfq]
                    .map(|owner| [f.balance(true, owner), f.balance(false, owner)])
                    .into();
                let count = f.value(Field::BidCount);
                let state = f.value(Field::State);
                // Replay the identical instruction while other claims may still be pending.
                f.process(claim.clone());
                assert_eq!(f.value(Field::BidCount), count);
                assert_eq!(f.value(Field::State), state);
                assert_eq!(
                    [f.user, f.makers[0], f.makers[1], f.rfq]
                        .map(|owner| [f.balance(true, owner), f.balance(false, owner)]),
                    balances.as_slice()
                );
            }
            for claim in claims {
                f.process(claim);
            }
            assert_eq!(f.balance(true, f.user), if buyer { 1_010 } else { 990 });
            assert_eq!(f.balance(false, f.user), if buyer { 920 } else { 1_120 });
            assert_eq!(
                f.balance(true, f.makers[0]),
                if buyer { 990 } else { 1_010 }
            );
            assert_eq!(
                f.balance(false, f.makers[0]),
                if buyer { 1_080 } else { 880 }
            );
            assert_eq!(f.balance(true, f.makers[1]), 1_000);
            assert_eq!(f.balance(false, f.makers[1]), 1_000);
            assert_eq!(f.balance(true, f.rfq), 0);
            assert_eq!(f.balance(false, f.rfq), 0);
            assert_eq!(f.value(Field::BidCount), 0);
            assert_eq!(f.value(Field::CanClose), 1);
        }
    }
}

/// The taker requests one more token than their balance on the required leg.
/// Creation succeeds, but funding must privately mark the RFQ Invalid, retain no
/// collateral, and prevent later maker deposits from becoming accepted bids.
#[test]
fn underfunded_taker() {
    for buyer in [true, false] {
        let mut f = RfqFixture::new(
            buyer,
            if buyer { 10 } else { 1_001 },
            if buyer { 1_001 } else { 100 },
            DEADLINE,
        );
        f.create();
        assert_eq!(f.value(Field::State), RFQState::Unfunded as u64);
        f.fund();
        assert_eq!(f.value(Field::State), RFQState::Invalid as u64);
        assert_eq!(f.value(Field::CanClose), 1);
        assert_eq!(f.balance(true, f.user), 1_000);
        assert_eq!(f.balance(false, f.user), 1_000);
        assert_eq!(f.balance(true, f.rfq), 0);
        assert_eq!(f.balance(false, f.rfq), 0);

        let bid = f.bid(1, 120, 80);
        f.process(bid);
        assert_eq!(f.value(Field::BidCount), 0);
        assert_eq!(f.maker_value(1, MakerPrivateField::Buy), 0);
        assert_eq!(f.maker_value(1, MakerPrivateField::Sell), 0);
        assert_eq!(f.balance(true, f.makers[0]), 1_000);
        assert_eq!(f.balance(false, f.makers[0]), 1_000);
    }
}

/// A maker funds one quote but lacks balance for the quote matching the taker's
/// side. The funded side may be accepted, but the unbacked side must never win.
/// Expiry and claims must return all collateral because there is no eligible bid.
#[test]
fn underfunded_maker() {
    for buyer in [true, false] {
        let mut f = RfqFixture::new(buyer, if buyer { 1_001 } else { 10 }, 100, DEADLINE);
        f.open();
        // Only the quote relevant to the user's side is underfunded. The other
        // side succeeds, so settlement must distinguish it from a winning quote.
        let bid = f.bid(1, if buyer { 120 } else { 1_001 }, 80);
        f.process(bid);
        assert_eq!(
            f.maker_value(1, MakerPrivateField::Buy),
            if buyer { 120 } else { 0 }
        );
        assert_eq!(
            f.maker_value(1, MakerPrivateField::Sell),
            if buyer { 0 } else { 80 }
        );
        assert_eq!(f.value(Field::BidCount), 1);
        f.set_clock(DEADLINE);
        f.process(f.expire());
        f.process(f.scan(1));
        assert_eq!(f.value(Field::BestMaker), 0);
        f.process(f.user_claim(false));
        f.process(f.maker_claim(1));
        for owner in [f.user, f.makers[0]] {
            assert_eq!(f.balance(true, owner), 1_000);
            assert_eq!(f.balance(false, owner), 1_000);
        }
        assert_eq!(f.balance(true, f.rfq), 0);
        assert_eq!(f.balance(false, f.rfq), 0);
        assert_eq!(f.value(Field::CanClose), 1);
    }
}

/// The second deposit carries an attestation signed for the wrong program.
/// Host verification must reject it and roll back the first transfer, maker-store
/// creation, rent spending, and counters in the same transaction.
#[test]
fn bid_rollback() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let mut bid = f.bid(1, 120, 80);
    let mut args = instruction::PlaceBid::deserialize(
        &mut &bid.data[instruction::PlaceBid::DISCRIMINATOR.len()..],
    )
    .unwrap();
    args.basis_transfer_attestation = kit::signing::amount_attestation_for(
        args.basis_transfer_attestation.input_handle,
        f.makers[0],
        confidential_rfq::ID,
    )
    .into();
    bid.data = args.data();
    let before = f.context.account_store.borrow().clone();
    kit::transaction::process_fhe_instruction(
        &f.context,
        f.user,
        &bid,
        &[kit::anchor_error_check(
            zama_host::ZamaHostError::InvalidInputAttestation as u32,
        )],
    );
    assert_eq!(*f.context.account_store.borrow(), before);
    assert_eq!(f.balance(true, f.makers[0]), 1_000);
    assert_eq!(f.balance(false, f.makers[0]), 1_000);
    assert_eq!(f.value(Field::BidCount), 0);
    assert_eq!({ f.state().open_stores }, 1);
}

/// A maker repeatedly submits, cancels, and resubmits zero quotes.
/// One maker must occupy only one store, and canceled quotes must not count toward
/// scanning. The remaining bid must settle without spending the bid-rent reserve.
#[test]
fn quote_spam() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let bid = f.bid(1, 0, 80);
    f.process(bid);
    for _ in 0..4 {
        for (buy, sell) in [(1, 1), (0, 0), (0, 0)] {
            let bid = f.bid(2, buy, sell);
            f.process(bid);
            assert_eq!(f.value(Field::BidCount), if buy == 0 { 1 } else { 2 });
            assert_eq!({ f.state().open_stores }, 3);
        }
    }
    assert_eq!(f.balance(true, f.makers[1]), 1_000);
    assert_eq!(f.balance(false, f.makers[1]), 1_000);
    f.set_clock(DEADLINE);
    f.process(f.expire());
    let reserve = f.context.account_store.borrow()[&f.rfq_funder].lamports;
    for _ in 0..3 {
        f.process(f.scan(2));
        assert_eq!(f.value(Field::SearchedBids), 0);
        assert_eq!(f.value(Field::State), RFQState::Expired as u64);
    }
    f.process(f.scan(1));
    assert_eq!(f.value(Field::SearchedBids), 1);
    assert_eq!(f.value(Field::BestMaker), 1);
    f.process(f.user_claim(false));
    f.process(f.maker_claim(1));
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
    assert_eq!(
        f.context.account_store.borrow()[&f.rfq_funder].lamports,
        reserve
    );
}

/// Pause between scans and again after the winning maker claims.
/// A maker may trigger expiry; an unrelated funded caller must then be able to
/// finish scanning and all claims without taker or winner signatures, releasing
/// every escrow balance even when the original participants stop submitting.
#[test]
fn resume_settlement() {
    for buyer in [true, false] {
        let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
        f.open();
        for (id, buy, sell) in [(1, 120, 80), (2, 110, 90)] {
            let bid = f.bid(id, buy, sell);
            f.process(bid);
        }
        f.set_clock(DEADLINE);
        // A maker can expire the RFQ even when the taker is absent.
        let mut expire = f.expire();
        expire.accounts[0].pubkey = f.makers[0];
        expire.data = instruction::ExpireRfq { maker_id: Some(1) }.data();
        let relayer = Pubkey::new_unique();
        f.context
            .account_store
            .borrow_mut()
            .insert(relayer, kit::funded_system_account());
        let mut first_scan = f.scan(2);
        first_scan.accounts[0].pubkey = relayer;
        let mut last_scan = f.scan(1);
        last_scan.accounts[0].pubkey = relayer;
        let mut winner_claim = f.maker_claim(1);
        winner_claim.accounts[0].pubkey = relayer;
        let mut user_claim = f.user_claim(false);
        user_claim.accounts[0].pubkey = relayer;
        let mut loser_claim = f.maker_claim(2);
        loser_claim.accounts[0].pubkey = relayer;

        for (step, mut ix) in [
            expire,
            first_scan,
            last_scan,
            winner_claim,
            user_claim,
            loser_claim,
        ]
        .into_iter()
        .enumerate()
        {
            // Every step is its own transaction with its own payer. No user or
            // winning-maker signature is needed to resume scanning or claims.
            let payer = ix.accounts[0].pubkey;
            for meta in &mut ix.accounts {
                if meta.pubkey == zama_host::transient_store_address(f.user).0 {
                    meta.pubkey = zama_host::transient_store_address(payer).0;
                }
            }
            if step == 2 || step == 4 {
                f.set_clock(DEADLINE + 86_400 * step as u64);
            }
            let result = kit::transaction::process_fhe_instruction(
                &f.context,
                payer,
                &ix,
                &[mollusk_svm::result::Check::success()],
            );
            f.ledger.replay_fhe_cpis(&f.context, &result);
            if step == 1 {
                assert_eq!(f.value(Field::State), RFQState::Expired as u64);
                assert_eq!(f.value(Field::SearchedBids), 1);
            } else if step == 3 {
                assert_eq!(f.value(Field::State), RFQState::Claimable as u64);
                assert_eq!(f.value(Field::CanClose), 0);
            }
        }
        assert_eq!(f.balance(true, f.user), if buyer { 1_010 } else { 990 });
        assert_eq!(f.balance(false, f.user), if buyer { 920 } else { 1_120 });
        assert_eq!(
            f.balance(true, f.makers[0]),
            if buyer { 990 } else { 1_010 }
        );
        assert_eq!(
            f.balance(false, f.makers[0]),
            if buyer { 1_080 } else { 880 }
        );
        assert_eq!(f.balance(true, f.makers[1]), 1_000);
        assert_eq!(f.balance(false, f.makers[1]), 1_000);
        assert_eq!(f.balance(true, f.rfq), 0);
        assert_eq!(f.balance(false, f.rfq), 0);
        assert_eq!(f.value(Field::CanClose), 1);
    }
}

/// Replace the token program with another executable program during claims.
/// Anchor must reject its program ID before any payout or state change, preventing
/// a caller from directing token CPIs to a different implementation.
#[test]
fn program_substitution() {
    let f = ready_to_settle(true);
    let before = f.context.account_store.borrow().clone();
    for mut ix in [f.user_claim(false), f.maker_claim(1)] {
        // Another executable program is still not the configured token program.
        for meta in &mut ix.accounts {
            if meta.pubkey == confidential_token::ID {
                meta.pubkey = zama_host::ID;
            }
        }
        kit::transaction::process_fhe_instruction(
            &f.context,
            f.user,
            &ix,
            &[mollusk_svm::result::Check::err(
                anchor_lang::solana_program::program_error::ProgramError::Custom(
                    anchor_lang::error::ErrorCode::InvalidProgramId as u32,
                ),
            )],
        );
        assert_eq!(*f.context.account_store.borrow(), before);
    }
}
