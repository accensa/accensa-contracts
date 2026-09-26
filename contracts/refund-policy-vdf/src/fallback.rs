//! Dispute fallback oracle (issue #469).
//!
//! The [`crate::RefundPolicy`] evaluation is a stateless yes/no gate, but a
//! *dispute* — the buyer and merchant disagreeing about whether a refund is
//! owed — needs a decision-maker. When the primary decentralized arbitrators
//! fail to reach quorum (arbitrator timeout), this module opens a second path:
//! the merchant escalates the dispute to an external **fallback oracle** (an
//! optimistic-oracle-style service such as UMA), and the oracle's response
//! settles the dispute.
//!
//! # Lifecycle
//!
//! 1. `request_fallback_dispute` records an open [`FallbackDispute`] keyed by
//!    an incrementing id and returns the id.
//! 2. `build_fallback_oracle_request` serializes the dispute (payment ref,
//!    recipient, amount) into the XDR blob handed to the oracle as its
//!    request payload.
//! 3. `settle_fallback_dispute` closes the dispute with the oracle's refund
//!    decision (`refund == true` refund, `false` deny). Only the dispute's
//!    designated oracle address may settle it.
//!
//! Keeping the dispute ledger here is a deliberate, isolated deviation from
//! the crate's otherwise-stateless design: the escalation state must live
//! somewhere the dispute's participants can observe and the oracle can
//! settle, and co-locating it in the policy contract keeps the flow in one
//! place.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, xdr::ToXdr, Address, Bytes, BytesN, Env};

/// A dispute escalated to the fallback oracle.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FallbackDispute {
    /// Payment the dispute is about.
    pub payment_ref: BytesN<32>,
    /// The merchant seeking resolution (the requestor, authorized at
    /// request time).
    pub merchant: Address,
    /// The refund recipient named in the dispute.
    pub recipient: Address,
    /// The disputed refund amount.
    pub amount: i128,
    /// The fallback oracle authorized to settle this dispute (`require_auth`
    /// is checked on this address at settle time).
    pub oracle: Address,
    /// Ledger at which the dispute was escalated.
    pub request_ledger: u32,
    /// `true` while the dispute awaits settlement; `false` after the oracle
    /// rules.
    pub open: bool,
}

#[contracttype]
enum FallbackDataKey {
    /// Instance: number of disputes ever escalated; also the next id.
    Count,
    /// Persistent: one dispute per id.
    Dispute(u32),
}

/// Emitted when a dispute is escalated to the fallback oracle.
///
/// Topics: `("fallback_dispute_requested_event", dispute_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FallbackDisputeRequestedEvent {
    #[topic]
    pub dispute_id: u32,
    pub oracle: Address,
    pub payment_ref: BytesN<32>,
    pub merchant: Address,
    pub recipient: Address,
    pub amount: i128,
}

/// Emitted when the fallback oracle settles a dispute.
///
/// Topics: `("fallback_dispute_settled_event", dispute_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FallbackDisputeSettledEvent {
    #[topic]
    pub dispute_id: u32,
    /// The oracle's ruling: `true` refund, `false` deny.
    pub refund: bool,
    pub oracle: Address,
    pub payment_ref: BytesN<32>,
}

/// Ledgers a settled dispute stays readable for (30 days at ~5 s/ledger), so
/// the outcome can be audited off-chain after the fact.
const DISPUTE_TTL: u32 = 518_400;

pub(crate) fn request_dispute(
    env: &Env,
    merchant: &Address,
    oracle: &Address,
    payment_ref: &BytesN<32>,
    recipient: &Address,
    amount: i128,
) -> Result<u32, Error> {
    merchant.require_auth();
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    let id: u32 = env
        .storage()
        .instance()
        .get(&FallbackDataKey::Count)
        .unwrap_or(0)
        + 1;
    env.storage().instance().set(&FallbackDataKey::Count, &id);

    let dispute = FallbackDispute {
        payment_ref: payment_ref.clone(),
        merchant: merchant.clone(),
        recipient: recipient.clone(),
        amount,
        oracle: oracle.clone(),
        request_ledger: env.ledger().sequence(),
        open: true,
    };
    let key = FallbackDataKey::Dispute(id);
    env.storage().persistent().set(&key, &dispute);
    // Threshold == extend_to so a freshly written entry is actually extended
    // past the network's minimum-persistent-TTL floor.
    env.storage()
        .persistent()
        .extend_ttl(&key, DISPUTE_TTL, DISPUTE_TTL);

    FallbackDisputeRequestedEvent {
        dispute_id: id,
        oracle: oracle.clone(),
        payment_ref: payment_ref.clone(),
        merchant: merchant.clone(),
        recipient: recipient.clone(),
        amount,
    }
    .publish(env);

    Ok(id)
}

