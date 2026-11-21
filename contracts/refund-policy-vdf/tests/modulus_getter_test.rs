//! Integration coverage for `VdfPolicy::get_modulus_fingerprint` (issue #988).

use refund_policy_vdf::{VdfPolicy, VdfPolicyClient};
use soroban_sdk::{Bytes, BytesN, Env};

// sha256(vdf::MODULUS), the fixed 1024-bit modulus baked into the contract.
// Recompute only when the modulus is rotated.
const MODULUS_FINGERPRINT: [u8; 32] = [
    0xb3, 0x96, 0x11, 0x14, 0x07, 0x16, 0xa7, 0xa4, 0x06, 0xb9, 0xb3, 0xd1, 0x21, 0xe6, 0xef, 0x7c,
    0xbd, 0xc0, 0x84, 0xdd, 0x5c, 0xc7, 0x08, 0x08, 0xc2, 0x99, 0x79, 0x9e, 0x11, 0x56, 0xfe, 0x6d,
];

#[test]
fn get_modulus_fingerprint_is_stable_and_pinned() {
    let env = Env::default();
    let contract_id = env.register(VdfPolicy, ());
    let client = VdfPolicyClient::new(&env, &contract_id);

    let expected = BytesN::from_array(&env, &MODULUS_FINGERPRINT);
    let got = client.get_modulus_fingerprint();
    assert_eq!(got, expected);

    // Read-only and deterministic: no auth, no storage access, same bytes twice.
    assert_eq!(client.get_modulus_fingerprint(), got);

    // Differs from sha256 of an all-zero 128-byte buffer.
    let zero_digest = env.crypto().sha256(&Bytes::from_array(&env, &[0u8; 128]));
    let zero = BytesN::from_array(&env, &zero_digest.to_array());
    assert_ne!(got, zero);
}
