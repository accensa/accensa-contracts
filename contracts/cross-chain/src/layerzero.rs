//! LayerZero omnichain dispute bridging (issue #455).
//!
//! Allows decentralized arbitrators on other chains to submit dispute
//! resolutions to Soroban through a LayerZero endpoint, instead of trusting a
//! single-chain multisig. The Soroban side is deliberately small: it never
//! verifies LayerZero's DVN signatures itself (that is the endpoint's job on
//! every chain), it only trusts a registered **peer** contract on a known
//! remote chain and enforces the invariants that survive even a malicious
//! endpoint operator:
//!
//! - `lz_receive` is only callable by a registered **LayerZero endpoint**
//!   (admin-configured, like Wormhole's guardian set in [`crate::wormhole`]).
//! - The packet's `(src_chain_id, sender)` must name a **trusted peer**
//!   registered by the admin (`set_trusted_peer`). Unknown peers are rejected
//!   with [`Error::Unauthorized`] — mirroring LayerZero's own
//!   `Lz_UnknownPeer` semantics.
//! - Packet nonces are tracked per peer channel and only advance forward, so
//!   a replayed or reordered-old packet fails with [`Error::StaleState`]
//!   (the shared enum's replay code), exactly like the outbound
//!   `sequence` bookkeeping in [`crate::outbound`].
//! - The payload is fully bounds-checked before any decoding, so a truncated
//!   or oversized packet fails with [`Error::InvalidProof`] rather than
//!   trapping mid-interpretation.
//! - The settled dispute id can only be accepted **once**
//!   ([`Error::AlreadyRefunded`], the shared enum's duplicate-outcome code).
//! - The whole bridge inherits the contract-wide pause switch
//!   ([`Error::Paused`]).
//!
//! # Payload format (`DISPUTE_RESOLUTION_PAYLOAD_VERSION = 1`)
//!
//! ```text
//! [0]      version            (must be 1)
//! [1..33]  dispute_id         (32 bytes, caller-chosen unique id)
//! [33..65] resolution_hash    (32 bytes, hash of the arbitrators' outcome)
//! [65..73] approved_amount    (BE u64, bridge-denominated units)
//! [73..77] settled_at         (BE u32, source-chain timestamp; informational)
//! ```
//!
//! 77 bytes total; a payload longer than the version-1 length is rejected so
//! a future version cannot silently decode as v1.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, Bytes, BytesN, Env};

/// Current dispute-resolution payload layout version accepted by
/// [`parse_dispute_payload`].
pub const DISPUTE_RESOLUTION_PAYLOAD_VERSION: u8 = 1;

/// Exact byte length of a version-1 dispute-resolution payload.
pub const DISPUTE_PAYLOAD_LEN: usize = 77;

/// TTL low-water mark shared with the rest of the contract's storage policy.
pub(crate) const LZ_TTL_THRESHOLD: u32 = 100;

/// TTL extension target: ~30 days at ~5 s/ledger, matching the
/// contract-wide retention policy.
pub(crate) const LZ_TTL_EXTEND: u32 = 518_400;

/// A trusted remote (chain id, sender contract) pair allowed to deliver
/// dispute resolutions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerAddress {
    /// LayerZero chain id of the source chain (e.g. 101 = Ethereum).
    pub chain_id: u32,
    /// Emitter contract address on the source chain. LayerZero addresses are
    /// opaque up to 32 bytes (EVM: 20-byte left-padded, Solana: 32).
    pub sender: BytesN<32>,
}

/// A validated dispute resolution delivered from a trusted peer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolution {
    /// Caller-chosen unique dispute id; identical ids cannot be settled twice.
    pub dispute_id: BytesN<32>,
    /// Cryptographic commitment to the arbitrators' outcome.
    pub resolution_hash: BytesN<32>,
    /// Approved amount in bridge-denominated units.
    pub approved_amount: u64,
    /// Source-chain timestamp the resolution was finalized at (informational).
    pub settled_at: u32,
    /// LayerZero chain id the resolution arrived from.
    pub src_chain_id: u32,
    /// The trusted peer that delivered the packet.
    pub peer: BytesN<32>,
}