/// The XDR request payload handed to the fallback oracle for `dispute_id`:
/// the dispute itself, serialized so the oracle (or an off-chain courier)
/// can decode ref, recipient and amount.
pub(crate) fn oracle_request_bytes(env: &Env, dispute_id: u32) -> Result<Bytes, Error> {
    let dispute: FallbackDispute = env
        .storage()
        .persistent()
        .get(&FallbackDataKey::Dispute(dispute_id))
        .ok_or(Error::DisputeNotFound)?;
    Ok(dispute.to_xdr(env))
}

pub(crate) fn settle_dispute(env: &Env, dispute_id: u32, refund: bool) -> Result<(), Error> {
    let key = FallbackDataKey::Dispute(dispute_id);
    let mut dispute: FallbackDispute = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::DisputeNotFound)?;
    if !dispute.open {
        return Err(Error::DisputeClosed);
    }
    // Only the dispute's designated oracle may settle it: `require_auth` on a
    // contract address succeeds only when that contract is the direct caller
    // of this invocation, so no third party can force a ruling.
    dispute.oracle.require_auth();

    dispute.open = false;
    env.storage().persistent().set(&key, &dispute);

    FallbackDisputeSettledEvent {
        dispute_id,
        refund,
        oracle: dispute.oracle,
        payment_ref: dispute.payment_ref,
    }
    .publish(env);

    Ok(())
}

pub(crate) fn get_dispute(env: &Env, dispute_id: u32) -> Result<FallbackDispute, Error> {
    env.storage()
        .persistent()
        .get(&FallbackDataKey::Dispute(dispute_id))
        .ok_or(Error::DisputeNotFound)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{testutils::Address as _, xdr::FromXdr, BytesN, Env};

    fn test_env() -> Env {
        let env = Env::default();
        env.mock_all_auths();
        env
    }

    fn payment_ref(env: &Env, slot: u8) -> BytesN<32> {
        BytesN::from_array(env, &[slot; 32])
    }

    #[test]
    fn dispute_can_be_escalated_and_queried() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 1),
            &recipient,
            &100,
        );
        assert_eq!(dispute_id, 1);

        let dispute = client.get_fallback_dispute(&dispute_id);
        assert!(dispute.open);
        assert_eq!(dispute.merchant, merchant);
        assert_eq!(dispute.oracle, oracle);
        assert_eq!(dispute.recipient, recipient);
        assert_eq!(dispute.amount, 100);
    }

    #[test]
    fn oracle_escalation_assigns_monotonic_ids() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let a = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 1),
            &recipient,
            &10,
        );
        let b = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 2),
            &recipient,
            &10,
        );
        assert_ne!(a, b);
        assert_eq!(a + 1, b);
    }

    #[test]
    fn oracle_request_payload_decodes_to_the_dispute() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 3),
            &recipient,
            &77,
        );
        let payload = client.build_fallback_oracle_request(&dispute_id);
        let decoded = FallbackDispute::from_xdr(&env, &payload).unwrap();
        assert_eq!(decoded.recipient, recipient);
        assert_eq!(decoded.amount, 77);

        assert_eq!(
            client.try_get_fallback_dispute(&99),
            Err(Ok(accensa_common::Error::DisputeNotFound))
        );
    }

    #[test]
    fn oracle_settles_a_dispute_exactly_once() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 4),
            &recipient,
            &50,
        );

        // Ruling: refund.
        client.settle_fallback_dispute(&dispute_id, &true);
        assert!(!client.get_fallback_dispute(&dispute_id).open);

        // A dispute settles exactly once.
        assert_eq!(
            client.try_settle_fallback_dispute(&dispute_id, &false),
            Err(Ok(accensa_common::Error::DisputeClosed))
        );
    }
}
