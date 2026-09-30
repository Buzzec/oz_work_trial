//! Host public-decrypt verification before RFQ account closure.

use anchor_lang::{prelude::*, solana_program::program::get_return_data};
use zama_host::instructions::{MmrInclusionProof, PublicDecryptReturnData};

use crate::ConfidentialRfqError;

pub fn verify_can_close<'info>(
    program: Pubkey,
    accounts: zama_host::cpi::accounts::VerifyPublicDecrypt<'info>,
    handle: [u8; 32],
    signatures: Vec<[u8; 65]>,
    extra_data: Vec<u8>,
    proof: MmrInclusionProof,
) -> Result<()> {
    zama_host::cpi::verify_public_decrypt(
        CpiContext::new(program, accounts),
        handle,
        bool_true_cleartext(),
        signatures,
        extra_data,
        proof,
    )?;
    let (verifier_program, data) =
        get_return_data().ok_or(error!(ConfidentialRfqError::InvalidVerifierReturn))?;
    let verified = PublicDecryptReturnData::try_from_slice(&data)
        .map_err(|_| error!(ConfidentialRfqError::InvalidVerifierReturn))?;
    verify_close_result(verifier_program, handle, &verified)
}

pub(crate) const fn bool_true_cleartext() -> [u8; 32] {
    let mut value = [0; 32];
    value[31] = 1;
    value
}

pub(crate) fn verify_close_result(
    verifier_program: Pubkey,
    expected_handle: [u8; 32],
    verified: &PublicDecryptReturnData,
) -> Result<()> {
    require_keys_eq!(
        verifier_program,
        zama_host::ID,
        ConfidentialRfqError::InvalidVerifierReturn
    );
    require!(
        verified.handle == expected_handle,
        ConfidentialRfqError::InvalidVerifierReturn
    );
    require!(
        verified.cleartext == bool_true_cleartext(),
        ConfidentialRfqError::RfqNotClosable
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returned_certificate_must_come_from_host_and_match_current_true_handle() {
        let handle = [9; 32];
        let mut returned = PublicDecryptReturnData {
            handle,
            cleartext: bool_true_cleartext(),
            context_id: [3; 32],
        };
        verify_close_result(zama_host::ID, handle, &returned).unwrap();
        assert!(verify_close_result(Pubkey::new_unique(), handle, &returned).is_err());
        assert!(verify_close_result(zama_host::ID, [4; 32], &returned).is_err());
        returned.cleartext = [0; 32];
        assert!(verify_close_result(zama_host::ID, handle, &returned).is_err());
        returned.cleartext[31] = 2;
        assert!(verify_close_result(zama_host::ID, handle, &returned).is_err());
    }
}
