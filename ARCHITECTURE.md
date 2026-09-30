# Concepts

| Concept | Description                                                                                                    |
|---------|----------------------------------------------------------------------------------------------------------------|
| Market  | A set of valid makers, a user makes a RFQ against a specific market.                                           |
| Admin   | Can approvce makers on the market they are admin of and add tokens. Creates the market                         |
| Maker   | Approved by admins. Can see sizes of requests and place bids on requests in their market. Indexed by `u64` id. |
| User    | Can plce RFQs against any market, does not require approval.                                                   |

# Accounts

## Market

- `MakerId` = `NonZeroU32`

Optional versions of these ids are `u32`s, with `0` representing `None`.

Markets start with an empty maker vector sorted by ID. `Market` methods provide lookup, insertion, removal, and membership counts. Adding or removing a maker reallocates the account to fit the current membership; the admin pays additional rent on growth and receives the unused rent on shrinkage.

| Field            | Type                           | Description                                                                                                                        |
|------------------|--------------------------------|------------------------------------------------------------------------------------------------------------------------------------|
| Admin            | `Pubkey`                       | The admin for this market.                                                                                                         |
| Maker Group Bump | `u8`                           | The bump seed for the maker group's pda                                                                                            |
| Makers           | `Vec<(MakerId, Pubkey, bool)>` | Approved makers sorted by nonzero ID, with each entry containing a `u32` ID and a `Pubkey` with `bool` for whether they're active. |

## RFQ

| Field       | Type       | Description                                                                                  |
|-------------|------------|----------------------------------------------------------------------------------------------|
| Nonce       | `[u8; 32]` | Random nonce to prevent account replacement attacks. Derived from input handles on creation. |
| Bump        | `u8`       | The bump for the RFQ.                                                                        |
| Funder Bump | `u8`       | The bump for the account that holds funds for rent.                                          |
| Market      | `Pubkey`   | The market the RFQ is against.                                                               |
| User        | `Pubkey`   | The user that placed the RFQ.                                                                |
| Asset Token | `Pubkey`   | The token that is being bought/sold.                                                         |
| Basis Token | `Pubkey`   | The token used as currency.                                                                  |

### Private Fields

#### Primary Store

| Field            | ID                 | Type                 | Description                                                                                           | Revealed To     |
|------------------|--------------------|----------------------|-------------------------------------------------------------------------------------------------------|-----------------|
| State            | `state`            | `u8`                 | The state of the RFQ.                                                                                 | Public          |
| Timeout          | `expire_timestamp` | `UnixTimestamp`      | When the RFQ expires.                                                                                 | User and Makers |
| Bid Count        | `bid_count`        | `u32`                | The amount of valid bids.                                                                             | Public          |
| Bid Seq          | `bid_seq`          | `u32`                | Monotomically increasing sequence id, used to break ties.                                             | Public          |
| Searched Bids    | `searched_bids`    | `u32`                | The number of bids that have been searched to find the best bid.                                      | Public          |
| User Is Buyer    | `user_buyer`       | `bool`               | True if the user escrows the basis token, false if the user escrows the asset token.                  | User            |
| Offer Limit      | `offer_limit`      | `u64`                | Any offer below/above this is rejected, based on whether the user is the buyer or seller.             | User            |
| Size             | `size`             | `u64`                | The amount of asset sub-tokens the user wishes to buy/sell.                                           | User and Makers |
| Best Offer       | `best_offer`       | `u64`                | The best conter sub-token amount a maker has made.                                                    | None            |
| Best Maker       | `best_maker`       | `Option<MakerId>`    | The maker that offered the best offer. If `0` (`Option::None`), no maker has made a good enough offer | None            |
| Best Maker Index | `best_maker_index` | `Option<NonZeroU32>` | The index of the best maker, used to break ties.                                                      | None            |

#### Maker Store

Must be separate since there's a max of 32 fields.

| Field       | ID           | Type                 | Description                                                                                                                                                      | Revealed To |
|-------------|--------------|----------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------|
| Maker Buy   | `maker_buy`  | `u64`                | The amount of basis sub-tokens the maker wants to buy the asset tokens for. Succeeds if "user_buyer" is false and is greater than "offer_limit" and "best_offer" | Maker(id)   |
| Maker Sell  | `maker_sell` | `u64`                | The amount of basis sub-tokens the maker wants to sell asset tokens for. Succeeds if "user_buyer" is true and is less than "offer_limit" and "best_offer"        | Maker(id)   |
| Maker Index | `maker_seq`  | `Option<NonZeroU32>` | The `bid_seq + 1` of when the maker placed their bid. Should be set to `bid_seq + 1`. If `0`, then this means this bid has been searched already.                | Public      |

