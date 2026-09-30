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

| Field       | Type       | Description                                                                                  |
|-------------|------------|----------------------------------------------------------------------------------------------|
| Nonce       | `[u8; 32]` | Random nonce to prevent account replacement attacks. Derived from input handles on creation. |
| Bump        | `u8`       | The bump for the RFQ.                                                                        |
| Funder Bump | `u8`       | The bump for the account that holds funds for rent.                                          |
| Open Stores | `u32`      | The number of open primary and maker stores. Stores remain open for now.                     |
| Market      | `Pubkey`   | The market the RFQ is against.                                                               |
| User        | `Pubkey`   | The user that placed the RFQ.                                                                |
| Asset Token | `Pubkey`   | The token that is being bought/sold.                                                         |
| Basis Token | `Pubkey`   | The token used as currency.                                                                  |

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
- `Unfunded`: The RFQ's terms are valid, but collateral has not been deposited. Bids cannot be placed.
- `Valid`: The RFQ is fully funded and can have bids placed.
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
- Check the expiry timestamp is valid privately, must be less than the current time + `MAXIMUM_TIMEOUT`.
- If invalid (`size == 0` OR `offer_limit == 0` OR the timestamp is too far out):
    - Set the public `state` to `Invalid`.
- Otherwise, set the state to `Unfunded`. No tokens are transferred until funding, and bids cannot be placed before funding succeeds.
- A past expiry is accepted without publicly revealing the timestamp; any later bid will retain zero collateral.

### Fund Quote (`fund_quote`)

- Callable only by the User, in a separate transaction after `request_quote`.
- Transfer either `offer_limit` basis tokens or `size` asset tokens based on the side of the RFQ, the other transferring `0`.
- If the state is `Unfunded` and both transferred amounts match the required collateral, set the state to `Valid`.
- If funding is invalid, set an `Unfunded` RFQ to `Invalid` and return all transferred tokens to the User. For any other state, return the new deposits without changing the state.
- These checks happen in FHE operations. Transfers, refunds, and the state change are atomic within this transaction.

### Cancel Quote

- Callable only by the User.
- If the RFQ is `Unfunded`, mark it `Canceled` at any time. There are no user deposits to refund.
- If the RFQ is `Valid` and `expire_timestamp` has not passed, mark it `Canceled` and return the user's collateral.

### Place Bid

- Callable by a Maker on the `Market`.
- The `RFQ` must be in the `Valid` state AND the Maker must be active (not removed).
- Creates the maker's store if it does not exist, initializes its fields, and increments `open_stores` exactly once for that creation.
- Adjust collateral against the previous accepted quotes (both default to `0`):
    - Basis collateral is `maker_buy`.
    - Asset collateral is `size` when `maker_sell` is nonzero, otherwise `0`. Changing a nonzero sell quote to another nonzero value does not change the asset collateral.
    - Deposit any increase in collateral into the RFQ; refund any decrease to the maker. These amounts are selected in FHE operations.
    - If the RFQ is not `Valid` (including `Unfunded`) or `expire_timestamp` has passed, retain the previous quotes and collateral; refund any supplied deposits in full, so the net collateral change is `0`.
- Store both quote amounts and the maker's sequence (for ordering who wins ties).
    - The seq id is updated if the bid is changed, even if an invalid change.
    - The payer for this is the `RFQ`'s funder (`funder_bump`).
    - This ensures the Maker does not pay rent for their bid.
- If invalid (the actual deposit does not match the required increase OR `expire_timestamp` has passed):
    - Set the claimed bid for that side size to the old value.
    - Refund that side's actual transferred amount; an insufficient-balance transfer already returns `0`.
- A Maker may offer `0` for one or both sides, that makes that side considered invalid.
    - They can do it for both sides, which would cancel the bid.
        - As `bid_count` is public, this would be publicly known.
        - If they wanted to do this privately, they could offer `1` sub-token to either side, a weakness of not having rent tracking since this means they could offer bogus bids.
- If both sides were previously `0` and either side is now non-`0`:
    - Increment `bid_count`
- If either side was previously non-`0` and both sides are now `0`:
    - Decrement `bid_count`
- Increment `bid_seq` while the RFQ is `Valid`, including invalid changes.

### Expire RFQ

- Callable by a Maker or the User (those who have access to the `expire_timestamp`).
- The `RFQ` must be in the `Valid` state and the `expire_timestamp` must be at or before the current time.
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

- Deferred for now. Primary and maker stores remain open; see the store-closure limitation under Other issues.

### Close RFQ (`close_rfq`)

- Callable by anyone
- State must be `Claimed`, `Canceled`, or `Invalid` and `bid_count` must be `0`.
- Returns unused RFQ Funder lamports to the User and closes the RFQ, returning its remaining rent to the User.
- The primary and maker stores remain open.
- There should be no tokens left owned by the RFQ and no bids left.

# Other issues

## Persistent store closure

Zama has no instruction for the RFQ program to close encrypted stores. The stores remain open and their rent cannot be recovered.

## Rent

Usually I'd track rent and send it back to the appropriate account. In this case, the confidential value api doesn't fully support this so I'll have the user pre-allocate bid space. If they don't allocate enough then they're just hurting themselves by limiting the amount of bids they can have.