/// Emitted when a trusted peer's dispute resolution is accepted.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolvedEvent {
    #[topic]
    pub dispute_id: BytesN<32>,
    pub resolution_hash: BytesN<32>,
    pub approved_amount: u64,
    /// LayerZero chain id the resolution arrived from.
    pub src_chain_id: u32,
    /// The trusted peer that delivered the packet.
    pub peer: BytesN<32>,
    /// LayerZero packet nonce, persisted to block replays.
    pub nonce: u64,
    /// Ledger sequence at which the resolution was recorded.
    pub settled_ledger: u32,
}

/// Storage keys owned by the LayerZero half. A separate enum (with
/// [`PeerAddress`] beside it) keeps the existing `crate::DataKey` variant
/// list — and therefore every serialized enum value already in storage —
/// untouched. (The LayerZero endpoint itself is stored under the
/// contract-level `crate::DataKey::LzEndpoint` instance key.)
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Persistent: registered trusted peer for `(chain_id, sender)`.
    TrustedPeer(u32, BytesN<32>),
    /// Persistent: highest accepted packet nonce per peer channel.
    PeerNonce(u32, BytesN<32>),
    /// Persistent: dispute ids already settled (replay guard).
    DisputeSettled(BytesN<32>),
}

/// Encode a version-1 dispute-resolution payload (the wire format a peer
/// sends through its LayerZero endpoint).
pub fn encode_dispute_payload(
    env: &Env,
    dispute_id: &BytesN<32>,
    resolution_hash: &BytesN<32>,
    approved_amount: u64,
    settled_at: u32,
) -> Bytes {
    let mut buf = Bytes::new(env);
    buf.extend_from_slice(&[DISPUTE_RESOLUTION_PAYLOAD_VERSION]);
    buf.extend_from_slice(&dispute_id.to_array());
    buf.extend_from_slice(&resolution_hash.to_array());
    buf.extend_from_slice(&approved_amount.to_be_bytes());
    buf.extend_from_slice(&settled_at.to_be_bytes());
    buf
}

/// Parse and validate a version-1 dispute-resolution payload.
///
/// Returns [`Error::InvalidProof`] for a wrong version byte, a length other
/// than [`DISPUTE_PAYLOAD_LEN`], or any other structural violation — the same
/// "malformed cross-chain proof" code the Wormhole parser uses.
pub fn parse_dispute_payload(env: &Env, payload: &Bytes) -> Result<DisputeResolution, Error> {
    let len = payload.len() as usize;
    if len != DISPUTE_PAYLOAD_LEN {
        return Err(Error::InvalidProof);
    }

    let mut raw = [0u8; DISPUTE_PAYLOAD_LEN];
    payload.copy_into_slice(&mut raw);

    if raw[0] != DISPUTE_RESOLUTION_PAYLOAD_VERSION {
        return Err(Error::InvalidProof);
    }

    let mut dispute_bytes = [0u8; 32];
    dispute_bytes.copy_from_slice(&raw[1..33]);
    let dispute_id = BytesN::from_array(env, &dispute_bytes);

    let mut hash_bytes = [0u8; 32];
    hash_bytes.copy_from_slice(&raw[33..65]);
    let resolution_hash = BytesN::from_array(env, &hash_bytes);

    let amount_bytes = [
        raw[65], raw[66], raw[67], raw[68], raw[69], raw[70], raw[71], raw[72],
    ];
    let approved_amount = u64::from_be_bytes(amount_bytes);

    let settled_bytes = [raw[73], raw[74], raw[75], raw[76]];
    let settled_at = u32::from_be_bytes(settled_bytes);

    Ok(DisputeResolution {
        dispute_id,
        resolution_hash,
        approved_amount,
        settled_at,
        // The delivery path stamps the channel this packet arrived on.
        src_chain_id: 0,
        peer: BytesN::from_array(env, &[0u8; 32]),
    })
}

