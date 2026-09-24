use anchor_lang::prelude::*;

#[error_code]
pub enum ConfidentialRfqError {
    #[msg("Maker for given id already exists")]
    MakerAlreadyExists,
    #[msg("Maker for given id not found")]
    MakerNotFound,
}
