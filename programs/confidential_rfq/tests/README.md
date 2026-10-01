# Test index

The tests cover RFQ creation, funding, bidding, settlement, recovery, permissions, and account cleanup. Runtime tests execute the compiled RFQ and Zama programs in Mollusk. A cleartext oracle evaluates the emitted FHE operations to check private state and balances.

## Where to look

| Area                    | File                                         | Main checks                                                                                              |
|-------------------------|----------------------------------------------|----------------------------------------------------------------------------------------------------------|
| Market membership       | [market.rs](market.rs)                       | Maker identities, decryption delegation, removal, reactivation, and rent.                                |
| Creation and funding    | [request_quote.rs](request_quote.rs)         | Empty escrows, the `Unfunded` state, collateral validation, input signatures, and rollback.              |
| Bidding                 | [bidding.rs](bidding.rs)                     | Quote replacement, collateral changes, rejected deposits, deadlines, and bid counts.                     |
| Settlement              | [lifecycle.rs](lifecycle.rs)                 | Winner selection, ties, claim order, refunds, and payout destinations.                                   |
| Expiry and cancellation | [state_transitions.rs](state_transitions.rs) | Both operations from every RFQ state, deadline boundaries, and recovery of remaining funds.              |
| Adversarial scenarios   | [adversarial.rs](adversarial.rs)             | Unauthorized settlement, insufficient balances, repeated claims, rollback, spam, and stalled settlement. |
| Privacy                 | [privacy.rs](privacy.rs)                     | Decryption recipients and whether private inputs change account sizes or CPI structure.                  |
| Account closure         | [store_cleanup.rs](store_cleanup.rs)         | Closure certificates, rent recipients, invalid accounts, and retention of encrypted stores.              |
| Compute budget          | [compute_budget.rs](compute_budget.rs)       | Creation, funding, and bid changes within the transaction compute limit.                                 |
| Transaction size        | [transaction_size.rs](transaction_size.rs)   | Creation and funding fit separate versioned transactions with an address lookup table.                   |

## Detailed coverage

### Market membership

[market.rs](market.rs)

- `market_retains_disabled_identities_and_reactivates_their_delegation`: Adds makers and checks account size, admin-funded rent, and decryption grants. Removal must revoke the correct delegation while retaining the maker's identity and allocated space. Reactivation must restore access without charging rent again; selecting another maker's delegation for removal must fail.

### Creation and funding

[request_quote.rs](request_quote.rs)

- `creation_prepares_empty_escrows_and_leaves_valid_terms_unfunded`: Creation works with fresh or previously prepared escrows. It creates the primary store and leaves balances unchanged and the RFQ `Unfunded`.
- `funding_activates_only_the_private_side_and_preserves_the_bid_rent_reserve`: Funding escrows basis tokens for a buyer or asset tokens for a seller, enters `Valid`, and preserves the funder's bid-rent reserve.
- `funding_a_valid_or_invalid_quote_preserves_state_and_refunds_new_deposits`: Repeated funding cannot change either state or add collateral; new deposits are returned.
- `incorrect_funding_invalidates_an_unfunded_quote_and_refunds_both_sides`: Incorrect collateral amounts make an `Unfunded` RFQ `Invalid`, refund both deposits, and permit closure.
- `invalid_requests_refund_every_transferred_token`: Zero size, zero limit, excessive timeout, or insufficient balance on the required funding side must leave no escrowed tokens.
- `private_past_deadline_does_not_reject_account_creation`: A past private deadline does not publicly reject creation or funding.
- `a_rejected_second_funding_input_rolls_back_the_first_deposit`: An invalid second attestation must revert the first transfer and preserve `Unfunded` state and all balances.
- `compact_inputs_reject_signatures_bound_to_another_user_or_program`: Reconstructed input identities must still match the signature. A different signed user or program rejects creation and rolls back account creation.
- `prepared_escrow_requires_token_program_ownership_and_the_rfq_authority`: Existing escrow accounts with the wrong program owner or token authority must be rejected.

### Bidding

[bidding.rs](bidding.rs)

- `replacements_and_cancellation_adjust_only_the_required_collateral`: Increases deposit the difference, decreases refund it, and cancellation returns the remaining collateral. Updating a sell price keeps its asset collateral fixed. Bid counts, sequence numbers, store counts, and maker rent are checked.
- `failed_funding_preserves_old_quotes_and_other_side_can_succeed`: An underfunded replacement retains that side's old quote while allowing the independently funded side to update.
- `malformed_deposits_are_refunded_without_replacing_old_collateral`: Incorrect deposit amounts must be returned while preserving prior accepted quotes and collateral.
- `deadline_is_private_and_late_deposits_are_fully_refunded`: At the deadline, new deposits must be refunded and prior quotes retained without publicly rejecting the transaction.
- `bids_before_funding_refund_both_deposits_and_do_not_gain_priority`: An `Unfunded` RFQ accepts no collateral, active bid, or sequence priority. The same maker can bid normally after funding.
- `switching_quote_sides_counts_each_active_maker_once`: Moving between buy-only and sell-only quotes must preserve one active bid per maker; cancellation decrements the count once.

