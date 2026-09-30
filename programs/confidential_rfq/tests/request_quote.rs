mod support;
use anchor_lang::{
    AccountDeserialize, AnchorDeserialize, Discriminator, InstructionData, prelude::Pubkey,
};
use confidential_rfq::state::rfq::{MAXIMUM_TIMEOUT, RFQPrivateField as Field, RFQState};
use support::{DEADLINE, NOW, RfqFixture};
use zama_solana_test_kit as kit;

#[test]
fn creation_prepares_empty_escrows_and_leaves_valid_terms_unfunded() {
    for prepared in [false, true] {
        let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
        if prepared {
            f.prepare_escrows();
        }
        f.create();
        assert_eq!(f.value(Field::State), RFQState::Unfunded as u64);
        assert_eq!(f.value(Field::CanClose), 0);
        assert_eq!(f.value(Field::BidCount), 0);
        assert_eq!(f.balance(true, f.user), 1_000);
        assert_eq!(f.balance(false, f.user), 1_000);
        assert_eq!(f.balance(true, f.rfq), 0);
        assert_eq!(f.balance(false, f.rfq), 0);
        let open_stores = f.state().open_stores;
        assert_eq!(open_stores, 1);
    }
}

#[test]
fn funding_activates_only_the_private_side_and_preserves_the_bid_rent_reserve() {
    for buyer in [false, true] {
        let mut f = RfqFixture::new(buyer, 10, 100, DEADLINE);
        f.create();
        let reserve = f.context.account_store.borrow()[&f.rfq_funder].lamports;
        f.fund();
        assert_eq!(
            f.context.account_store.borrow()[&f.rfq_funder].lamports,
            reserve
        );
        assert_eq!(f.value(Field::State), RFQState::Valid as u64);
        assert_eq!(f.value(Field::ExpireTimestamp), DEADLINE);
        assert_eq!(f.value(Field::BidCount), 0);
        assert_eq!(f.value(Field::CanClose), 0);
        assert_eq!(f.balance(true, f.rfq), if buyer { 0 } else { 10 });
        assert_eq!(f.balance(false, f.rfq), if buyer { 100 } else { 0 });
        let count = f.state().open_stores;
        assert_eq!(count, 1);
    }
}

#[test]
fn funding_a_valid_or_invalid_quote_preserves_state_and_refunds_new_deposits() {
    for valid in [false, true] {
        let mut f = RfqFixture::new(true, if valid { 10 } else { 0 }, 100, DEADLINE);
        f.open();
        let balances = [
            f.balance(true, f.user),
            f.balance(false, f.user),
            f.balance(true, f.rfq),
            f.balance(false, f.rfq),
        ];
        let state = f.value(Field::State);
        let can_close = f.value(Field::CanClose);
        f.fund();
        assert_eq!(f.value(Field::State), state);
        assert_eq!(f.value(Field::CanClose), can_close);
        assert_eq!(f.value(Field::BidCount), 0);
        assert_eq!(
            [
                f.balance(true, f.user),
                f.balance(false, f.user),
                f.balance(true, f.rfq),
                f.balance(false, f.rfq),
            ],
            balances
        );
    }
}

#[test]
fn incorrect_funding_invalidates_an_unfunded_quote_and_refunds_both_sides() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.create();
    let mut ix = f.funding.clone();
    let mut args = confidential_rfq::instruction::FundQuote::deserialize(
        &mut &ix.data[confidential_rfq::instruction::FundQuote::DISCRIMINATOR.len()..],
    )
    .unwrap();
    let asset = kit::handle_for_chain(80, 5);
    let basis = kit::handle_for_chain(81, 5);
    f.ledger.seed_amount(asset, 3);
    f.ledger.seed_amount(basis, 90);
    args.asset_escrow =
        kit::signing::amount_attestation_for(asset, f.user, confidential_token::ID).into();
    args.basis_escrow =
        kit::signing::amount_attestation_for(basis, f.user, confidential_token::ID).into();
    ix.data = args.data();
    f.process(ix);
    assert_eq!(f.value(Field::State), RFQState::Invalid as u64);
    assert_eq!(f.value(Field::CanClose), 1);
    assert_eq!(f.balance(true, f.user), 1_000);
    assert_eq!(f.balance(false, f.user), 1_000);
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
}

#[test]
fn invalid_requests_refund_every_transferred_token() {
    for (size, limit, expiry) in [
        (0, 100, DEADLINE),
        (10, 0, DEADLINE),
        (10, 1001, DEADLINE),
        (1001, 100, DEADLINE),
        (10, 100, NOW + MAXIMUM_TIMEOUT),
    ] {
        for buyer in [false, true] {
            // Only the funded side's insufficiency invalidates a request.
            if (buyer && size == 1001) || (!buyer && limit == 1001) {
                continue;
            }
            let mut f = RfqFixture::new(buyer, size, limit, expiry);
            f.open();
            assert_eq!(f.value(Field::State), RFQState::Invalid as u64);
            assert_eq!(f.value(Field::CanClose), 1);
            assert_eq!(f.balance(true, f.user), 1000);
            assert_eq!(f.balance(false, f.user), 1000);
            assert_eq!(f.balance(true, f.rfq), 0);
            assert_eq!(f.balance(false, f.rfq), 0);
        }
    }
}

