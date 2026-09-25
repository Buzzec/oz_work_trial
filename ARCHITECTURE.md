# Concepts

| Concept | Description                                                                                                    |
|---------|----------------------------------------------------------------------------------------------------------------|
| Market  | A set of valid makers, a user makes a RFQ against a specific market.                                           |
| Admin   | Can approvce makers on the market they are admin of and add tokens. Creates the market                         |
| Maker   | Approved by admins. Can see sizes of requests and place bids on requests in their market. Indexed by `u64` id. |
| User    | Can plce RFQs against any market, does not require approval.                                                   |

# Accounts

## Market

- `MakerId` = `NonZeroU64`

Optional versions of these ids are `u64`s, with `0` representing `None`.

Markets start with an empty maker vector sorted by ID. The field is private; `Market` methods provide lookup, insertion, removal, and membership counts. Adding or removing a maker reallocates the account to fit the current membership; the admin pays additional rent on growth and receives the unused rent on shrinkage.

| Field            | Type                   | Description                                                                                   |
|------------------|------------------------|-----------------------------------------------------------------------------------------------|
| Admin            | `Pubkey`               | The admin for this market.                                                                    |
| Maker Group Bump | `u8`                   | The bump seed for the maker group's pda                                                       |
| Makers           | `Vec<MakerEntry>`      | Approved makers sorted by nonzero ID, with each entry containing a `u64` ID and a `Pubkey`. |

## RFQ

| Field       | Type            | Description                                                      |
|-------------|-----------------|------------------------------------------------------------------|
| Nonce       | `[u8; 32]`      | Random nonce to prevent account replacement attacks.             |
| Bump        | `u8`            | The bump for the RFQ's authority PDA.                            |
| Market      | `Pubkey`        | The market the RFQ is against.                                   |
| User        | `Pubkey`        | The user that placed the RFQ.                                    |
| Timeout     | `UnixTimestamp` | When this RFQ expires. All maker bids must last until this time. |
| Bid Count   | `u64`           | The amount of bids placed on the RFQ.                            |
| Asset Token | `Pubkey`        | The token that is being bought/sold.                             |
| Basis Token | `Pubkey`        | The token used as currency.                                      |

### Private Fields

| Field         | ID                | Type              | Description                                                                                                                                                      | Revealed To     |
|---------------|-------------------|-------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------|-----------------|
| User Is Buyer | "user_buyer"      | `bool`            | True if the user escrows the basis token, false if the user escrows the asset token.                                                                             | User            |
| User Claimed  | "user_claimed     | `bool`            | True if the user has claimed after the RFQ expires.                                                                                                              | User            |            
| Offer Limit   | "offer_limit"     | `u64`             | Any offer below/above this is rejected, based on whether the user is the buyer or seller.                                                                        | User            |
| Size          | "size"            | `u64`             | The amount of asset sub-tokens the user wishes to buy/sell.                                                                                                      | User and Makers |
| Best Amount   | "best_offer"      | `u64`             | The best conter sub-token amount a maker has made.                                                                                                               | None            |
| Best Maker    | "best_maker       | `Option<MakerId>` | The maker that offered the best offer. If `0` (`Option::None`), no maker has made a good enough offer                                                            | None            |
| Maker Buy     | "maker_buy_{id}"  | `u64`             | The amount of basis sub-tokens the maker wants to buy the asset tokens for. Succeeds if "user_buyer" is false and is greater than "offer_limit" and "best_offer" | Maker(id)       |
| Maker Sell    | "maker_sell_{id}" | `u64`             | The amount of basis sub-tokens the maker wants to sell asset tokens for. Succeeds if "user_buyer" is true and is less than "offer_limit" and "best_offer"        | Maker(id)       |
| Closed Bids   | "closed_bids"     | `u64`             | The amount of bids that have been closed after this timed out.                                                                                                   | None            |
| Can Close     | "can_close"       | `bool`            | If the RFQ can be closed, only true if closed_bids is equal to `bid_count` and `user_claimed` is true                                                            | Public          |

### Private Tokens

The RFQ owns both asset and basis tokens. The makers deposit both, sufficient to cover both sides of the trade. The user only deposits the token they need to, but the creation process still needs a token account that it withdraws 0 tokens from to maintain privacy.

### Privacy

- User: The user is public because solana would track who opened the account anyway. This could be hidden with a complex batching system that tracks users by ID and publishes RFQs in batches, but I'm calling that out of scope.
- Tokens Involved: It would be possible to encrypt the tokens involved in the trade, but the confidential token program does not support encrypting what tokens are stored in each account.
- Maker bid count: We could make this private, but it would be trivially easy to recreate based on transaction history.
- Timeout: We need to keep this public for bookkeeping reasons, it could be made private if doing rent properly

# Operations

| Operation       | Who    | Description                                                                                                                                                                                                                                                        |
|-----------------|--------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Create Market   | Anyone | Creates a market with the creator as the Admin.                                                                                                                                                                                                                    |
| Add Maker       | Admin  | Adds a maker to a market. Also adds the maker to the market's decryption group.                                                                                                                                                                                    |
| Remove Maker    | Admin  | Removes a maker from a market. Also removes the maker from the market's decrpytion group. Does not cancel active bids.                                                                                                                                             |
| Request Quote   | Anyone | Creates an RFQ. Escrows the needed assets, `size` asset tokens if seller, `offer_limit` basis tokens if buyer.                                                                                                                                                     |
| Place Bid       | Maker  | Places a bid on a RFQ. Escrows the needed asset and basis tokens.                                                                                                                                                                                                  |
| Claim RFQ User  | User   | If the timestamp has passed, transfers the opposite tokens if successful, or escrowed tokens if not. Sets `user_claimed` to true if timestamp has passed. Sets `can_close` if `closed_bids` is equal to `bid_count` and `user_claimed` is true.                    | 
| Claim RFQ Maker | Maker  | If the timestamp has passed, transfers the opposite tokens (escrow + trade) if successful or all escrow tokens if not. Sets the maker's values to 0 if timestamp has passed. Sets `can_close` if `closed_bids` is equal to `bid_count` and `user_claimed` is true. |
| Close RFQ       | Anyone | Close an RFQ if the public `can_close` value is true. Rent goes to the user.                                                                                                                                                                                       |                                                                                                                        

# Other issues

## Rent

Usually I'd track rent and send it back to the appropriate account. In this case, the confidential value api doesn't fully support this so I'll have the user pre-allocate bid space. If they don't allocate enough then they're just hurting themselves by limiting the amount of bids they can have.