### Settlement and refunds

[lifecycle.rs](lifecycle.rs)

- `unfunded_quotes_cannot_expire_or_pay_claims_and_can_be_canceled_after_deadline`: Expiry and claims do not activate an unfunded request. Cancellation remains available after the deadline and pays nothing.
- `canceled_unfunded_quotes_cannot_be_reactivated_by_funding`: Funding after cancellation returns deposits and preserves the terminal state.
- `empty_auction_expiry_and_user_claim_are_private_and_repeatable`: Early expiry and claims do nothing; an empty expired auction refunds the user once without spending the bid-rent reserve.
- `buyer_ties_follow_sequence_and_makers_can_claim_before_user`: Equal sell quotes select the earlier sequence even when scanned in reverse order. Repeated scans do not advance the count, and maker claims may precede the user claim.
- `seller_can_claim_first_and_winner_recovers_unused_asset_collateral`: The highest eligible buy quote wins. The seller may claim first; the winner receives the asset and recovers collateral for its unused quote side.
- `cancellation_refunds_user_then_sell_only_maker_and_cannot_run_at_expiry`: Cancellation refunds both parties through separate claims, cannot pay twice, and stops being effective at the deadline.
- `quotes_at_the_limit_do_not_win_and_all_collateral_is_refunded`: A quote equal to the user's limit is ineligible on either side. With no eligible winner, all deposits are returned.
- `permissionless_callers_cannot_redirect_payouts_or_cancel_another_users_quote`: Incorrect beneficiaries and unauthorized cancellation must fail without changing state or collateral.

### Expiry and cancellation by state

[state_transitions.rs](state_transitions.rs)

These cases run for both buyer and seller RFQs. States are reached through instructions. After the tested transition, the remaining legitimate claims are completed and repeated to check that all escrow is released exactly once.

An encrypted no-op is a successful transaction whose semantic state and token balances stay unchanged; ciphertext handles and permission history may still change.

| Starting state or boundary | Expiry test and expected result                                                                              | Cancellation test and expected result                                                    |
|----------------------------|--------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------|
| `Invalid`                  | `expiry_invalid`: Remains invalid with refunded balances.                                                    | `cancel_invalid`: No additional refund.                                                  |
| `Unfunded`                 | `expiry_unfunded`: No unbacked claim is created.                                                             | `cancel_unfunded`: Enters `Canceled` without a payout before, at, or after the deadline. |
| `Valid`, before deadline   | `expiry_early`: Preserves funded quotes and collateral.                                                      | `cancel_before_deadline`: Refunds the user and preserves the maker's separate claim.     |
| `Valid`, at deadline       | `expiry_at_deadline`: Enters `Expired` with active bids awaiting scanning.                                   | `cancel_at_deadline`: No-op; expiry and settlement remain available.                     |
| `Valid`, after deadline    | `expiry_after_deadline`: Enters `Expired` and retains collateral for settlement.                             | `cancel_after_deadline`: No-op; the user cannot withdraw collateral ahead of settlement. |
| `Valid`, no bids           | `expiry_without_bids`: At or after the deadline, enters `Claimable` for a full user refund without scanning. | Covered by the funded cancellation path; no separate no-bid cancellation case here.      |
| `Canceled`                 | `expiry_canceled`: Preserves the user refund and outstanding maker claim.                                    | `cancel_canceled`: Cannot refund the user twice.                                         |
| `Expired`                  | `expiry_expired`: Preserves bids awaiting scanning.                                                          | `cancel_expired`: Preserves collateral committed to settlement.                          |
| `Claimable`                | `expiry_claimable`: Preserves the selected winner and pending payouts.                                       | `cancel_claimable`: Cannot replace the trade with a user refund.                         |
| `Claimed`                  | `expiry_claimed`: Preserves the completed user payout and outstanding maker claim.                           | `cancel_claimed`: Cannot spend remaining maker collateral on another user refund.        |

### Adversarial scenarios

[adversarial.rs](adversarial.rs)

