mod support;

use confidential_rfq::state::rfq::{RFQPrivateField, RFQState};
use std::collections::BTreeSet;
use support::{DEADLINE, RfqFixture};

/// Walk a deterministic serial address stream: PDA bump searches vary with the
/// keys, while successive bids grow the same stores' permission histories.
#[test]
fn funding_and_bids_fit_the_transaction_budget_across_address_sets() {
    const LIMIT: u64 = 1_400_000;
    const OPERATIONS: [&str; 6] = [
        "create",
        "fund",
        "first_bid",
        "increase",
        "decrease",
        "cancel",
    ];
    let mut maximum = [0; OPERATIONS.len()];
    let mut bumps = BTreeSet::new();

    for address_set in 0..12 {
        let buyer = address_set % 2 == 0;
        let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
        assert_eq!(f.context.mollusk.compute_budget.compute_unit_limit, LIMIT);
        let maker = f.makers[0];

        for (step, operation) in OPERATIONS.into_iter().enumerate() {
            let instruction = match step {
                0 => f.request.clone(),
                1 => f.funding.clone(),
                2 => f.bid(1, 90, 110),
                3 => f.bid(1, 120, 130),
                4 => f.bid(1, 50, 0),
                5 => f.bid(1, 0, 0),
                _ => unreachable!(),
            };
            eprintln!("address_set={address_set} buyer={buyer} operation={operation}");
            // process() requires success and executes the real open/body/close
            // FHE envelope; its CU count includes all three instructions.
            let consumed = f.process(instruction).compute_units_consumed;
            eprintln!("consumed={consumed} limit={LIMIT}");
            maximum[step] = maximum[step].max(consumed);
            assert!(
                consumed <= LIMIT,
                "{operation} exceeded the transaction budget"
            );

            if step == 0 {
                let state = f.state();
                bumps.insert((state.bump, state.funder_bump));
                assert_eq!(f.value(RFQPrivateField::State), RFQState::Unfunded as u64);
            } else if step == 1 {
                assert_eq!(f.value(RFQPrivateField::State), RFQState::Valid as u64);
            } else {
                let (asset_collateral, basis_collateral) = match step {
                    2 => (10, 90),
                    3 => (10, 120),
                    4 => (0, 50),
                    5 => (0, 0),
                    _ => unreachable!(),
                };
                assert_eq!(f.balance(true, maker), 1_000 - asset_collateral);
                assert_eq!(f.balance(false, maker), 1_000 - basis_collateral);
                assert_eq!(f.value(RFQPrivateField::BidCount), u64::from(step != 5));
                assert_eq!(f.value(RFQPrivateField::BidSeq), (step - 1) as u64);
            }
        }
        assert_eq!(f.balance(true, f.rfq), if buyer { 0 } else { 10 });
        assert_eq!(f.balance(false, f.rfq), if buyer { 100 } else { 0 });
    }

    assert!(
        bumps.len() > 1,
        "address sets must exercise different PDA bumps"
    );
    for (operation, consumed) in OPERATIONS.into_iter().zip(maximum) {
        eprintln!("maximum {operation}: {consumed}/{LIMIT} CU");
    }
}
