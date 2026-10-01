mod support;

use confidential_rfq::state::rfq::{MakerPrivateField, RFQPrivateField as Field, RFQState};
use support::{DEADLINE, NOW, RfqFixture};
use zama_solana_test_kit as kit;

#[test]
fn unfunded_quotes_cannot_expire_or_pay_claims_and_can_be_canceled_after_deadline() {
    for buyer in [true, false] {
        let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
        f.create();
        assert_eq!(f.value(Field::State), RFQState::Unfunded as u64);
        assert_eq!(f.value(Field::CanClose), 0);
        f.set_clock(DEADLINE);
        f.process(f.expire());
        f.process(f.user_claim(false));
        assert_eq!(f.value(Field::State), RFQState::Unfunded as u64);
        assert_eq!(f.value(Field::CanClose), 0);
        f.process(f.user_claim(true));
        assert_eq!(f.value(Field::State), RFQState::Canceled as u64);
        assert_eq!(f.value(Field::CanClose), 1);
        f.process(f.user_claim(true));
        f.process(f.user_claim(false));
        for asset in [true, false] {
            assert_eq!(f.balance(asset, f.user), 1_000);
            assert_eq!(f.balance(asset, f.rfq), 0);
        }
    }
}

#[test]
fn canceled_unfunded_quotes_cannot_be_reactivated_by_funding() {
    for buyer in [true, false] {
        let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
        f.create();
        f.process(f.user_claim(true));
        assert_eq!(f.value(Field::State), RFQState::Canceled as u64);
        assert_eq!(f.value(Field::CanClose), 1);
        f.fund();
        assert_eq!(f.value(Field::State), RFQState::Canceled as u64);
        assert_eq!(f.value(Field::BidCount), 0);
        assert_eq!(f.value(Field::CanClose), 1);
        for asset in [true, false] {
            assert_eq!(f.balance(asset, f.user), 1_000);
            assert_eq!(f.balance(asset, f.rfq), 0);
        }
    }
}

#[test]
fn empty_auction_expiry_and_user_claim_are_private_and_repeatable() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let bid_reserve = f
        .context
        .account_store
        .borrow()
        .get(&f.rfq_funder)
        .unwrap()
        .lamports;
    f.process(f.expire());
    f.process(f.user_claim(false));
    assert_eq!(f.value(Field::State), RFQState::Valid as u64);
    assert_eq!(f.balance(false, f.user), 900);
    f.set_clock(DEADLINE);
    f.process(f.expire());
    assert_eq!(f.value(Field::State), RFQState::Claimable as u64);
    f.process(f.user_claim(false));
    assert_eq!(f.value(Field::State), RFQState::Claimed as u64);
    assert_eq!(f.value(Field::CanClose), 1);
    assert_eq!(f.balance(false, f.user), 1_000);
    assert_eq!(f.balance(true, f.user), 1_000);
    f.process(f.user_claim(false));
    assert_eq!(f.balance(false, f.user), 1_000);
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
    assert_eq!(
        f.context
            .account_store
            .borrow()
            .get(&f.rfq_funder)
            .unwrap()
            .lamports,
        bid_reserve
    );
}

#[test]
fn buyer_ties_follow_sequence_and_makers_can_claim_before_user() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    for (id, buy, sell) in [(1, 90, 80), (2, 0, 80), (3, 0, 95)] {
        let ix = f.bid(id, buy, sell);
        f.process(ix);
    }
    f.process(f.maker_claim(1));
    assert_eq!(f.value(Field::BidCount), 3);
    assert_eq!(f.balance(false, f.makers[0]), 910);
    f.set_clock(DEADLINE);
    f.process(f.expire());
    f.process(f.scan(2));
    assert_eq!(f.value(Field::BestMaker), 2);
    f.process(f.scan(2));
    assert_eq!(f.value(Field::SearchedBids), 1);
    f.process(f.scan(1));
    f.process(f.scan(3));
    assert_eq!(f.value(Field::State), RFQState::Claimable as u64);
    assert_eq!(f.value(Field::BestMaker), 1);
    assert_eq!(f.value(Field::BestOffer), 80);
    assert_eq!(f.value(Field::BestMakerIndex), 1);
    for id in 1..=3 {
        f.process(f.maker_claim(id));
    }
    assert_eq!(f.value(Field::BidCount), 0);
    assert_eq!(f.value(Field::CanClose), 0);
    assert_eq!(f.balance(true, f.makers[0]), 990);
    assert_eq!(f.balance(false, f.makers[0]), 1_080);
    assert_eq!(f.balance(true, f.makers[1]), 1_000);
    let maker_store = f.maker_store(1).1;
    assert_eq!(f.store_value(maker_store, MakerPrivateField::Buy.key()), 0);
    assert_eq!(f.store_value(maker_store, MakerPrivateField::Sell.key()), 0);
    f.process(f.maker_claim(1));
    assert_eq!(f.value(Field::BidCount), 0);
    assert_eq!(f.balance(false, f.makers[0]), 1_080);
    f.process(f.user_claim(false));
    assert_eq!(f.balance(true, f.user), 1_010);
    assert_eq!(f.balance(false, f.user), 920);
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
    assert_eq!(f.value(Field::CanClose), 1);
}

