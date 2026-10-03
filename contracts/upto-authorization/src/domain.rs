//! EIP-712-style domain separation for authorization signatures
//! (issue #416).
//!
//! A bare digest over `(payment_id, from, to, cap, expiry)` is valid
//! everywhere that tuple can be constructed: the same facilitator key, the
//! same buyer, replayed from a copy of this contract on another network, or
//! against another deployment address. [`compute_domain_separator`] binds
//! every signature to
//!
//! - the **network id** (host hash of the network passphrase) — Testnet
//!   signatures never validate on Mainnet and vice versa,
//! - the **deployed contract address** — a clone at a different address is
//!   a different domain, and
//! - the **protocol version** — a signature never survives a change of the
//!   digest construction itself.
//!
//! [`authorization_digest`] prepends the 32-byte domain separator to the
//! authorization preimage before hashing, so every signed digest is
//! domain-scoped. Off-chain signers should fetch
//! `UptoAuthorization::get_authorization_digest` (or recompute it from
//! these two functions) rather than building the preimage themselves.

use soroban_sdk::{xdr::ToXdr, Address, Bytes, BytesN, Env};

/// Version of the authorization signing scheme. Bump whenever the digest
/// preimage changes shape; old signatures then hash differently and die
/// with the domain separator rather than replaying across versions.
pub const PROTOCOL_VERSION: u32 = 1;

/// `sha256(network_id ‖ contract_address_xdr ‖ protocol_version_be)`.
///
/// Pure with respect to authorization state: only the host/network context
/// and this contract's own address enter the hash.
pub fn compute_domain_separator(env: &Env) -> BytesN<32> {
    let mut buf = Bytes::new(env);
    // Network id: hash of the network passphrase (e.g. Testnet vs Mainnet).
    buf.extend_from_slice(&env.ledger().network_id().to_array());
    // Deployed address of *this* contract, as XDR (same encoding an
    // off-chain client sees in the transaction footprint).
    buf.append(&env.current_contract_address().to_xdr(env));
    // Scheme version, big-endian so the encoding is canonical.
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    env.crypto().sha256(&buf).into()
}

/// `sha256(domain_separator ‖ payment_id ‖ from_xdr ‖ to_xdr ‖ cap_be ‖ expiry_be)`
/// — the exact 32 bytes an Ed25519 signer must sign for
/// `UptoAuthorization::authorize_signed`. The domain separator is
/// **prepended**, so the binding is the first thing hashed in.
#[allow(clippy::too_many_arguments)]
pub fn authorization_digest(
    env: &Env,
    domain_separator: &BytesN<32>,
    payment_id: &BytesN<32>,
    from: &Address,
    to: &Address,
    cap: i128,
    expiry: u32,
) -> BytesN<32> {
    let mut buf = Bytes::new(env);
    buf.extend_from_slice(&domain_separator.to_array());
    buf.extend_from_slice(&payment_id.to_array());
    buf.append(&from.clone().to_xdr(env));
    buf.append(&to.clone().to_xdr(env));
    buf.extend_from_slice(&cap.to_be_bytes());
    buf.extend_from_slice(&expiry.to_be_bytes());
    env.crypto().sha256(&buf).into()
}
