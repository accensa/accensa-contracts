//! Linkable spontaneous anonymous group signatures for governance votes.
//!
//! The implementation uses Ristretto255 (curve25519-dalek) and the standard
//! LSAG challenge cycle. Each vote message binds the network, contract,
//! proposal, choice, and ordered ring. Key images additionally bind the
//! network, contract, and proposal, making them linkable only within one
//! proposal.

use curve25519_dalek::{
    constants::RISTRETTO_BASEPOINT_POINT,
    ristretto::{CompressedRistretto, RistrettoPoint},
    scalar::Scalar,
};
use soroban_sdk::{xdr::ToXdr, Bytes, BytesN, Env, Vec};

use crate::RingSignature;

const MESSAGE_DOMAIN: &[u8] = b"ACCENSA_GOVERNANCE_LSAG_V1";
const IMAGE_DOMAIN: &[u8] = b"ACCENSA_GOVERNANCE_LSAG_KEY_IMAGE_V1";
const CHALLENGE_DOMAIN: &[u8] = b"ACCENSA_GOVERNANCE_LSAG_CHALLENGE_V1";

/// The deterministic public payload signed by an LSAG vote.
pub(crate) fn vote_message(
    env: &Env,
    proposal_id: u64,
    support: bool,
    ring: &Vec<BytesN<32>>,
) -> Bytes {
    let mut message = Bytes::new(env);
    message.extend_from_slice(MESSAGE_DOMAIN);
    message.extend_from_slice(&env.ledger().network_id().to_array());
    message.append(&env.current_contract_address().to_xdr(env));
    message.extend_from_slice(&proposal_id.to_be_bytes());
    message.extend_from_slice(&[u8::from(support)]);
    message.extend_from_slice(&ring.len().to_be_bytes());
    for key in ring.iter() {
        message.extend_from_slice(&key.to_array());
    }
    message
}

/// Validate the canonical compressed Ristretto encoding of a public key.
pub(crate) fn valid_public_key(key: &BytesN<32>) -> bool {
    CompressedRistretto(key.to_array())
        .decompress()
        .is_some_and(|point| point != RistrettoPoint::default())
}

/// Verify an LSAG proof without recovering or recording its signer index.
pub(crate) fn verify(
    env: &Env,
    proposal_id: u64,
    ring: &Vec<BytesN<32>>,
    message: &Bytes,
    signature: &RingSignature,
) -> bool {
    if signature.responses.len() != ring.len() {
        return false;
    }
    let Some(image) = CompressedRistretto(signature.key_image.to_array()).decompress() else {
        return false;
    };
    if image == RistrettoPoint::default() {
        return false;
    }
    let Some(initial) = scalar(&signature.initial_challenge) else {
        return false;
    };
    let mut challenge = initial;
    for (key, response) in ring.iter().zip(signature.responses.iter()) {
        let Some(public) = CompressedRistretto(key.to_array()).decompress() else {
            return false;
        };
        let Some(response) = scalar(&response) else {
            return false;
        };
        let hp = hash_to_point(env, proposal_id, &key);
        let left = response * RISTRETTO_BASEPOINT_POINT + challenge * public;
        let right = response * hp + challenge * image;
        challenge = challenge_scalar(env, message, &left, &right);
    }
    challenge == initial
}

fn scalar(encoded: &BytesN<32>) -> Option<Scalar> {
    Option::<Scalar>::from(Scalar::from_canonical_bytes(encoded.to_array()))
}

fn challenge_scalar(
    env: &Env,
    message: &Bytes,
    left: &RistrettoPoint,
    right: &RistrettoPoint,
) -> Scalar {
    let mut preimage = Bytes::new(env);
    preimage.extend_from_slice(CHALLENGE_DOMAIN);
    preimage.extend_from_slice(&message.len().to_be_bytes());
    preimage.append(message);
    preimage.extend_from_slice(left.compress().as_bytes());
    preimage.extend_from_slice(right.compress().as_bytes());
    Scalar::from_bytes_mod_order(env.crypto().sha256(&preimage).to_array())
}

fn hash_to_point(env: &Env, proposal_id: u64, public_key: &BytesN<32>) -> RistrettoPoint {
    let mut scope = Bytes::new(env);
    scope.extend_from_slice(IMAGE_DOMAIN);
    scope.extend_from_slice(&env.ledger().network_id().to_array());
    scope.append(&env.current_contract_address().to_xdr(env));
    scope.extend_from_slice(&proposal_id.to_be_bytes());
    scope.extend_from_slice(&public_key.to_array());
    let mut first = scope.clone();
    first.extend_from_slice(&[0]);
    let mut second = scope;
    second.extend_from_slice(&[1]);
    let mut uniform = [0u8; 64];
    uniform[..32].copy_from_slice(&env.crypto().sha256(&first).to_array());
    uniform[32..].copy_from_slice(&env.crypto().sha256(&second).to_array());
    RistrettoPoint::from_uniform_bytes(&uniform)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn secret(env: &Env, seed: u64) -> BytesN<32> {
        BytesN::from_array(env, &Scalar::from(seed + 1).to_bytes())
    }

    pub fn public(env: &Env, secret: &BytesN<32>) -> BytesN<32> {
        let x = scalar(secret).unwrap();
        BytesN::from_array(env, (x * RISTRETTO_BASEPOINT_POINT).compress().as_bytes())
    }

    /// Deterministic signer used only to produce reproducible unit/integration
    /// vectors; production signing must happen off-chain with fresh nonce data.
    pub fn sign(
        env: &Env,
        proposal_id: u64,
        support: bool,
        ring: &Vec<BytesN<32>>,
        secrets: &[BytesN<32>],
        signer: usize,
    ) -> RingSignature {
        let message = vote_message(env, proposal_id, support, ring);
        let x = scalar(&secrets[signer]).unwrap();
        let image = x * hash_to_point(env, proposal_id, &ring.get(signer as u32).unwrap());
        let alpha = Scalar::from(101u64 + proposal_id + u64::from(support));
        let mut challenges = alloc::vec![Scalar::ZERO; ring.len() as usize];
        let mut responses = alloc::vec![Scalar::ZERO; ring.len() as usize];
        let next = (signer + 1) % ring.len() as usize;
        challenges[next] = challenge_scalar(
            env,
            &message,
            &(alpha * RISTRETTO_BASEPOINT_POINT),
            &(alpha * hash_to_point(env, proposal_id, &ring.get(signer as u32).unwrap())),
        );
        let mut index = next;
        while index != signer {
            responses[index] = Scalar::from(200u64 + index as u64);
            let key = ring.get(index as u32).unwrap();
            let public = CompressedRistretto(key.to_array()).decompress().unwrap();
            let hp = hash_to_point(env, proposal_id, &key);
            let left = responses[index] * RISTRETTO_BASEPOINT_POINT + challenges[index] * public;
            let right = responses[index] * hp + challenges[index] * image;
            challenges[(index + 1) % ring.len() as usize] =
                challenge_scalar(env, &message, &left, &right);
            index = (index + 1) % ring.len() as usize;
        }
        responses[signer] = alpha - challenges[signer] * x;
        let mut sdk_responses = Vec::new(env);
        for response in responses {
            sdk_responses.push_back(BytesN::from_array(env, &response.to_bytes()));
        }
        RingSignature {
            key_image: BytesN::from_array(env, image.compress().as_bytes()),
            initial_challenge: BytesN::from_array(env, &challenges[0].to_bytes()),
            responses: sdk_responses,
        }
    }
}
