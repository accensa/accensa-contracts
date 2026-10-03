//! CREATE2-style deterministic vault deployments (issue #472).
//!
//! The counter-derived salt family used by [`crate::RefundVaultFactory::deploy_vault`]
//! guarantees *stability* for a merchant but not *predictability*: the address
//! of the next vault cannot be known until the counter advances. This module
//! adds a second, balanced deployment path keyed to the two participants of an
//! escrow:
//!
//! - the salt is `sha256(buyer ‖ merchant)` (`participant_salt`), a pure
//!   function of the escrow itself, not of any mutable counter;
//! - the vault address is therefore `deployer.with_current_contract(salt)
//!   .deployed_address()` (`predict_address`), reproducible off-chain *before*
//!   the factory ever deploys.
//!
//! A buyer and merchant can thus agree on an escrow address ahead of time
//! (e.g. to pre-fund it) and the factory's later `deploy_for_participants`
//! call lands on exactly that address. Reusing an already-deployed
//! participant pair reverts with the normal [`crate::Error::SaltCollision`]
//! guard.

use soroban_sdk::{xdr::ToXdr, Address, Bytes, BytesN, Env};

/// Salt for an escrow between `buyer` and `merchant`: the SHA-256 hash of the
/// concatenation of the two addresses' XDR encodings.
///
/// SHA-256 of participant addresses, per #472. Binding the hash to the XDR
/// bytes (rather than raw key bytes) keeps it stable across the address
/// formats Soroban supports (Ed25519, contracts, muxed).
pub(crate) fn participant_salt(env: &Env, buyer: &Address, merchant: &Address) -> BytesN<32> {
    let mut buf = buyer.clone().to_xdr(env);
    buf.append(&merchant.clone().to_xdr(env));
    env.crypto().sha256(&Bytes::from(&buf)).into()
}

/// The deterministic address a vault for `(buyer, merchant)` will land on,
/// without deploying anything: `with_current_contract(participant_salt)
/// .deployed_address()`.
///
/// Because a deployment address is a pure function of the deploying contract
/// (the factory) and the salt, and the salt here is itself a pure function of
/// the participants, `(factory, buyer, merchant)` uniquely determines the
/// vault address.
pub(crate) fn predict_address(env: &Env, buyer: &Address, merchant: &Address) -> Address {
    env.deployer()
        .with_current_contract(participant_salt(env, buyer, merchant))
        .deployed_address()
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    #[test]
    fn participant_salt_is_stable() {
        let env = Env::default();
        let buyer = Address::generate(&env);
        let merchant = Address::generate(&env);

        let a = participant_salt(&env, &buyer, &merchant);
        let b = participant_salt(&env, &buyer, &merchant);
        assert_eq!(
            a, b,
            "salt must be deterministic for identical participants"
        );
    }

    #[test]
    fn participant_salt_depends_on_both_parties_and_order() {
        let env = Env::default();
        let buyer = Address::generate(&env);
        let merchant = Address::generate(&env);

        let switched = participant_salt(&env, &merchant, &buyer);
        let original = participant_salt(&env, &buyer, &merchant);
        assert_ne!(
            switched, original,
            "participant order must bind to the salt"
        );

        let other = Address::generate(&env);
        let with_other = participant_salt(&env, &buyer, &other);
        assert_ne!(
            with_other, original,
            "changing a party must change the salt"
        );
    }
}
