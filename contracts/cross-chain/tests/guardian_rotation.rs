//! Integration tests for guardian set rotation and rejection of rotated-out guardian sets (issue #1034).

#![cfg(test)]

extern crate std;

use accensa_common::Error;
use accensa_cross_chain::{
    hash_vaa_body, parse_vaa, pubkey_to_address, verify_vaa, CrossChainBridge,
    CrossChainBridgeClient, GuardianAddress, GuardianSet, GuardianSignature,
};
use k256::ecdsa::SigningKey;
use soroban_sdk::{testutils::Address as _, vec, Address, Bytes, BytesN, Env, Vec};

fn create_test_guardian(env: &Env, seed: u8) -> (SigningKey, GuardianAddress) {
    let mut key_bytes = [0u8; 32];
    key_bytes[0] = seed;
    key_bytes[31] = seed;
    let sk = SigningKey::from_bytes(&key_bytes.into()).unwrap();
    let vk = sk.verifying_key();
    let uncompressed = vk.to_encoded_point(false);
    let pubkey_bytes: BytesN<65> =
        BytesN::from_array(env, uncompressed.as_bytes().try_into().unwrap());
    let addr = pubkey_to_address(env, &pubkey_bytes);
    (sk, addr)
}

fn sign_digest(
    env: &Env,
    sk: &SigningKey,
    digest: &soroban_sdk::crypto::Hash<32>,
    guardian_index: u32,
) -> GuardianSignature {
    let digest_bytes = digest.to_array();
    let (sig, rec_id) = sk.sign_prehash_recoverable(&digest_bytes).unwrap();
    let sig_bytes: [u8; 64] = sig.to_bytes().into();

    GuardianSignature {
        guardian_index,
        signature: BytesN::from_array(env, &sig_bytes),
        recovery_id: rec_id.to_byte() as u32,
    }
}

fn build_vaa_bytes(
    env: &Env,
    version: u8,
    guardian_set_index: u32,
    signatures: &Vec<GuardianSignature>,
    body_bytes: &[u8],
) -> Bytes {
    let mut out = std::vec::Vec::new();
    out.push(version);
    out.extend_from_slice(&guardian_set_index.to_be_bytes());
    out.push(signatures.len() as u8);

    for i in 0..signatures.len() {
        let sig = signatures.get(i).unwrap();
        out.push(sig.guardian_index as u8);
        out.extend_from_slice(&sig.signature.to_array());
        out.push(sig.recovery_id as u8);
    }

    out.extend_from_slice(body_bytes);
    Bytes::from_slice(env, &out)
}

fn build_dummy_body_bytes(payload: &[u8], sequence: u64) -> std::vec::Vec<u8> {
    let mut body = std::vec::Vec::new();
    body.extend_from_slice(&1705000000u32.to_be_bytes()); // timestamp
    body.extend_from_slice(&42u32.to_be_bytes()); // nonce
    body.extend_from_slice(&2u16.to_be_bytes()); // emitter_chain (Ethereum = 2)
    body.extend_from_slice(&[0xaa; 32]); // emitter_address
    body.extend_from_slice(&sequence.to_be_bytes()); // sequence
    body.push(15); // consistency_level
    body.extend_from_slice(payload);
    body
}

