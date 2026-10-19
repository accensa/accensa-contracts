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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UptoAuthorization;
    use soroban_sdk::testutils::{Address as _, Ledger as _};

    // Mirror of `compute_domain_separator` with an explicit version, so the
    // test can prove the version bytes are part of the preimage without
    // changing the production constant.
    fn separator_for_version(env: &Env, version: u32) -> BytesN<32> {
        let mut buf = Bytes::new(env);
        buf.extend_from_slice(&env.ledger().network_id().to_array());
        buf.append(&env.current_contract_address().to_xdr(env));
        buf.extend_from_slice(&version.to_be_bytes());
        env.crypto().sha256(&buf).into()
    }

    fn two_contract_addresses(env: &Env) -> (Address, Address) {
        (
            env.register(UptoAuthorization, ()),
            env.register(UptoAuthorization, ()),
        )
    }

    #[test]
    fn separator_binds_the_network_id() {
        let env = Env::default();
        let (contract, _) = two_contract_addresses(&env);

        env.ledger().set_network_id([1u8; 32]);
        let testnet = env.as_contract(&contract, || compute_domain_separator(&env));

        env.ledger().set_network_id([2u8; 32]);
        let mainnet = env.as_contract(&contract, || compute_domain_separator(&env));

        assert_ne!(testnet, mainnet, "the separator must bind the network id");
    }

    #[test]
    fn separator_binds_the_contract_address() {
        let env = Env::default();
        let (first, second) = two_contract_addresses(&env);

        let first_sep = env.as_contract(&first, || compute_domain_separator(&env));
        let second_sep = env.as_contract(&second, || compute_domain_separator(&env));

        assert_ne!(
            first_sep, second_sep,
            "a clone at another address is a different domain"
        );
    }

    #[test]
    fn separator_binds_the_protocol_version() {
        let env = Env::default();
        let (contract, _) = two_contract_addresses(&env);

        let actual = env.as_contract(&contract, || compute_domain_separator(&env));
        let pinned = env.as_contract(&contract, || separator_for_version(&env, PROTOCOL_VERSION));
        let next = env.as_contract(&contract, || {
            separator_for_version(&env, PROTOCOL_VERSION + 1)
        });

        assert_eq!(actual, pinned, "the separator must hash the version bytes");
        assert_ne!(actual, next, "a version bump must change the separator");
    }

    #[test]
    fn digest_is_reproducible_and_domain_scoped() {
        let env = Env::default();
        let (first, second) = two_contract_addresses(&env);
        let payment_id = BytesN::from_array(&env, &[7u8; 32]);
        let from = Address::generate(&env);
        let to = Address::generate(&env);

        let digest = |contract: &Address| {
            env.as_contract(contract, || {
                let separator = compute_domain_separator(&env);
                authorization_digest(&env, &separator, &payment_id, &from, &to, 1_000, 42)
            })
        };

        let once = digest(&first);
        let twice = digest(&first);
        assert_eq!(once, twice, "identical inputs and context must reproduce");

        let other_address = digest(&second);
        assert_ne!(
            once, other_address,
            "the contract address must scope the digest"
        );

        env.ledger().set_network_id([9u8; 32]);
        let other_network = digest(&first);
        assert_ne!(once, other_network, "the network id must scope the digest");
    }
}
