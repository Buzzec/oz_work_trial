mod support;

use confidential_rfq::state::rfq::{MakerPrivateField, RFQPrivateField as Field, RFQState};
use solana_sdk::instruction::Instruction;
use support::{DEADLINE, RfqFixture};

/// Reach every state through real instructions. Funded scenarios retain a live,
/// two-sided maker position so rejected transitions must preserve recoverable funds.
fn at_state(buyer: bool, target: RFQState) -> RfqFixture {
    let mut f = RfqFixture::new(
        buyer,
        if target == RFQState::Invalid { 0 } else { 10 },
        100,
        DEADLINE,
    );
    f.create();
    if target != RFQState::Unfunded {
        f.fund();
        if target != RFQState::Invalid {
            let bid = f.bid(1, 120, 80);
            f.process(bid);
            match target {
                RFQState::Canceled => {
                    f.process(f.user_claim(true));
                }
                RFQState::Expired | RFQState::Claimable | RFQState::Claimed => {
                    f.set_clock(DEADLINE);
                    f.process(f.expire());
                    if target != RFQState::Expired {
                        f.process(f.scan(1));
                    }
                    if target == RFQState::Claimed {
                        f.process(f.user_claim(false));
                    }
                }
                RFQState::Valid => {}
                RFQState::Invalid | RFQState::Unfunded => unreachable!(),
            }
        }
    }
    assert_eq!(f.value(Field::State), target as u64);
    f
}

/// Compare semantic values, since a successful encrypted no-op can replace handles
/// and append history while leaving the private state and token balances unchanged.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    fields: [u64; 12],
    maker: Option<[u64; 3]>,
    balances: Vec<[u64; 2]>,
}

impl Snapshot {
    fn read(f: &RfqFixture) -> Self {
        let fields = [
            Field::State,
            Field::ExpireTimestamp,
            Field::BidCount,
            Field::BidSeq,
            Field::SearchedBids,
            Field::UserBuyer,
            Field::OfferLimit,
            Field::Size,
            Field::BestOffer,
            Field::BestMaker,
            Field::BestMakerIndex,
            Field::CanClose,
        ]
        .map(|field| f.value(field));
        let maker = (f.state().open_stores > 1).then(|| {
            let store = f.maker_store(1).1;
            [
                MakerPrivateField::Buy,
                MakerPrivateField::Sell,
                MakerPrivateField::Sequence,
            ]
            .map(|field| f.store_value(store, field.key()))
        });
        let balances = std::iter::once(f.user)
            .chain(f.makers.iter().copied())
            .chain(std::iter::once(f.rfq))
            .map(|owner| [f.balance(true, owner), f.balance(false, owner)])
            .collect();
        Self {
            fields,
            maker,
            balances,
        }
    }
}

/// Complete the remaining legitimate path after the tested action and prove that
/// all escrow is paid out once, including collateral retained in Canceled/Claimed.
fn recover_funds(mut f: RfqFixture, buyer: bool, state: RFQState) {
    if state == RFQState::Unfunded {
        f.process(f.user_claim(true));
    }
    if state == RFQState::Valid {
        f.set_clock(DEADLINE + 1);
        f.process(f.expire());
    }
    if matches!(state, RFQState::Valid | RFQState::Expired) {
        f.process(f.scan(1));
    }
    if matches!(
        state,
        RFQState::Valid | RFQState::Expired | RFQState::Claimable
    ) {
        f.process(f.user_claim(false));
    }
    let has_maker = f.state().open_stores > 1;
    if has_maker {
        f.process(f.maker_claim(1));
    }

    let traded = f.value(Field::BestMaker) != 0;
    let user = if traded && buyer {
        [1_010, 920]
    } else if traded {
        [990, 1_120]
    } else {
        [1_000, 1_000]
    };
    let maker = if traded && buyer {
        [990, 1_080]
    } else if traded {
        [1_010, 880]
    } else {
        [1_000, 1_000]
    };
    assert_eq!([f.balance(true, f.user), f.balance(false, f.user)], user);
    assert_eq!(
        [f.balance(true, f.makers[0]), f.balance(false, f.makers[0])],
        maker
    );
    for owner in &f.makers[1..] {
        assert_eq!(
            [f.balance(true, *owner), f.balance(false, *owner)],
            [1_000, 1_000]
        );
    }
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
    assert_eq!(f.value(Field::BidCount), 0);
    assert_eq!(f.value(Field::CanClose), 1);

    let settled = Snapshot::read(&f);
    f.process(f.user_claim(false));
    f.process(f.user_claim(true));
    if has_maker {
        f.process(f.maker_claim(1));
    }
    assert_eq!(
        Snapshot::read(&f),
        settled,
        "repeated claims must not pay again"
    );
}