/// Record a delivered dispute resolution after every gate has passed.
///
/// The single write path behind [`crate::CrossChainBridge::lz_receive`]: it
/// owns the settled-dispute ledger, the per-channel nonce high-water mark,
/// and the [`DisputeResolvedEvent`] emission, so every accepted resolution
/// is recorded exactly the same way.
pub(crate) fn record_dispute_resolution(
    env: &Env,
    resolution: DisputeResolution,
    nonce: u64,
) -> Result<(), Error> {
    // A settled dispute is final: the id can never be accepted twice, no
    // matter which peer delivered it.
    let settled_key = DataKey::DisputeSettled(resolution.dispute_id.clone());
    if env.storage().persistent().has(&settled_key) {
        return Err(Error::AlreadyRefunded);
    }
    env.storage().persistent().set(&settled_key, &true);
    env.storage()
        .persistent()
        .extend_ttl(&settled_key, LZ_TTL_THRESHOLD, LZ_TTL_EXTEND);

    // Persist the packet-nonce high-water mark so replays of this or an
    // older packet are rejected on the next delivery.
    let nonce_key = DataKey::PeerNonce(resolution.src_chain_id, resolution.peer.clone());
    env.storage().persistent().set(&nonce_key, &nonce);
    env.storage()
        .persistent()
        .extend_ttl(&nonce_key, LZ_TTL_THRESHOLD, LZ_TTL_EXTEND);

    DisputeResolvedEvent {
        dispute_id: resolution.dispute_id,
        resolution_hash: resolution.resolution_hash,
        approved_amount: resolution.approved_amount,
        src_chain_id: resolution.src_chain_id,
        peer: resolution.peer,
        nonce,
        settled_ledger: env.ledger().sequence(),
    }
    .publish(env);

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn sample_resolution(env: &Env) -> DisputeResolution {
        DisputeResolution {
            dispute_id: BytesN::from_array(env, &[7u8; 32]),
            resolution_hash: BytesN::from_array(env, &[9u8; 32]),
            approved_amount: 1_000,
            settled_at: 1_700_000_000,
            src_chain_id: 0,
            peer: BytesN::from_array(env, &[0u8; 32]),
        }
    }

    #[test]
    fn encode_round_trips_through_parse() {
        let env = Env::default();
        let r = sample_resolution(&env);

        let payload = encode_dispute_payload(
            &env,
            &r.dispute_id,
            &r.resolution_hash,
            r.approved_amount,
            r.settled_at,
        );
        assert_eq!(payload.len(), DISPUTE_PAYLOAD_LEN as u32);

        let parsed = parse_dispute_payload(&env, &payload).unwrap();
        assert_eq!(parsed.dispute_id, r.dispute_id);
        assert_eq!(parsed.resolution_hash, r.resolution_hash);
        assert_eq!(parsed.approved_amount, r.approved_amount);
        assert_eq!(parsed.settled_at, r.settled_at);
    }

    #[test]
    fn wrong_length_is_rejected() {
        let env = Env::default();
        let r = sample_resolution(&env);
        let payload = encode_dispute_payload(
            &env,
            &r.dispute_id,
            &r.resolution_hash,
            r.approved_amount,
            r.settled_at,
        );
        let truncated: Bytes = payload.slice(0..(DISPUTE_PAYLOAD_LEN as u32 - 1));
        assert_eq!(
            parse_dispute_payload(&env, &truncated),
            Err(Error::InvalidProof)
        );

        // One byte too long is equally invalid.
        let mut oversized = std::vec![0u8; DISPUTE_PAYLOAD_LEN];
        payload.copy_into_slice(&mut oversized);
        oversized.push(0);
        let oversized = Bytes::from_slice(&env, &oversized);
        assert_eq!(
            parse_dispute_payload(&env, &oversized),
            Err(Error::InvalidProof)
        );
    }

    #[test]
    fn wrong_version_is_rejected() {
        let env = Env::default();
        let r = sample_resolution(&env);
        let payload = encode_dispute_payload(
            &env,
            &r.dispute_id,
            &r.resolution_hash,
            r.approved_amount,
            r.settled_at,
        );
        let mut raw = std::vec![0u8; DISPUTE_PAYLOAD_LEN];
        payload.copy_into_slice(&mut raw);
        raw[0] = DISPUTE_RESOLUTION_PAYLOAD_VERSION + 1;
        let future = Bytes::from_slice(&env, &raw);
        assert_eq!(
            parse_dispute_payload(&env, &future),
            Err(Error::InvalidProof)
        );
    }
}
