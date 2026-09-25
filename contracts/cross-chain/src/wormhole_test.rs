#![cfg(test)]

extern crate std;

use accensa_common::Error;
use k256::ecdsa::SigningKey;
use soroban_sdk::{testutils::Address as _, vec, Address, Bytes, BytesN, Env, Vec};

use crate::{
    wormhole::{
        hash_vaa_body, parse_vaa, pubkey_to_address, verify_vaa, GuardianAddress, GuardianSet,
        GuardianSignature,
    },
    CrossChainBridge, CrossChainBridgeClient,
};

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

fn build_dummy_body_bytes(payload: &[u8]) -> std::vec::Vec<u8> {
    let mut body = std::vec::Vec::new();
    body.extend_from_slice(&1700000000u32.to_be_bytes()); // timestamp
    body.extend_from_slice(&123u32.to_be_bytes()); // nonce
    body.extend_from_slice(&2u16.to_be_bytes()); // emitter_chain (Ethereum = 2 in Wormhole)
    body.extend_from_slice(&[0xaa; 32]); // emitter_address
    body.extend_from_slice(&456u64.to_be_bytes()); // sequence
    body.push(15); // consistency_level
    body.extend_from_slice(payload);
    body
}

#[test]
fn test_parse_and_verify_valid_vaa() {
    let env = Env::default();

    // 3 guardians (quorum is (3*2)/3 + 1 = 3)
    let (g1_sk, g1_addr) = create_test_guardian(&env, 1);
    let (g2_sk, g2_addr) = create_test_guardian(&env, 2);
    let (g3_sk, g3_addr) = create_test_guardian(&env, 3);

    let guardian_keys = vec![&env, g1_addr, g2_addr, g3_addr];
    let guardian_set = GuardianSet {
        index: 0,
        keys: guardian_keys,
    };
    assert_eq!(guardian_set.quorum(), 3);

    let payload = b"hello wormhole cross-chain state proof";
    let body_vec = build_dummy_body_bytes(payload);
    let body_bytes = Bytes::from_slice(&env, &body_vec);

    let digest = hash_vaa_body(&env, &body_bytes);

    let mut sigs = Vec::new(&env);
    sigs.push_back(sign_digest(&env, &g1_sk, &digest, 0));
    sigs.push_back(sign_digest(&env, &g2_sk, &digest, 1));
    sigs.push_back(sign_digest(&env, &g3_sk, &digest, 2));

    let raw_vaa = build_vaa_bytes(&env, 1, 0, &sigs, &body_vec);

    let parsed = parse_vaa(&env, &raw_vaa).expect("failed to parse valid VAA");
    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.guardian_set_index, 0);
    assert_eq!(parsed.signatures.len(), 3);
    assert_eq!(parsed.body.sequence, 456);
    assert_eq!(parsed.body.emitter_chain, 2);
    assert_eq!(parsed.body.payload, Bytes::from_slice(&env, payload));

    assert_eq!(verify_vaa(&env, &parsed, &guardian_set), Ok(()));
}

#[test]
fn test_verify_vaa_invalid_signature_rejected() {
    let env = Env::default();

    let (g1_sk, g1_addr) = create_test_guardian(&env, 1);
    let (g2_sk, g2_addr) = create_test_guardian(&env, 2);
    let (_g3_sk, g3_addr) = create_test_guardian(&env, 3);
    let (attacker_sk, _attacker_addr) = create_test_guardian(&env, 99);

    let guardian_keys = vec![&env, g1_addr, g2_addr, g3_addr];
    let guardian_set = GuardianSet {
        index: 0,
        keys: guardian_keys,
    };

    let body_vec = build_dummy_body_bytes(b"tampered state");
    let body_bytes = Bytes::from_slice(&env, &body_vec);
    let digest = hash_vaa_body(&env, &body_bytes);

    let mut sigs = Vec::new(&env);
    sigs.push_back(sign_digest(&env, &g1_sk, &digest, 0));
    sigs.push_back(sign_digest(&env, &g2_sk, &digest, 1));
    // Attacker signs for guardian index 2
    sigs.push_back(sign_digest(&env, &attacker_sk, &digest, 2));

    let raw_vaa = build_vaa_bytes(&env, 1, 0, &sigs, &body_vec);
    let parsed = parse_vaa(&env, &raw_vaa).unwrap();

    assert_eq!(
        verify_vaa(&env, &parsed, &guardian_set),
        Err(Error::InvalidSignature)
    );
}

#[test]
fn test_verify_vaa_insufficient_quorum_rejected() {
    let env = Env::default();

    let (g1_sk, g1_addr) = create_test_guardian(&env, 1);
    let (_g2_sk, g2_addr) = create_test_guardian(&env, 2);
    let (_g3_sk, g3_addr) = create_test_guardian(&env, 3);

    let guardian_set = GuardianSet {
        index: 0,
        keys: vec![&env, g1_addr, g2_addr, g3_addr],
    };

    let body_vec = build_dummy_body_bytes(b"payload");
    let body_bytes = Bytes::from_slice(&env, &body_vec);
    let digest = hash_vaa_body(&env, &body_bytes);

    // Only 1 signature, but quorum requires 3
    let mut sigs = Vec::new(&env);
    sigs.push_back(sign_digest(&env, &g1_sk, &digest, 0));

    let raw_vaa = build_vaa_bytes(&env, 1, 0, &sigs, &body_vec);
    let parsed = parse_vaa(&env, &raw_vaa).unwrap();

    assert_eq!(
        verify_vaa(&env, &parsed, &guardian_set),
        Err(Error::InvalidProof)
    );
}