/// Test a forbidden transition after the deadline, when the timestamp predicate
/// alone would allow expiry. Both calls must succeed privately without moving funds.
fn unchanged_in(state: RFQState, action: impl Fn(&RfqFixture) -> Instruction) {
    for buyer in [true, false] {
        let mut f = at_state(buyer, state);
        f.set_clock(DEADLINE + 1);
        let before = Snapshot::read(&f);
        for _ in 0..2 {
            f.process(action(&f));
            assert_eq!(Snapshot::read(&f), before, "state {state:?}, buyer {buyer}");
        }
        recover_funds(f, buyer, state);
    }
}

/// Expire an Invalid request after its deadline. The transaction succeeds privately
/// but must leave state and balances unchanged because no funded RFQ exists.
#[test]
fn expiry_invalid() {
    unchanged_in(RFQState::Invalid, RfqFixture::expire);
}

/// Cancel an Invalid request twice. Both calls must leave it unchanged: its funding
/// was already returned, so cancellation cannot create another refund.
#[test]
fn cancel_invalid() {
    unchanged_in(RFQState::Invalid, |f| f.user_claim(true));
}

/// Expire an Unfunded request after its deadline. It must remain Unfunded because
/// no collateral backs a claim; the user must still be able to cancel it.
#[test]
fn expiry_unfunded() {
    unchanged_in(RFQState::Unfunded, RfqFixture::expire);
}

/// Cancel an Unfunded request before, at, and after its deadline on both sides.
/// Every call must enter Canceled without paying tokens because funding never ran.
#[test]
fn cancel_unfunded() {
    for buyer in [true, false] {
        for now in [DEADLINE - 1, DEADLINE, DEADLINE + 1] {
            let mut f = at_state(buyer, RFQState::Unfunded);
            f.set_clock(now);
            let balances = Snapshot::read(&f).balances;
            f.process(f.user_claim(true));
            assert_eq!(f.value(Field::State), RFQState::Canceled as u64);
            assert_eq!(Snapshot::read(&f).balances, balances);
            assert_eq!(f.value(Field::CanClose), 1);
            recover_funds(f, buyer, RFQState::Canceled);
        }
    }
}

/// Try to expire a funded request immediately before its deadline.
/// The FHE predicate must preserve the request and collateral until expiry is due,
/// after which normal settlement must still release all funds.
#[test]
fn expiry_early() {
    for buyer in [true, false] {
        let mut f = at_state(buyer, RFQState::Valid);
        f.set_clock(DEADLINE - 1);
        let before = Snapshot::read(&f);
        f.process(f.expire());
        assert_eq!(Snapshot::read(&f), before);
        recover_funds(f, buyer, RFQState::Valid);
    }
}

/// With a live bid, expiry starts scanning; it does not release user or maker collateral.
fn expire_valid_at(now: u64) {
    for buyer in [true, false] {
        let mut f = at_state(buyer, RFQState::Valid);
        f.set_clock(now);
        let balances = Snapshot::read(&f).balances;
        f.process(f.expire());
        assert_eq!(f.value(Field::State), RFQState::Expired as u64);
        assert_eq!(f.value(Field::BidCount), 1);
        assert_eq!(f.value(Field::SearchedBids), 0);
        assert_eq!(f.value(Field::CanClose), 0);
        assert_eq!(Snapshot::read(&f).balances, balances);
        recover_funds(f, buyer, RFQState::Expired);
    }
}

/// Expire a funded request exactly at its deadline with an active bid.
/// The inclusive expiry comparison must enter Expired and retain collateral
/// until winner scanning and claims complete.
#[test]
fn expiry_at_deadline() {
    expire_valid_at(DEADLINE);
}

/// Expire a funded request after its deadline with an active bid.
/// It must enter Expired without paying either party prematurely, and subsequent
/// scanning and claims must release all collateral.
#[test]
fn expiry_after_deadline() {
    expire_valid_at(DEADLINE + 1);
}