#[test]
fn private_past_deadline_does_not_reject_account_creation() {
    let mut f = RfqFixture::new(true, 10, 100, NOW - 1);
    f.create();
    assert_eq!(f.value(Field::State), RFQState::Unfunded as u64);
    f.fund();
    assert_eq!(f.value(Field::State), RFQState::Valid as u64);
    assert_eq!(f.value(Field::BidCount), 0);
}

#[test]
fn a_rejected_second_funding_input_rolls_back_the_first_deposit() {
    let mut f = RfqFixture::new(false, 10, 100, DEADLINE);
    f.create();
    let mut ix = f.funding.clone();
    let mut args = confidential_rfq::instruction::FundQuote::deserialize(
        &mut &ix.data[confidential_rfq::instruction::FundQuote::DISCRIMINATOR.len()..],
    )
    .unwrap();
    args.basis_escrow = kit::signing::amount_attestation_for(
        args.basis_escrow.input_handle,
        f.user,
        confidential_rfq::ID,
    )
    .into();
    ix.data = args.data();
    kit::transaction::process_fhe_instruction(
        &f.context,
        f.user,
        &ix,
        &[kit::anchor_error_check(
            zama_host::ZamaHostError::InvalidInputAttestation as u32,
        )],
    );
    assert_eq!(f.value(Field::State), RFQState::Unfunded as u64);
    assert_eq!(f.value(Field::CanClose), 0);
    assert_eq!(f.balance(true, f.user), 1_000);
    assert_eq!(f.balance(false, f.user), 1_000);
    assert_eq!(f.balance(true, f.rfq), 0);
    assert_eq!(f.balance(false, f.rfq), 0);
}

#[test]
fn compact_inputs_reject_signatures_bound_to_another_user_or_program() {
    for wrong_user in [true, false] {
        let f = RfqFixture::new(true, 10, 100, DEADLINE);
        let mut ix = f.request.clone();
        let mut args = confidential_rfq::instruction::RequestQuote::deserialize(
            &mut &ix.data[confidential_rfq::instruction::RequestQuote::DISCRIMINATOR.len()..],
        )
        .unwrap();
        let (signed_user, signed_program) = if wrong_user {
            (Pubkey::new_unique(), confidential_rfq::ID)
        } else {
            (f.user, confidential_token::ID)
        };
        args.user_buyer = kit::signing::amount_attestation_for(
            args.user_buyer.input_handle,
            signed_user,
            signed_program,
        )
        .into();
        ix.data = args.data();

        // Compact encoding removes the identities, but reconstructing the expected ones
        // must not turn a valid signature for another context into an accepted input.
        kit::transaction::process_fhe_instruction(
            &f.context,
            f.user,
            &ix,
            &[kit::anchor_error_check(
                zama_host::ZamaHostError::InvalidInputAttestation as u32,
            )],
        );
        assert!(f.context.account_store.borrow()[&f.rfq].data.is_empty());
        assert_eq!(f.balance(true, f.user), 1_000);
        assert_eq!(f.balance(false, f.user), 1_000);
        assert!(
            f.context.account_store.borrow()[&f.asset.token(f.rfq)]
                .data
                .is_empty()
        );
        assert!(
            f.context.account_store.borrow()[&f.basis.token(f.rfq)]
                .data
                .is_empty()
        );
    }
}

#[test]
fn prepared_escrow_requires_token_program_ownership_and_the_rfq_authority() {
    let mut f = RfqFixture::new(true, 10, 100, DEADLINE);
    f.prepare_escrows();
    let escrow = f.asset.token(f.rfq);
    let original = f.context.account_store.borrow()[&escrow].clone();
    for wrong_program_owner in [true, false] {
        let mut account = original.clone();
        if wrong_program_owner {
            account.owner = anchor_lang::system_program::ID;
        } else {
            let mut token = confidential_token::ConfidentialTokenAccount::try_deserialize(
                &mut account.data.as_slice(),
            )
            .unwrap();
            token.owner = f.makers[0];
            account.data = kit::serialized_account(token);
        }
        f.context.account_store.borrow_mut().insert(escrow, account);
        kit::transaction::process_fhe_instruction(
            &f.context,
            f.user,
            &f.request,
            &[kit::anchor_error_check(
                confidential_rfq::ConfidentialRfqError::InvalidRfqAccounts as u32,
            )],
        );
        assert!(f.context.account_store.borrow()[&f.rfq].data.is_empty());
        assert_eq!(f.balance(true, f.user), 1_000);
        assert_eq!(f.balance(false, f.user), 1_000);
        assert_eq!(f.balance(true, f.rfq), 0);
        assert_eq!(f.balance(false, f.rfq), 0);
    }
}