#[test]
fn test_guardian_rotation_rejects_superseded_guardian_set() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token, &1);

    // Initial guardian set 0 configured during test setup
    let (g0_1_sk, g0_1_addr) = create_test_guardian(&env, 1);
    let (g0_2_sk, g0_2_addr) = create_test_guardian(&env, 2);
    let (g0_3_sk, g0_3_addr) = create_test_guardian(&env, 3);

    let set_0 = GuardianSet {
        index: 0,
        keys: vec![&env, g0_1_addr, g0_2_addr, g0_3_addr],
    };
    client.set_guardian_set(&admin, &set_0);
    assert_eq!(client.get_current_guardian_set_index(), 0);
    assert_eq!(client.get_guardian_set(&0), Some(set_0.clone()));

    // Rotate to guardian set 1 with completely new validator keys
    let (g1_1_sk, g1_1_addr) = create_test_guardian(&env, 11);
    let (g1_2_sk, g1_2_addr) = create_test_guardian(&env, 12);
    let (g1_3_sk, g1_3_addr) = create_test_guardian(&env, 13);

    let set_1 = GuardianSet {
        index: 1,
        keys: vec![&env, g1_1_addr, g1_2_addr, g1_3_addr],
    };
    client.set_guardian_set(&admin, &set_1);
    assert_eq!(client.get_current_guardian_set_index(), 1);
    assert_eq!(client.get_guardian_set(&1), Some(set_1.clone()));

    let active_guardian_set = client
        .get_guardian_set(&client.get_current_guardian_set_index())
        .expect("active guardian set must exist");
    assert_eq!(active_guardian_set.index, 1);

    // 1. Build a VAA signed by the superseded guardian set 0
    let payload = b"transferred cross-chain payload";
    let body_bytes_0 = build_dummy_body_bytes(payload, 100);
    let digest_0 = hash_vaa_body(&env, &Bytes::from_slice(&env, &body_bytes_0));

    let mut sigs_0 = Vec::new(&env);
    sigs_0.push_back(sign_digest(&env, &g0_1_sk, &digest_0, 0));
    sigs_0.push_back(sign_digest(&env, &g0_2_sk, &digest_0, 1));
    sigs_0.push_back(sign_digest(&env, &g0_3_sk, &digest_0, 2));

    let raw_vaa_0 = build_vaa_bytes(&env, 1, 0, &sigs_0, &body_bytes_0);
    let parsed_0 = parse_vaa(&env, &raw_vaa_0).expect("valid VAA wire encoding");

    // Assert that the superseded VAA is rejected when verified against the active rotated guardian set
    let verify_res_0 = verify_vaa(&env, &parsed_0, &active_guardian_set);
    assert_eq!(
        verify_res_0,
        Err(Error::Unauthorized),
        "VAA signed by superseded set 0 must fail verification against active set 1"
    );

    // 2. Forged VAA claiming active guardian_set_index = 1 but signed with old set 0 keys must be rejected
    let raw_forged_vaa = build_vaa_bytes(&env, 1, 1, &sigs_0, &body_bytes_0);
    let parsed_forged = parse_vaa(&env, &raw_forged_vaa).expect("valid VAA wire encoding");
    let verify_res_forged = verify_vaa(&env, &parsed_forged, &active_guardian_set);
    assert_eq!(
        verify_res_forged,
        Err(Error::InvalidSignature),
        "forged VAA with old keys claiming new set must fail signature verification"
    );

    // 3. Build a valid VAA signed by the active guardian set 1
    let body_bytes_1 = build_dummy_body_bytes(payload, 101);
    let digest_1 = hash_vaa_body(&env, &Bytes::from_slice(&env, &body_bytes_1));

    let mut sigs_1 = Vec::new(&env);
    sigs_1.push_back(sign_digest(&env, &g1_1_sk, &digest_1, 0));
    sigs_1.push_back(sign_digest(&env, &g1_2_sk, &digest_1, 1));
    sigs_1.push_back(sign_digest(&env, &g1_3_sk, &digest_1, 2));

    let raw_vaa_1 = build_vaa_bytes(&env, 1, 1, &sigs_1, &body_bytes_1);
    let parsed_1 = parse_vaa(&env, &raw_vaa_1).expect("valid VAA wire encoding");

    // VAA signed by active set 1 succeeds verification
    let verify_res_1 = verify_vaa(&env, &parsed_1, &active_guardian_set);
    assert_eq!(verify_res_1, Ok(()));

    // Full contract verification and parse succeeds for active set 1
    let body = client.verify_and_parse_vaa(&raw_vaa_1);
    assert_eq!(body.sequence, 101);
    assert_eq!(body.payload, Bytes::from_slice(&env, payload));
}

#[test]
fn test_unregistered_guardian_set_fails_with_root_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token, &1);

    let (g_sk, _g_addr) = create_test_guardian(&env, 1);
    let body_bytes = build_dummy_body_bytes(b"test", 1);
    let digest = hash_vaa_body(&env, &Bytes::from_slice(&env, &body_bytes));

    let mut sigs = Vec::new(&env);
    sigs.push_back(sign_digest(&env, &g_sk, &digest, 0));

    // VAA references guardian set 99 which has never been registered
    let raw_vaa = build_vaa_bytes(&env, 1, 99, &sigs, &body_bytes);
    let res = client.try_verify_and_parse_vaa(&raw_vaa);
    assert_eq!(res, Err(Ok(Error::RootNotFound)));
}
