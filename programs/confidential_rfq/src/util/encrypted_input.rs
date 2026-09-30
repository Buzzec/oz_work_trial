//! Compact instruction inputs omit identities already fixed by the signer and CPI target.
//! The complete signed attestation is reconstructed before the host verifies it.

use anchor_lang::prelude::*;
use zama_host::CoprocessorInputAttestation;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct EncryptedInput {
    pub input_handle: [u8; 32],
    pub ct_handles: Vec<[u8; 32]>,
    pub handle_index: u8,
    pub contract_chain_id: u64,
    pub extra_data: Vec<u8>,
    pub signatures: Vec<[u8; 65]>,
}

impl EncryptedInput {
    /// Binding a proof to a different signer or program invalidates its signature;
    /// the host still verifies the full attestation before accepting any input.
    pub fn into_attestation(self, user: Pubkey, program: Pubkey) -> CoprocessorInputAttestation {
        CoprocessorInputAttestation {
            input_handle: self.input_handle,
            ct_handles: self.ct_handles,
            handle_index: self.handle_index,
            user_address: user.to_bytes(),
            contract_address: program.to_bytes(),
            contract_chain_id: self.contract_chain_id,
            extra_data: self.extra_data,
            signatures: self.signatures,
        }
    }
}

impl From<CoprocessorInputAttestation> for EncryptedInput {
    fn from(value: CoprocessorInputAttestation) -> Self {
        Self {
            input_handle: value.input_handle,
            ct_handles: value.ct_handles,
            handle_index: value.handle_index,
            contract_chain_id: value.contract_chain_id,
            extra_data: value.extra_data,
            signatures: value.signatures,
        }
    }
}
