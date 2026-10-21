//! Privacy contract tests (issue #440).

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    Address, Bytes, BytesN, Env, Event, Vec,
};

const START: u64 = 1_700_000_000;

struct Setup {
    env: Env,
    client: PrivacyClient<'static>,
    admin: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = START);

    let admin = Address::generate(&env);
    let contract = env.register(Privacy, (admin.clone(),));
    let client = PrivacyClient::new(&env, &contract);

    Setup { env, client, admin }
}

fn valid_proof(env: &Env) -> Groth16Proof {
    Groth16Proof {
        a: Bytes::from_slice(env, &[1u8; 64]),
        b: Bytes::from_slice(env, &[2u8; 128]),
        c: Bytes::from_slice(env, &[3u8; 64]),
    }
}

fn zero_proof(env: &Env) -> Groth16Proof {
    Groth16Proof {
        a: Bytes::from_slice(env, &[0u8; 64]),
        b: Bytes::from_slice(env, &[0u8; 128]),
        c: Bytes::from_slice(env, &[0u8; 64]),
    }
}

fn empty_proof(env: &Env) -> Groth16Proof {
    Groth16Proof {
        a: Bytes::new(env),
        b: Bytes::new(env),
        c: Bytes::new(env),
    }
}

/// VK whose IC vector has `public_input_count + 1` elements.
fn valid_vk(env: &Env, public_input_count: u32) -> VerifyingKey {
    let mut ic = Vec::new(env);
    for i in 0..=public_input_count {
        ic.push_back(Bytes::from_slice(env, &[(i + 10) as u8; 64]));
    }
    VerifyingKey {
        alpha_g1: Bytes::from_slice(env, &[4u8; 64]),
        beta_g2: Bytes::from_slice(env, &[5u8; 128]),
        gamma_g2: Bytes::from_slice(env, &[6u8; 128]),
        delta_g2: Bytes::from_slice(env, &[7u8; 128]),
        ic,
    }
}