#[test]
fn test_verify_vaa_out_of_order_guardian_index_rejected() {
    let env = Env::default();

    let (g1_sk, g1_addr) = create_test_guardian(&env, 1);
    let (g2_sk, g2_addr) = create_test_guardian(&env, 2);
    let (g3_sk, g3_addr) = create_test_guardian(&env, 3);

    let guardian_set = GuardianSet {
        index: 0,
        keys: vec![&env, g1_addr, g2_addr, g3_addr],
    };

    let body_vec = build_dummy_body_bytes(b"payload");
    let body_bytes = Bytes::from_slice(&env, &body_vec);
    let digest = hash_vaa_body(&env, &body_bytes);

    // Indices out of order: 1, 0, 2
    let mut sigs = Vec::new(&env);
    sigs.push_back(sign_digest(&env, &g2_sk, &digest, 1));
    sigs.push_back(sign_digest(&env, &g1_sk, &digest, 0));
    sigs.push_back(sign_digest(&env, &g3_sk, &digest, 2));

    let raw_vaa = build_vaa_bytes(&env, 1, 0, &sigs, &body_vec);
    let parsed = parse_vaa(&env, &raw_vaa).unwrap();

    assert_eq!(
        verify_vaa(&env, &parsed, &guardian_set),
        Err(Error::InvalidSignature)
    );
}

#[test]
fn test_verify_vaa_guardian_set_index_mismatch() {
    let env = Env::default();

    let (g1_sk, g1_addr) = create_test_guardian(&env, 1);
    let guardian_set = GuardianSet {
        index: 1, // Active set is index 1
        keys: vec![&env, g1_addr],
    };

    let body_vec = build_dummy_body_bytes(b"payload");
    let body_bytes = Bytes::from_slice(&env, &body_vec);
    let digest = hash_vaa_body(&env, &body_bytes);

    let mut sigs = Vec::new(&env);
    sigs.push_back(sign_digest(&env, &g1_sk, &digest, 0));

    // VAA is for guardian_set_index 0
    let raw_vaa = build_vaa_bytes(&env, 1, 0, &sigs, &body_vec);
    let parsed = parse_vaa(&env, &raw_vaa).unwrap();

    assert_eq!(
        verify_vaa(&env, &parsed, &guardian_set),
        Err(Error::Unauthorized)
    );
}

#[test]
fn test_parse_vaa_invalid_version_or_length() {
    let env = Env::default();

    // Too short
    let short_bytes = Bytes::from_slice(&env, &[1, 2, 3]);
    assert_eq!(parse_vaa(&env, &short_bytes), Err(Error::InvalidProof));

    // Invalid version != 1
    let body_vec = build_dummy_body_bytes(b"payload");
    let sigs = Vec::new(&env);
    let invalid_version_vaa = build_vaa_bytes(&env, 2, 0, &sigs, &body_vec);
    assert_eq!(
        parse_vaa(&env, &invalid_version_vaa),
        Err(Error::InvalidProof)
    );
}

#[test]
fn test_contract_verify_and_parse_vaa_integration() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token, &1);

    assert_eq!(client.get_admin(), admin);

    let (g1_sk, g1_addr) = create_test_guardian(&env, 1);
    let (g2_sk, g2_addr) = create_test_guardian(&env, 2);
    let (g3_sk, g3_addr) = create_test_guardian(&env, 3);

    let guardian_set = GuardianSet {
        index: 1,
        keys: vec![&env, g1_addr, g2_addr, g3_addr],
    };

    client.set_guardian_set(&admin, &guardian_set);
    assert_eq!(client.get_guardian_set(&1), Some(guardian_set.clone()));
    assert_eq!(client.get_current_guardian_set_index(), 1);

    let payload = b"state proof data from wormhole";
    let body_vec = build_dummy_body_bytes(payload);
    let body_bytes = Bytes::from_slice(&env, &body_vec);
    let digest = hash_vaa_body(&env, &body_bytes);

    let mut sigs = Vec::new(&env);
    sigs.push_back(sign_digest(&env, &g1_sk, &digest, 0));
    sigs.push_back(sign_digest(&env, &g2_sk, &digest, 1));
    sigs.push_back(sign_digest(&env, &g3_sk, &digest, 2));

    let raw_vaa = build_vaa_bytes(&env, 1, 1, &sigs, &body_vec);

    let extracted_body = client.verify_and_parse_vaa(&raw_vaa);
    assert_eq!(extracted_body.sequence, 456);
    assert_eq!(extracted_body.payload, Bytes::from_slice(&env, payload));
}

#[test]
fn test_contract_missing_guardian_set_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token, &1);

    let body_vec = build_dummy_body_bytes(b"test");
    let sigs = Vec::new(&env);
    // VAA specifies guardian_set_index 99 which is not registered
    let raw_vaa = build_vaa_bytes(&env, 1, 99, &sigs, &body_vec);

    let res = client.try_verify_and_parse_vaa(&raw_vaa);
    assert!(res.is_err());
}
