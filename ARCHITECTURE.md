# Concepts

| Concept | Description                                                                                                     |
|---------|-----------------------------------------------------------------------------------------------------------------|
| Market  | A set of valid makers, a user makes a RFQ against a specific market.                                            |
| Admin   | Can approve makers on the market they are admin of and add tokens. Creates the market.                          |
| Maker   | Approved by admins. Can see sizes of requests and place bids on requests in their market. Indexed by `MakerId`. |
| User    | Can place RFQs against any market, does not require approval.                                                   |

# Accounts

## Market

- `MakerId` = `NonZeroU32`

Optional versions of these ids are `u32`s, with `0` representing `None`.

| Field            | Type                           | Description                                                                                                                        |
|------------------|--------------------------------|------------------------------------------------------------------------------------------------------------------------------------|
| Admin            | `Pubkey`                       | The admin for this market.                                                                                                         |
| Maker Group Bump | `u8`                           | The bump seed for the maker group's PDA.                                                                                           |
| Makers           | `Vec<(MakerId, Pubkey, bool)>` | Approved makers sorted by nonzero ID, with each entry containing a `u32` ID and a `Pubkey` with `bool` for whether they're active. |

## RFQ

| Field       | Type       | Description                                                                                                                                   |
|-------------|------------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| Nonce       | `[u8; 32]` | Random nonce to prevent account replacement attacks. Derived from input handles on creation.                                                  |
| Bump        | `u8`       | The bump for the RFQ.                                                                                                                         |
| Funder Bump | `u8`       | The bump for the account that holds funds for rent.                                                                                           |
| Open Stores | `u32`      | Public `open_stores` count of the RFQ's open primary and maker stores. Incremented on creation and decremented only after successful closure. |
| Market      | `Pubkey`   | The market the RFQ is against.                                                                                                                |
| User        | `Pubkey`   | The user that placed the RFQ.                                                                                                                 |
| Asset Token | `Pubkey`   | The token that is being bought/sold.                                                                                                          |
| Basis Token | `Pubkey`   | The token used as currency.                                                                                                                   |

### Private Fields

#### Primary Store

| Field            | ID                 | Type                 | Description                                                                                                               | Revealed To     |
|------------------|--------------------|----------------------|---------------------------------------------------------------------------------------------------------------------------|-----------------|
| State            | `state`            | `u8`                 | The state of the RFQ.                                                                                                     | Public          |
| Timeout          | `expire_timestamp` | `UnixTimestamp`      | When the RFQ expires.                                                                                                     | User and Makers |
| Bid Count        | `bid_count`        | `u32`                | The amount of valid bids.                                                                                                 | Public          |
| Bid Seq          | `bid_seq`          | `u32`                | Monotonically increasing sequence ID, used to break ties.                                                                 | Public          |
| Searched Bids    | `searched_bids`    | `u32`                | The number of bids that have been searched to find the best bid.                                                          | Public          |
| User Is Buyer    | `user_buyer`       | `bool`               | True if the user escrows the basis token, false if the user escrows the asset token.                                      | User            |
| Offer Limit      | `offer_limit`      | `u64`                | Total basis sub-token limit for trading `size` asset sub-tokens: the buyer's maximum payment or seller's minimum receipt. | User            |
| Size             | `size`             | `u64`                | The amount of asset sub-tokens the user wishes to buy/sell.                                                               | User and Makers |
| Best Offer       | `best_offer`       | `u64`                | Total basis sub-token payment for the winning trade of `size` asset sub-tokens.                                           | None            |
| Best Maker       | `best_maker`       | `Option<MakerId>`    | The maker that offered the best offer. If `0` (`Option::None`), no maker has made a good enough offer                     | None            |
| Best Maker Index | `best_maker_index` | `Option<NonZeroU32>` | The index of the best maker, used to break ties.                                                                          | None            |

#### Maker Store

Must be separate since there's a max of 32 fields.

| Field          | ID           | Type                 | Description                                                                                                                                                                                                                                                         | Revealed To |
|----------------|--------------|----------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------|
| Maker Buy      | `maker_buy`  | `u64`                | Total basis sub-tokens offered to buy `size` asset sub-tokens; also the required basis collateral. Eligible when nonzero, `user_buyer` is false, and the offer exceeds `offer_limit`. The highest eligible offer wins.                                              | Maker(id)   |
| Maker Sell     | `maker_sell` | `u64`                | Total basis sub-tokens requested to sell `size` asset sub-tokens. Required asset collateral is `size` when this quote is nonzero, otherwise `0`. Eligible when nonzero, `user_buyer` is true, and the offer is below `offer_limit`. The lowest eligible offer wins. | Maker(id)   |
| Maker Sequence | `maker_seq`  | `Option<NonZeroU32>` | Set to `bid_seq + 1` when the maker submits or changes a bid. `0` marks a bid that has already been searched.                                                                                                                                                       | Public      |

