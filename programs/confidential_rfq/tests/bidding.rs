mod support;

use confidential_rfq::{
    state::rfq::{MakerPrivateField, RFQPrivateField, RFQState},
    util::pda::maker_store_address,
};
use support::{DEADLINE, RfqFixture};

fn maker_value(fixture: &RfqFixture, maker_id: u32, field: MakerPrivateField) -> u64 {
    let authority = maker_store_address(fixture.rfq, maker_id).0;
    let store =
        zama_host::encrypted_store_address(confidential_rfq::ID, authority, fixture.state().nonce)
            .0;
    fixture.store_value(store, field.key())
}

#[test]
fn replacements_and_cancellation_adjust_only_the_required_collateral() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let maker = f.makers[0];
    let maker_lamports = f.context.account_store.borrow()[&maker].lamports;

    let bid = f.bid(1, 90, 110);
    f.process(bid);
    assert_eq!(f.balance(true, maker), 990);
    assert_eq!(f.balance(false, maker), 910);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
    assert!(f.state().open_stores == 2);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sequence), 1);

    // Changing the requested sell payment never changes its size collateral.
    let replacement = f.bid(1, 120, 130);
    f.process(replacement);
    assert_eq!(f.balance(true, maker), 990);
    assert_eq!(f.balance(false, maker), 880);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
    assert!(f.state().open_stores == 2);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sequence), 2);

    let decrease = f.bid(1, 50, 0);
    f.process(decrease);
    assert_eq!(f.balance(true, maker), 1_000);
    assert_eq!(f.balance(false, maker), 950);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);

    let cancel = f.bid(1, 0, 0);
    f.process(cancel);
    assert_eq!(f.balance(true, maker), 1_000);
    assert_eq!(f.balance(false, maker), 1_000);
    assert_eq!(f.value(RFQPrivateField::BidCount), 0);
    assert_eq!(f.value(RFQPrivateField::BidSeq), 4);
    assert!(f.state().open_stores == 2);
    assert_eq!(
        f.context.account_store.borrow()[&maker].lamports,
        maker_lamports
    );

    let second_maker = f.bid(2, 60, 80);
    f.process(second_maker);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
    assert_eq!(maker_value(&f, 2, MakerPrivateField::Sequence), 5);
    assert!(f.state().open_stores == 3);
}

#[test]
fn failed_funding_preserves_old_quotes_and_other_side_can_succeed() {
    let mut f = RfqFixture::new(false, 10, 100, DEADLINE);
    f.open();
    let maker = f.makers[0];
    let initial = f.bid(1, 100, 0);
    f.process(initial);
    let unfunded = f.bid(1, 1_100, 120);
    f.process(unfunded);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Buy), 100);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sell), 120);
    assert_eq!(f.balance(false, maker), 900);
    assert_eq!(f.balance(true, maker), 990);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sequence), 2);
}

#[test]
fn malformed_deposits_are_refunded_without_replacing_old_collateral() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let maker = f.makers[0];
    let initial = f.bid(1, 100, 120);
    f.process(initial);
    let malformed = f.bid_with_deposits(1, 150, 0, 1, 51);
    f.process(malformed);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Buy), 100);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sell), 120);
    assert_eq!(f.balance(false, maker), 900);
    assert_eq!(f.balance(true, maker), 990);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
}

#[test]
fn deadline_is_private_and_late_deposits_are_fully_refunded() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let maker = f.makers[0];
    let initial = f.bid(1, 100, 120);
    f.process(initial);
    f.set_clock(DEADLINE);
    let late = f.bid(1, 150, 80);
    f.process(late);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Buy), 100);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sell), 120);
    assert_eq!(f.balance(false, maker), 900);
    assert_eq!(f.balance(true, maker), 990);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sequence), 2);
}

#[test]
fn bids_before_funding_refund_both_deposits_and_do_not_gain_priority() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.create();
    let maker = f.makers[0];
    assert_eq!(f.value(RFQPrivateField::State), RFQState::Unfunded as u64);

    // Both transfers succeed, but an unfunded RFQ cannot accept either quote.
    let early = f.bid(1, 90, 110);
    f.process(early);
    assert_eq!(f.balance(true, maker), 1_000);
    assert_eq!(f.balance(false, maker), 1_000);
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Buy), 0);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sell), 0);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sequence), 0);
    assert_eq!(f.value(RFQPrivateField::BidCount), 0);
    assert_eq!(f.value(RFQPrivateField::BidSeq), 0);
    assert_eq!(f.value(RFQPrivateField::State), RFQState::Unfunded as u64);
    assert_eq!({ f.state().open_stores }, 2);

    f.fund();
    assert_eq!(f.value(RFQPrivateField::State), RFQState::Valid as u64);
    let funded = f.bid(1, 90, 110);
    f.process(funded);
    assert_eq!(f.balance(true, maker), 990);
    assert_eq!(f.balance(false, maker), 910);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Buy), 90);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sell), 110);
    assert_eq!(maker_value(&f, 1, MakerPrivateField::Sequence), 1);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
    assert_eq!(f.value(RFQPrivateField::BidSeq), 1);
    assert_eq!({ f.state().open_stores }, 2);
}

#[test]
fn switching_quote_sides_counts_each_active_maker_once() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.open();
    let first_sell = f.bid(1, 0, 80);
    f.process(first_sell);
    let second_buy = f.bid(2, 70, 0);
    f.process(second_buy);
    assert_eq!(f.value(RFQPrivateField::BidCount), 2);

    // The old sell activity must survive privately until the basis phase has
    // replaced it with a buy; otherwise this transition would count twice.
    let switch_to_buy = f.bid(1, 60, 0);
    f.process(switch_to_buy);
    assert_eq!(f.value(RFQPrivateField::BidCount), 2);
    assert_eq!(f.balance(true, f.makers[0]), 1_000);
    assert_eq!(f.balance(false, f.makers[0]), 940);

    let switch_to_sell = f.bid(1, 0, 90);
    f.process(switch_to_sell);
    assert_eq!(f.value(RFQPrivateField::BidCount), 2);
    assert_eq!(f.balance(true, f.makers[0]), 990);
    assert_eq!(f.balance(false, f.makers[0]), 1_000);

    let cancel_first = f.bid(1, 0, 0);
    f.process(cancel_first);
    assert_eq!(f.value(RFQPrivateField::BidCount), 1);
    let cancel_second = f.bid(2, 0, 0);
    f.process(cancel_second);
    assert_eq!(f.value(RFQPrivateField::BidCount), 0);
}