fn value_n32(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

fn inputs_with_commitment(
    env: &Env,
    amount: i128,
    value: &BytesN<32>,
    blinding: &BytesN<32>,
) -> Vec<BytesN<32>> {
    let mut inputs = Vec::new(env);
    inputs.push_back(public_input_from_i128(env, amount));
    inputs.push_back(value.clone());
    inputs.push_back(blinding.clone());
    inputs
}

// ── Commitment ────────────────────────────────────────────────────────────

#[test]
fn test_commitment_is_deterministic() {
    let env = Env::default();
    let value = value_n32(&env, 9);
    let blinding = value_n32(&env, 8);
    let c1 = commit(&env, &value, &blinding);
    let c2 = commit(&env, &value, &blinding);
    assert_eq!(c1, c2);
    assert_ne!(c1, BytesN::from_array(&env, &[0u8; 32]));
}

#[test]
fn test_commitment_differs_with_different_blinding() {
    let env = Env::default();
    let value = value_n32(&env, 9);
    let b1 = value_n32(&env, 8);
    let b2 = value_n32(&env, 7);
    assert_ne!(commit(&env, &value, &b1), commit(&env, &value, &b2));
    assert_ne!(
        commit(&env, &value, &b1),
        commit(&env, &value_n32(&env, 1), &b1)
    );
}

// ── Groth16 structural verification (module-level) ───────────────────────

#[test]
fn test_verify_groth16_valid_proof() {
    let env = Env::default();
    let proof = valid_proof(&env);
    let vk = valid_vk(&env, 1);
    let mut public_inputs = Vec::new(&env);
    public_inputs.push_back(value_n32(&env, 99));

    assert!(verify_groth16(&env, &proof, &vk, &public_inputs).unwrap());
}

#[test]
fn test_verify_groth16_rejects_empty_proof() {
    let env = Env::default();
    let proof = empty_proof(&env);
    let vk = valid_vk(&env, 1);
    let mut public_inputs = Vec::new(&env);
    public_inputs.push_back(value_n32(&env, 99));

    assert!(!verify_groth16(&env, &proof, &vk, &public_inputs).unwrap());
}

#[test]
fn test_verify_groth16_rejects_zero_points() {
    let env = Env::default();
    let proof = zero_proof(&env);
    let vk = valid_vk(&env, 1);
    let mut public_inputs = Vec::new(&env);
    public_inputs.push_back(value_n32(&env, 99));

    assert!(!verify_groth16(&env, &proof, &vk, &public_inputs).unwrap());
}

#[test]
fn test_verify_groth16_rejects_wrong_public_input_count() {
    let env = Env::default();
    let proof = valid_proof(&env);
    // VK built for 1 public input (ic.len() == 2); supply two inputs.
    let vk = valid_vk(&env, 1);
    let mut mismatched = Vec::new(&env);
    mismatched.push_back(value_n32(&env, 99));
    mismatched.push_back(value_n32(&env, 100));
    assert!(!verify_groth16(&env, &proof, &vk, &mismatched).unwrap());

    // VK built for 2 inputs; supply one.
    let vk2 = valid_vk(&env, 2);
    let mut short = Vec::new(&env);
    short.push_back(value_n32(&env, 99));
    assert!(!verify_groth16(&env, &proof, &vk2, &short).unwrap());
}

// ── Contract: escrow lifecycle ───────────────────────────────────────────

#[test]
fn test_create_escrow_stores_commitment() {
    let s = setup();
    let commitment = value_n32(&s.env, 7);
    let id = s.client.create_escrow(&commitment, &50);

    assert_eq!(id, 0);
    let escrow = s.client.get_escrow(&id).unwrap();
    assert_eq!(escrow.commitment, commitment);
    assert_eq!(escrow.min_amount, 50);
    assert_eq!(escrow.status, EscrowStatus::Pending);

    let id2 = s.client.create_escrow(&value_n32(&s.env, 6), &10);
    assert_eq!(id2, 1);
}

#[test]
fn test_create_escrow_rejects_zero_commitment() {
    let s = setup();
    assert_eq!(
        s.client
            .try_create_escrow(&BytesN::from_array(&s.env, &[0u8; 32]), &10),
        Err(Ok(Error::InvalidCommitment))
    );
}

#[test]
fn test_verify_escrow_accepts_valid_proof() {
    let s = setup();
    let vk = valid_vk(&s.env, 3);
    s.client.set_verifying_key(&vk);

    let value = value_n32(&s.env, 9);
    let blinding = value_n32(&s.env, 8);
    let commitment = commit(&s.env, &value, &blinding);
    let id = s.client.create_escrow(&commitment, &100);

    let inputs = inputs_with_commitment(&s.env, 150, &value, &blinding);
    assert!(s.client.verify_escrow(&id, &valid_proof(&s.env), &inputs));

    let escrow = s.client.get_escrow(&id).unwrap();
    assert_eq!(escrow.status, EscrowStatus::Verified);
}

#[test]
fn test_verify_escrow_rejects_invalid_proof() {
    let s = setup();
    let vk = valid_vk(&s.env, 3);
    s.client.set_verifying_key(&vk);

    let value = value_n32(&s.env, 9);
    let blinding = value_n32(&s.env, 8);
    let commitment = commit(&s.env, &value, &blinding);
    let id = s.client.create_escrow(&commitment, &100);

    let inputs = inputs_with_commitment(&s.env, 150, &value, &blinding);
    assert!(!s.client.verify_escrow(&id, &zero_proof(&s.env), &inputs));
    assert_eq!(
        s.client.get_escrow(&id).unwrap().status,
        EscrowStatus::Rejected
    );

    // A rejected escrow cannot be re-verified.
    assert_eq!(
        s.client
            .try_verify_escrow(&id, &valid_proof(&s.env), &inputs),
        Err(Ok(Error::AlreadyVerified))
    );
}

#[test]
fn test_verify_escrow_rejects_amount_below_minimum() {
    let s = setup();
    let vk = valid_vk(&s.env, 3);
    s.client.set_verifying_key(&vk);

    let value = value_n32(&s.env, 9);
    let blinding = value_n32(&s.env, 8);
    let commitment = commit(&s.env, &value, &blinding);
    let id = s.client.create_escrow(&commitment, &100);

    let inputs = inputs_with_commitment(&s.env, 50, &value, &blinding);
    assert_eq!(
        s.client
            .try_verify_escrow(&id, &valid_proof(&s.env), &inputs),
        Err(Ok(Error::AmountBelowMinimum))
    );
}

#[test]
fn test_verify_escrow_rejects_double_verification() {
    let s = setup();
    let vk = valid_vk(&s.env, 3);
    s.client.set_verifying_key(&vk);

    let value = value_n32(&s.env, 9);
    let blinding = value_n32(&s.env, 8);
    let commitment = commit(&s.env, &value, &blinding);
    let id = s.client.create_escrow(&commitment, &100);
    let inputs = inputs_with_commitment(&s.env, 150, &value, &blinding);

    assert!(s.client.verify_escrow(&id, &valid_proof(&s.env), &inputs));
    assert_eq!(
        s.client
            .try_verify_escrow(&id, &valid_proof(&s.env), &inputs),
        Err(Ok(Error::AlreadyVerified))
    );
}

#[test]
fn test_verify_escrow_rejects_commitment_mismatch() {
    let s = setup();
    let vk = valid_vk(&s.env, 3);
    s.client.set_verifying_key(&vk);

    let value = value_n32(&s.env, 9);
    let blinding = value_n32(&s.env, 8);
    // Escrow commits to (9, 8) but the proof claims (1, 8).
    let commitment = commit(&s.env, &value, &blinding);
    let id = s.client.create_escrow(&commitment, &100);

    let wrong_value = value_n32(&s.env, 1);
    let inputs = inputs_with_commitment(&s.env, 150, &wrong_value, &blinding);
    assert!(!s.client.verify_escrow(&id, &valid_proof(&s.env), &inputs));
    assert_eq!(
        s.client.get_escrow(&id).unwrap().status,
        EscrowStatus::Rejected
    );
}

#[test]
fn test_get_verifying_key_requires_config() {
    let s = setup();
    assert_eq!(
        s.client.try_get_verifying_key(),
        Err(Ok(Error::InvalidProof))
    );

    let vk = valid_vk(&s.env, 1);
    s.client.set_verifying_key(&vk);
    assert_eq!(s.client.get_verifying_key(), vk);
}

#[test]
fn test_constructor_binds_admin() {
    let s = setup();
    assert_eq!(s.client.get_admin(), s.admin);
}

// ── Escrow lifecycle events (issue #609) ──────────────────────────────────

/// The three escrow events, compared in the XDR form the contract actually
/// publishes rather than field by field, so a changed `#[topic]`, a reordered
/// payload or a retyped field fails the test instead of sliding past it.
///
/// Each case drives the branch that emits the event, using the real verifier
/// and the proving-key fixtures the cases above already use: `zero_proof` is
/// what makes `verify_concealed_amount` return `false` (the reject branch),
/// and `valid_proof` what makes it return `true`.
///
/// The rejected branch is the one worth pinning: `verify_escrow` returns
/// `Ok(false)` there rather than erroring, so the invocation *succeeds*, its
/// writes commit and its events are observable — unlike a failed call, whose
/// events the host discards. That is what makes "exactly one event, and not
/// the verified type" a claim this test can actually falsify.
mod escrow_events {
    use super::*;

    /// The created event carries the id and the exact payload stored for it.
    #[test]
    fn created_escrow_event_matches_storage() {
        let s = setup();
        let commitment = value_n32(&s.env, 7);
        let id = s.client.create_escrow(&commitment, &50);

        let events = s.env.events().all().filter_by_contract(&s.client.address);
        assert_eq!(events.events().len(), 1);
        assert_eq!(
            &events.events()[0],
            &EscrowCreatedEvent {
                escrow_id: id,
                commitment: commitment.clone(),
                min_amount: 50,
            }
            .to_xdr(&s.env, &s.client.address)
        );

        // The event and the stored record cannot diverge.
        let escrow = s.client.get_escrow(&id).unwrap();
        assert_eq!(escrow.commitment, commitment);
        assert_eq!(escrow.min_amount, 50);
        assert_eq!(escrow.status, EscrowStatus::Pending);
    }

    /// A proof that fails the commitment rejects the escrow and emits exactly
    /// the rejected event — never the verified one.
    #[test]
    fn rejected_verification_emits_only_the_rejected_event() {
        let s = setup();
        s.client.set_verifying_key(&valid_vk(&s.env, 3));

        let value = value_n32(&s.env, 9);
        let blinding = value_n32(&s.env, 8);
        let id = s
            .client
            .create_escrow(&commit(&s.env, &value, &blinding), &100);

        let inputs = inputs_with_commitment(&s.env, 150, &value, &blinding);
        assert!(!s.client.verify_escrow(&id, &zero_proof(&s.env), &inputs));

        // `events().all()` covers the last invocation only: the refusal.
        let events = s.env.events().all().filter_by_contract(&s.client.address);
        assert_eq!(events.events().len(), 1);
        assert_eq!(
            &events.events()[0],
            &EscrowRejectedEvent { escrow_id: id }.to_xdr(&s.env, &s.client.address)
        );
        assert_ne!(
            &events.events()[0],
            &EscrowVerifiedEvent { escrow_id: id }.to_xdr(&s.env, &s.client.address)
        );

        assert_eq!(
            s.client.get_escrow(&id).unwrap().status,
            EscrowStatus::Rejected
        );
    }

    /// A proof the verifier accepts emits exactly the verified event.
    #[test]
    fn verified_escrow_emits_only_the_verified_event() {
        let s = setup();
        s.client.set_verifying_key(&valid_vk(&s.env, 3));

        let value = value_n32(&s.env, 9);
        let blinding = value_n32(&s.env, 8);
        let id = s
            .client
            .create_escrow(&commit(&s.env, &value, &blinding), &100);

        let inputs = inputs_with_commitment(&s.env, 150, &value, &blinding);
        assert!(s.client.verify_escrow(&id, &valid_proof(&s.env), &inputs));

        let events = s.env.events().all().filter_by_contract(&s.client.address);
        assert_eq!(events.events().len(), 1);
        assert_eq!(
            &events.events()[0],
            &EscrowVerifiedEvent { escrow_id: id }.to_xdr(&s.env, &s.client.address)
        );
        assert_eq!(
            s.client.get_escrow(&id).unwrap().status,
            EscrowStatus::Verified
        );
    }

    /// The two input-driven refusals. Neither publishes: the amount floor is a
    /// hard error by design (callers distinguish "wrong proof" from "honest
    /// but too small" by the error, not by an event), and the all-zero
    /// commitment is refused before anything is written.
    #[test]
    fn refused_create_and_low_amount_are_errors() {
        let s = setup();
        s.client.set_verifying_key(&valid_vk(&s.env, 3));

        assert_eq!(
            s.client
                .try_create_escrow(&BytesN::from_array(&s.env, &[0u8; 32]), &10),
            Err(Ok(Error::InvalidCommitment))
        );

        let value = value_n32(&s.env, 9);
        let blinding = value_n32(&s.env, 8);
        let id = s
            .client
            .create_escrow(&commit(&s.env, &value, &blinding), &100);

        let below_floor = inputs_with_commitment(&s.env, 50, &value, &blinding);
        assert_eq!(
            s.client
                .try_verify_escrow(&id, &valid_proof(&s.env), &below_floor),
            Err(Ok(Error::AmountBelowMinimum))
        );
        // The refused verification left the escrow pending, not rejected.
        assert_eq!(
            s.client.get_escrow(&id).unwrap().status,
            EscrowStatus::Pending
        );
    }
}