### Private Tokens

The RFQ owns both asset and basis tokens. The makers deposit both, sufficient to cover both sides of the trade. The user only deposits the token they need to, but the creation process still needs a token account that it withdraws 0 tokens from to maintain privacy.

### Privacy

- User: The user is public because solana would track who opened the account anyway. This could be hidden with a complex batching system that tracks users by ID and publishes RFQs in batches, but I'm calling that out of scope.
- Tokens Involved: It would be possible to encrypt the tokens involved in the trade, but the confidential token program does not support encrypting what tokens are stored in each account.
- Maker bid count: We could make this private, but it would be trivially easy to recreate based on transaction history.
- Timeout: We need to keep this public for bookkeeping reasons, it could be made private if doing rent properly

# PDAs

- Maker Decryption Group: `"market_maker_group"` + `KeyFor<Market>`
- RFQ: `"rfq"` + `KeyFor<Market>` + `KeyFor<User>` + `nonce`
- RFQ Funder: `"rfq_funder"` + `KeyFor<RFQ>`
- Maker Store `"rfq_maker_store"` + `KeyFor<RFQ>` + `maker_id`

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
- Initializes the maker decryption group at the `Market`'s PDA (not much to do).

### Add Maker

- Adds a maker's public key to the `Market` with a given id.
- If the id is already in use, error.
    - If reusing an id with the same key and that key was deactivated, reactivate it instead of erroring.
- Adds the maker's public key to the `Market`'s maker decryption group.

### Remove Maker

- Disables a maker's public key from the `Market` by marking it disabled. This stops them from placing new bids.
- Remove the maker's public ket from the `Market`'s maker decryption group.

### Request Quote

- Callable by anyone, the caller becomes the User.
- User opens a new PDA with seeds: `Market` key, User key, and the input handle from one of the inputs to nonce the account and prevent account replacement attacks.
- Transfer extra rent to the `RFQ` sufficient to support `x` bids, where `x` is supplied by the user.
    - This is not immediately transferred to the private store account to help with bookkeeping.
    - A user can later add support for more bids by transferring to the `RFQ`.
- Initializes all private fields with default values.
    - We can't initialize later as that would require more rent.
- Transfer either `offer_limit` or `size` tokens based on the side of the `RFQ` to the `RFQ`, the other transferring `0`.
- Check the expiry timestamp is valid privately, must be less than the current time + `MAXIMUM_TIMEOUT`.
- If invalid (`size == 0` OR `offer_limit == 0` OR the user didn't have enough tokens OR the timestamp is too far out):
    - Set the public `state` to `Invalid`.
    - Return all transferred tokens to the user.

### Cancel Quote

- Callable only by the User.
- `RFQ` must be in the `Valid` state.
- If `expire_timestamp` has not passed: The User marks the state as `Cancelled` and recovers all tokens.

### Place Bid

- Callable by a Maker on the `Market`.
- The `RFQ` must be in the `Valid` state AND the Maker must be active (not removed).
- Transfer the difference of both offered amount (buy and sell) from the previous offer (defaults to `0` as the previous offer) to the `RFQ`.
- Store the size of both offer sides and the index of the maker (for ordering who wins ties).
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
    - If `best_maker` is the maker's id then transfers the opposite of their trade (the opposite side's deposit gets transferred back).
    - Otherwise, transfers their original deposit.
- Sets both bid sides to `0`.
- Decrements `bid_count`.

### Close RFQ

- Callable by anyone
- State must be `Claimed`, `Canceled`, or `Invalid` and `bid_count` must be `0`.
- Closes the RFQ, returning all rent to the User.
- There should be no tokens left owned by the RFQ and no bids left.

# Other issues

## Rent

Usually I'd track rent and send it back to the appropriate account. In this case, the confidential value api doesn't fully support this so I'll have the user pre-allocate bid space. If they don't allocate enough then they're just hurting themselves by limiting the amount of bids they can have.
