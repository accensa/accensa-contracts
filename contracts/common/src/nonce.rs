#![no_std]

use soroban_sdk::{contracttype, BytesN};

/// Cryptographic Nonce Revocation Bitmap
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonceBitmap {
    pub bitmap: BytesN<32>,
}

impl NonceBitmap {
    pub fn is_revoked(&self, nonce: u64) -> bool {
        let bit_index = nonce % 256;
        let byte_index = bit_index / 8;
        let bit_in_byte = bit_index % 8;

        let byte = self.bitmap.get(byte_index as u32).unwrap_or(0);
        (byte & (1 << bit_in_byte)) != 0
    }
}