- `non_winner_settlement`: A losing maker cannot claim the winner's ID or redirect either payout. Relaying a valid claim succeeds, but pays the recorded winner; the loser recovers only its own collateral.
- `double_settlement`: Replays identical claims before and after other parties claim, in both claim orders and on both RFQ sides. Balances and bid counts must change only once.
- `underfunded_taker`: Insufficient encrypted balance on the required leg makes funding invalid, leaves no escrow, and prevents later bids from being accepted.
- `underfunded_maker`: A maker can fund the opposite quote side while lacking balance for the eligible side. The unbacked quote must not win, and expiry and claims must return all deposits.
- `bid_rollback`: A second deposit attestation bound to the wrong program rejects the whole bid transaction, including the first transfer, maker-store creation, rent spending, and counter changes.
- `quote_spam`: Repeated tiny bids, cancellations, and zero quotes reuse one maker store and do not inflate active-bid or scan counts. The remaining legitimate bid can settle.
- `resume_settlement`: A maker triggers expiry, then an unrelated funded caller resumes scanning and claims after delays. No taker or winner signature is needed to release the remaining escrow.
- `program_substitution`: Substituting another executable program for the token program rejects claims before any payout or state change.

### Privacy checks

[privacy.rs](privacy.rs)

- `decrypt_permissions`: Inspects host CPI effects throughout both RFQ sides. Side and limit are user-only; size and expiry allow the user and maker group; counters are public; winner fields receive no decryption grants. Maker quotes remain maker-only, token balances owner-only, and temporary transfer amounts non-public.
- `private_branch_shape`: Varies private side, amounts, and whether the deadline has passed. The tested operations must retain the same account owners and sizes, CPI sequence, account counts, discriminators, and payload lengths.

These checks cover emitted permissions and observable structure in the offline runtime. They do not test cryptographic security, live coprocessor/KMS execution, or network timing.

### Closure and rent

[store_cleanup.rs](store_cleanup.rs)

- `certified_close_refunds_only_rfq_and_funder_and_retains_encrypted_stores`: A valid certificate permits closure and returns RFQ rent and remaining funder lamports to the user. Encrypted stores stay intact; closing the RFQ again fails.
- `close_rejects_wrong_beneficiary_funder_foreign_store_and_trailing_accounts`: Substituted refund, funder, or store accounts and unexpected extra accounts must fail.
- `close_requires_current_true_certificate_and_valid_public_inclusion_proof`: False certificates, invalid proofs, stale handles, and missing current slots cannot authorize closure.
- `close_handles_an_empty_funder_without_touching_the_primary_store`: An empty funder does not block closure; RFQ rent is returned and the primary store is preserved.

Additional rent assertions appear in [market.rs](market.rs), [request_quote.rs](request_quote.rs), [bidding.rs](bidding.rs), [lifecycle.rs](lifecycle.rs), and `quote_spam` in [adversarial.rs](adversarial.rs).

### Runtime limits

- [compute_budget.rs](compute_budget.rs), `funding_and_bids_fit_the_transaction_budget_across_address_sets`: Runs creation, funding, first bid, increase, decrease, and cancellation across 12 address sets at a 1,400,000 CU transaction limit. Checks resulting state and collateral, and prints maximum usage for each operation.
- [transaction_size.rs](transaction_size.rs), `creation_and_funding_with_lookup_table_fit_separate_solana_packets`: Serializes each transaction with its FHE envelope, compute-budget instruction, lookup table, and signature space. Each must fit the 1,232-byte packet limit.

### Shared fixtures and unit tests

[support/mod.rs](support/mod.rs) supplies funded participants, token accounts, signed mock inputs, instruction builders, clock control, and the cleartext ledger used by the runtime scenarios. It is test support, not a separate test suite.

The package also contains unit tests beside the implementation:

| Source                                                        | Checks                                                                                                                             |
|---------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------|
| [state/market.rs](../src/state/market.rs)                     | Retained maker identities, sorted membership, rejected mutations, account versions, and space calculations.                        |
| [instructions/place_bid.rs](../src/instructions/place_bid.rs) | Collateral circuits fit the host execution limits; a bid with three attestations fits a versioned transaction with a lookup table. |
| [util/close_rfq_cpi.rs](../src/util/close_rfq_cpi.rs)         | Returned closure certificates come from the host and match the current true handle.                                                |
| [allocator.rs](../src/allocator.rs)                           | Reused allocations preserve live memory; freeing adjacent blocks recovers capacity after exhaustion.                               |
| [state/mod.rs](../src/state/mod.rs)                           | Field-key padding.                                                                                                                 |

## Running tests

Run from the repository root. This builds the RFQ, host, and confidential-token SBF programs, then runs the package tests:

```sh
bash scripts/test-confidential-rfq.sh
```

After those binaries are built, run a single file or exact case:

```sh
cargo test --locked -p confidential_rfq --test adversarial
cargo test --locked -p confidential_rfq --test adversarial non_winner_settlement -- --exact
```

To see compute measurements:

```sh
cargo test --locked -p confidential_rfq --test compute_budget -- --nocapture
```
