use anchor_lang::prelude::*;

#[error_code]
pub enum ConfidentialRfqError {
    #[msg("Maker for given id already exists")]
    MakerAlreadyExists,
    #[msg("Maker for given id not found")]
    MakerNotFound,
    #[msg("Invalid RFQ input")]
    InvalidRfqInput,
    #[msg("Invalid RFQ account")]
    InvalidRfqAccounts,
    #[msg("Invalid FHE execution")]
    InvalidFheExecution,
    #[msg("Invalid confidential transfer result")]
    InvalidTransferResult,
    #[msg("Maker id must be nonzero")]
    InvalidMakerId,
    #[msg("Signer is not an approved maker")]
    UnauthorizedMaker,
    #[msg("RFQ belongs to a different market")]
    MarketMismatch,
    #[msg("RFQ encrypted store mismatch")]
    RfqStoreMismatch,
    #[msg("RFQ mint mismatch")]
    MintMismatch,
    #[msg("Confidential token account mismatch")]
    TokenAccountMismatch,
    #[msg("Maker has already placed a bid on this RFQ")]
    BidAlreadyExists,
    #[msg("RFQ bid capacity reached")]
    BidCapacityReached,
    #[msg("RFQ bid count overflow")]
    BidCountOverflow,
    #[msg("RFQ has not expired")]
    RfqNotExpired,
    #[msg("RFQ has expired")]
    RfqExpired,
    #[msg("Maker public key must be nonzero")]
    InvalidMakerKey,
    #[msg("Maker public key is already registered")]
    MakerKeyAlreadyExists,
    #[msg("RFQ account version is unsupported")]
    InvalidRfqVersion,
    #[msg("RFQ user account does not match the recorded user")]
    InvalidUser,
    #[msg("RFQ encrypted store does not match this RFQ")]
    InvalidEncryptedStore,
    #[msg("The current can_close value is missing")]
    CanCloseMissing,
    #[msg("Zama's public decrypt verifier returned invalid data")]
    InvalidVerifierReturn,
    #[msg("The certified can_close value is not true")]
    RfqNotClosable,
}