#[test]
fn seller_can_claim_first_and_winner_recovers_unused_asset_collateral() {
    let mut f = RfqFixture::new(false, 10, 100, DEADLINE);
    f.open();
    for (id, buy, sell) in [(1, 120, 150), (2, 110, 0)] {
        let ix = f.bid(id, buy, sell);
        f.process(ix);
    }
    f.set_clock(DEADLINE);
    f.process(f.expire());
    f.process(f.scan(1));
    f.process(f.scan(2));
    assert_eq!(f.value(Field::BestMaker), 1);
    f.process(f.user_claim(false));
    assert_eq!(f.value(Field::CanClose), 0);
    assert_eq!(f.balance(true, f.user), 990);
    assert_eq!(f.balance(false, f.user), 1_120);
    f.process(f.maker_claim(1));
    f.process(f.maker_claim(2));
    assert_eq!(f.balance(true, f.makers[0]), 1_010);
    assert_eq!(f.balance(false, f.makers[0]), 880);
    assert_eq!(f.balance(false, f.makers[1]), 1_000);
    assert_eq!(f.value(Field::CanClose), 1);
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
}

#[test]
fn cancellation_refunds_user_then_sell_only_maker_and_cannot_run_at_expiry() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let ix = f.bid(1, 0, 80);
    f.process(ix);
    f.process(f.user_claim(true));
    assert_eq!(f.value(Field::State), RFQState::Canceled as u64);
    assert_eq!(f.balance(false, f.user), 1_000);
    assert_eq!(f.value(Field::CanClose), 0);
    f.process(f.maker_claim(1));
    assert_eq!(f.balance(true, f.makers[0]), 1_000);
    assert_eq!(f.value(Field::BidCount), 0);
    assert_eq!(f.value(Field::CanClose), 1);
    f.process(f.user_claim(true));
    f.process(f.user_claim(false));
    assert_eq!(f.balance(false, f.user), 1_000);

    let mut f = RfqFixture::new(true, 10, 100, NOW);
    f.open();
    f.process(f.user_claim(true));
    assert_eq!(f.value(Field::State), RFQState::Valid as u64);
    assert_eq!(f.balance(false, f.user), 900);
    f.process(f.expire());
    f.process(f.user_claim(false));
    assert_eq!(f.balance(false, f.user), 1_000);
}

#[test]
fn quotes_at_the_limit_do_not_win_and_all_collateral_is_refunded() {
    for buyer in [true, false] {
        let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
        f.open();
        let ix = f.bid(1, 100, 100);
        f.process(ix);
        f.set_clock(DEADLINE);
        f.process(f.expire());
        f.process(f.scan(1));
        assert_eq!(f.value(Field::State), RFQState::Claimable as u64);
        assert_eq!(f.value(Field::BestMaker), 0);
        assert_eq!(f.value(Field::BestOffer), 0);
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

#[test]
fn permissionless_callers_cannot_redirect_payouts_or_cancel_another_users_quote() {
    use anchor_lang::solana_program::program_error::ProgramError;
    use confidential_rfq::ConfidentialRfqError;
    use mollusk_svm::result::Check;

    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let ix = f.bid(1, 0, 80);
    f.process(ix);
    for (mut ix, changed_account, replacement, error) in [
        (
            f.user_claim(true),
            0,
            f.makers[0],
            ConfidentialRfqError::InvalidUser,
        ),
        (
            f.user_claim(false),
            1,
            f.makers[0],
            ConfidentialRfqError::InvalidUser,
        ),
        (
            f.maker_claim(1),
            1,
            f.makers[1],
            ConfidentialRfqError::UnauthorizedMaker,
        ),
    ] {
        ix.accounts[changed_account].pubkey = replacement;
        kit::transaction::process_fhe_instruction(
            &f.context,
            f.user,
            &ix,
            &[Check::err(ProgramError::Custom(6_000 + error as u32))],
        );
    }
    assert_eq!(f.value(Field::State), RFQState::Valid as u64);
    assert_eq!(f.value(Field::BidCount), 1);
    assert_eq!(f.balance(false, f.user), 900);
    assert_eq!(f.balance(true, f.makers[0]), 990);
}