/// Expire a funded request with no bids at and after its deadline.
/// It must become Claimable immediately, allowing the user to recover all
/// collateral without requiring a maker store or a scan.
#[test]
fn expiry_without_bids() {
    for buyer in [true, false] {
        for now in [DEADLINE, DEADLINE + 1] {
            let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
            f.open();
            f.set_clock(now);
            let balances = Snapshot::read(&f).balances;
            f.process(f.expire());
            assert_eq!(f.value(Field::State), RFQState::Claimable as u64);
            assert_eq!(Snapshot::read(&f).balances, balances);
            recover_funds(f, buyer, RFQState::Claimable);
        }
    }
}

/// Cancel a funded request immediately before expiry. The user must receive a
/// full refund, while maker collateral remains available to a separate maker
/// claim. Closure must stay disabled until that remaining claim is paid.
#[test]
fn cancel_before_deadline() {
    for buyer in [true, false] {
        let mut f = at_state(buyer, RFQState::Valid);
        f.set_clock(DEADLINE - 1);
        let maker = [f.balance(true, f.makers[0]), f.balance(false, f.makers[0])];
        f.process(f.user_claim(true));
        assert_eq!(f.value(Field::State), RFQState::Canceled as u64);
        assert_eq!(
            [f.balance(true, f.user), f.balance(false, f.user)],
            [1_000, 1_000]
        );
        assert_eq!(
            [f.balance(true, f.makers[0]), f.balance(false, f.makers[0])],
            maker
        );
        assert_eq!(f.value(Field::BidCount), 1);
        assert_eq!(f.value(Field::CanClose), 0);
        recover_funds(f, buyer, RFQState::Canceled);
    }
}

/// Cancel a funded request exactly at its deadline. Cancellation must be an
/// encrypted no-op because its time window has ended; expiry and settlement
/// must still complete using the original collateral.
#[test]
fn cancel_at_deadline() {
    for buyer in [true, false] {
        let mut f = at_state(buyer, RFQState::Valid);
        f.set_clock(DEADLINE);
        let before = Snapshot::read(&f);
        f.process(f.user_claim(true));
        assert_eq!(Snapshot::read(&f), before);
        recover_funds(f, buyer, RFQState::Valid);
    }
}

/// Cancel a funded request after its deadline twice. Both calls must preserve
/// its state and collateral so the user cannot withdraw ahead of settlement.
#[test]
fn cancel_after_deadline() {
    unchanged_in(RFQState::Valid, |f| f.user_claim(true));
}

/// Expire an already Canceled request. It must stay Canceled, preserving the
/// completed user refund and the maker collateral still awaiting recovery.
#[test]
fn expiry_canceled() {
    unchanged_in(RFQState::Canceled, RfqFixture::expire);
}

/// Cancel an already Canceled request twice. Neither call may pay the user again
/// or consume collateral that must remain available to the maker.
#[test]
fn cancel_canceled() {
    unchanged_in(RFQState::Canceled, |f| f.user_claim(true));
}

/// Expire an already Expired request twice. It must preserve unscanned bids and
/// collateral so repeated expiry cannot reset or bypass winner calculation.
#[test]
fn expiry_expired() {
    unchanged_in(RFQState::Expired, RfqFixture::expire);
}

/// Cancel an Expired request before scanning. The state and balances must remain
/// unchanged because collateral is committed to the settlement path.
#[test]
fn cancel_expired() {
    unchanged_in(RFQState::Expired, |f| f.user_claim(true));
}

/// Expire a Claimable request with a selected winner. It must retain the winner
/// and both pending payouts rather than restarting scanning or releasing refunds.
#[test]
fn expiry_claimable() {
    unchanged_in(RFQState::Claimable, RfqFixture::expire);
}

/// Cancel a Claimable request. Cancellation must leave the selected trade intact;
/// otherwise the taker could replace an owed payment with a refund.
#[test]
fn cancel_claimable() {
    unchanged_in(RFQState::Claimable, |f| f.user_claim(true));
}

/// Expire a request after the user has claimed but before the maker has.
/// It must preserve the completed user payout and allow the outstanding maker
/// claim without reopening settlement.
#[test]
fn expiry_claimed() {
    unchanged_in(RFQState::Claimed, RfqFixture::expire);
}

/// Cancel a request after the user has claimed. It must neither refund the user
/// again nor spend the maker collateral still awaiting its legitimate claim.
#[test]
fn cancel_claimed() {
    unchanged_in(RFQState::Claimed, |f| f.user_claim(true));
}