### Private Tokens

The RFQ owns both asset and basis tokens. Each maker escrows `maker_buy` basis sub-tokens and, when `maker_sell` is nonzero, `size` asset sub-tokens. Both quotes are total basis-token amounts for the same asset quantity; the sell quote is the payment requested, not the amount of asset collateral. A zero quote requires zero collateral for that side. Maker collateral is independent of the user's private side.

The user escrows `offer_limit` basis sub-tokens when buying or `size` asset sub-tokens when selling. Creation still transfers `0` from the other token account to maintain privacy.

### Privacy

- User: The user is public because Solana would track who opened the account anyway. This could be hidden with a complex batching system that tracks users by ID and publishes RFQs in batches, but I'm calling that out of scope.
- Tokens Involved: It would be possible to encrypt the tokens involved in the trade, but the confidential token program does not support encrypting what tokens are stored in each account.
- Maker bid count: The active bid count is publicly decryptable. Transaction history reveals bid submissions, but does not by itself reveal whether their confidential transfers succeeded.
- Timeout: The timestamp is revealed only to the User and Makers. Expiry comparisons happen in FHE operations.

# PDAs

- Maker Decryption Group: `"market_maker_group"` + `KeyFor<Market>`
- RFQ: `"rfq"` + `KeyFor<Market>` + `KeyFor<User>` + `nonce`
- RFQ Funder: `"rfq_funder"` + `KeyFor<RFQ>`
    - System program owned, funds only.
- Maker Store `"rfq_maker_store"` + `KeyFor<RFQ>` + `maker_id`
    - Authority for the store.

# Operations

## RFQ States

- `Invalid`: The RFQ is invalid and cannot have bids placed.
- `Valid`: The RFQ is valid and can have bids placed.
- `Canceled`: The RFQ was canceled by the user, makers may claim their tokens.
- `Expired`: The RFQ has passed its timestamp, the best offer is being searched for.
- `Claimable`: The RFQ's best offer has been found (there may be no best offer), the user and makers may claim.
- `Claimed`: The user has claimed, makers may still claim.

## Operation Details

### Create Market

- Creates a new `Market` account with the creator as the admin.
- Initializes the maker decryption group at the Maker Decryption Group PDA (not much to do).

### Add Maker

- Adds a maker's public key to the `Market` with a given id.
- If the id is already in use, error.
    - If reusing an id with the same key and that key was deactivated, reactivate it instead of erroring.
- Adds the maker's public key to the `Market`'s maker decryption group.

### Remove Maker

- Disables a maker's public key from the `Market` by marking it disabled. This stops them from placing new bids.
- Removes the maker's public key from the `Market`'s maker decryption group.

### Request Quote

- Callable by anyone, the caller becomes the User.
- User opens a new PDA with seeds: `Market` key, User key, and the input handle from one of the inputs to nonce the account and prevent account replacement attacks.
- Transfer extra rent to the RFQ Funder sufficient to support `x` bids, where `x` is supplied by the user.
    - This is not immediately transferred to the private store account to help with bookkeeping.
    - A user can later add support for more bids by transferring to the RFQ Funder.
- Creates the primary store, initializes its fields, and sets `open_stores` to `1`.
    - Maker stores are created and initialized with zero/default values on their maker's first bid submission, using the prefunded rent.
