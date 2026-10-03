//! Ephemeral key delegation for mobile/hot wallet operations (issue #461).
//!
//! Cold wallets store their master key offline. To interact with high-frequency
//! state channels from mobile devices or hot automated signers without exposing
//! the master private key, a cold wallet signs a [`DelegationCertificate`] delegating
//! signing authority to an ephemeral public key until a specified ledger sequence
//! (block height).
//!
//! State channel state signatures produced by the ephemeral key are valid if and
//! only if the delegation certificate signature verifies against the channel's
//! master `sender_pubkey` and the current ledger sequence has not exceeded
//! `expires_at_ledger`.

use accensa_common::Error;
use soroban_sdk::{contracttype, Bytes, BytesN, Env};

/// Delegation certificate authorizing an ephemeral key to sign state updates
/// on behalf of a master cold wallet key.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationCertificate {
    /// Cold wallet master public key. Must match the channel's sender_pubkey.
    pub master_pubkey: BytesN<32>,
    /// Ephemeral public key authorized to sign state channel updates.
    pub ephemeral_pubkey: BytesN<32>,
    /// Ledger sequence (block height) at which this delegation expires (inclusive).
    pub expires_at_ledger: u32,
    /// Channel ID restricted to, or 0 to allow all channels for this master.
    pub channel_id: u64,
}

impl DelegationCertificate {
    /// Domain separator and canonical byte serialization for certificate signing.
    pub fn payload(&self, env: &Env) -> Bytes {
        let mut buf = Bytes::new(env);
        buf.extend_from_slice(b"accensa:delegation:v1");
        buf.extend_from_slice(&self.master_pubkey.to_array());
        buf.extend_from_slice(&self.ephemeral_pubkey.to_array());
        buf.extend_from_slice(&self.expires_at_ledger.to_be_bytes());
        buf.extend_from_slice(&self.channel_id.to_be_bytes());
        buf
    }

    /// Verifies the delegation certificate against the expected master key,
    /// channel id, and current ledger height.
    pub fn verify(
        &self,
        env: &Env,
        cert_signature: &BytesN<64>,
        expected_master: &BytesN<32>,
        expected_channel_id: u64,
    ) -> Result<(), Error> {
        // Master public key must match expected channel master
        if self.master_pubkey != *expected_master {
            return Err(Error::Unauthorized);
        }

        // Channel constraint: must match expected channel or be 0 (wildcard)
        if self.channel_id != 0 && self.channel_id != expected_channel_id {
            return Err(Error::Unauthorized);
        }

        // Expiration check: block height / ledger sequence must not exceed expiration
        if env.ledger().sequence() > self.expires_at_ledger {
            return Err(Error::WindowExpired);
        }

        // Verify master key's signature over canonical certificate payload
        let payload = self.payload(env);
        env.crypto()
            .ed25519_verify(&self.master_pubkey, &payload, cert_signature);

        Ok(())
    }

    /// Verifies both the delegation certificate and the delegated state signature.
    pub fn verify_delegated_state_signature(
        &self,
        env: &Env,
        cert_signature: &BytesN<64>,
        expected_master: &BytesN<32>,
        expected_channel_id: u64,
        state_payload: &Bytes,
        state_signature: &BytesN<64>,
    ) -> Result<(), Error> {
        self.verify(env, cert_signature, expected_master, expected_channel_id)?;
        env.crypto()
            .ed25519_verify(&self.ephemeral_pubkey, state_payload, state_signature);
        Ok(())
    }
}