- Transfer either `offer_limit` or `size` tokens based on the side of the `RFQ` to the `RFQ`, the other transferring `0`.
- Check the expiry timestamp is valid privately, must be less than the current time + `MAXIMUM_TIMEOUT`.
- If invalid (`size == 0` OR `offer_limit == 0` OR the user didn't have enough tokens OR the timestamp is too far out):
    - Set the public `state` to `Invalid`.
    - Return all transferred tokens to the user.

### Cancel Quote

- Callable only by the User.
- `RFQ` must be in the `Valid` state.
- If `expire_timestamp` has not passed: The User marks the state as `Canceled` and recovers all tokens.

### Place Bid

- Callable by a Maker on the `Market`.
- The `RFQ` must be in the `Valid` state AND the Maker must be active (not removed).
- Creates the maker's store if it does not exist, initializes its fields, and increments `open_stores` exactly once for that creation.
- Adjust collateral against the previous accepted quotes (both default to `0`):
    - Basis collateral is `maker_buy`.
    - Asset collateral is `size` when `maker_sell` is nonzero, otherwise `0`. Changing a nonzero sell quote to another nonzero value does not change the asset collateral.
    - Deposit any increase in collateral into the RFQ; refund any decrease to the maker. These amounts are selected in FHE operations.
    - If `expire_timestamp` has passed, select `0` for both deposits and refunds and retain the previous quotes.
- Store both quote amounts and the maker's sequence (for ordering who wins ties).
    - The seq id is updated if the bid is changed, even if an invalid change.
    - The payer for this is the `RFQ`'s funder (`funder_bump`).
    - This ensures the Maker does not pay rent for their bid.
- If invalid (not enough tokens for the claimed bid OR `expire_timestamp` has passed):
    - Set the claimed bid for that side size to the old value.
    - No need to refund as if there's not enough tokens it transfers `0`.
- A Maker may offer `0` for one or both sides, that makes that side considered invalid.
    - They can do it for both sides, which would cancel the bid.
        - As `bid_count` is public, this would be publicly known.
        - If they wanted to do this privately, they could offer `1` sub-token to either side, a weakness of not having rent tracking since this means they could offer bogus bids.
- If both sides were previously `0` and either side is now non-`0`:
    - Increment `bid_count`
- If either side was previously non-`0` and both sides are now `0`:
    - Decrement `bid_count`
- Increment `bid_seq`

### Expire RFQ

- Callable by a Maker or the User (those who have access to the `expire_timestamp`).
- The `RFQ` must be in the `Valid` state and the `expire_timestamp` must be before the current time.
- If `bid_count` is `0`:
    - Set the state to `Claimable`.
- Else:
    - Set the state to `Expired`.

### Calculate Winner

- Callable by anyone.
- State must be `Expired`.
- Called 1 time for each bid.
- Looks at the bid id passed in, increasing `searched_bids` if the bid has not already been searched.
- Sets the best offer value and id if this bid is the best offer and better than the limit for the user's side.
    - Ties are broken by lower bid index.
- If `searched_bids` is equal to `bid_count`, set the state to `Claimable`.

### User Claim

- Callable by anyone (usually the user)
- State must be `Claimable`
- Transfers the tokens to the user.
    - If the RFQ fails (`best_offer` is `0`) then the user gets their original deposit.
    - If the RFQ succeeds (`best_offer` is `>0`) then the user gets the offered tokens.
        - If this was a buy, also transfer the difference between `offer_limit` and `best_offer`.
- Set the state to `Claimed`

### Maker Claim

- Callable by anyone (usually the maker) passing a specific maker id.
- State must be `Claimed`, `Claimable`, `Canceled`, or `Invalid` and at least one side must be non-zero.
- Transfers the tokens to the maker.
    - If `best_maker` is the maker's ID, pays `best_offer` basis sub-tokens when the maker sells, or `size` asset sub-tokens when the maker buys, and refunds the unused collateral for the maker's other quote.
    - Otherwise, refunds the maker's basis and asset collateral.
- Sets both bid sides to `0`.
- Decrements `bid_count`.

### Close Stores (`close_stores`)

- Callable by anyone, supplying one or more of this RFQ's stores.
- Before closing any stores, verifies through the primary store that the state is `Claimed`, `Canceled`, or `Invalid` and `bid_count` is `0`.
- Validates each supplied store belongs to this RFQ, is still open, and appears only once in the call.
- Closes maker stores first, returning their rent to the User. Decrements `open_stores` once per successfully closed store; canceled bids still have stores that must be closed.
- Closes the primary store last, only when it is the sole remaining open store (`open_stores == 1`). Verifies the final state and bid count before closing it, then decrements `open_stores` to `0`.
- May be called repeatedly to close stores in batches. Must finish before `close_rfq`.
- Requires a host instruction that closes an encrypted store with its authority's signature and refunds rent to the User. The checked-in host only provides upgrade-authority preview cleanup; an authority-controlled close-store CPI is a required library extension for this operation.

### Close RFQ (`close_rfq`)

- Callable by anyone
- Requires `open_stores == 0`. `close_stores` has already verified the terminal state and zero bid count before closing the primary store.
- Returns unused RFQ Funder lamports to the User and closes the RFQ, returning its remaining rent to the User.
- There should be no tokens left owned by the RFQ and no bids left.

# Other issues

## Rent

Usually I'd track rent and send it back to the appropriate account. In this case, the confidential value api doesn't fully support this so I'll have the user pre-allocate bid space. If they don't allocate enough then they're just hurting themselves by limiting the amount of bids they can have.
