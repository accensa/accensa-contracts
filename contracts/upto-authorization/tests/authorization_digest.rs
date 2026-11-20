use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, BytesN, Env,
};
use upto_authorization::{UptoAuthorization, UptoAuthorizationClient};

#[test]
fn authorization_digest_matches_golden_reference() {
    let env = Env::default();

    // 1. Fixed network id: hash of fixed 32-byte network seed
    env.ledger().set_network_id([42u8; 32]);

    // 2. Fixed deployed contract address
    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);

    // 3. Fixed deterministic parameters
    let payment_id = BytesN::from_array(&env, &[1u8; 32]);
    let from = Address::generate(&env);
    let to = Address::generate(&env);
    let cap: i128 = 500_000_000;
    let expiry: u32 = 100_000;

    let digest = client.get_authorization_digest(&payment_id, &from, &to, &cap, &expiry);

    // Assert digest matches the golden reference value
    let digest_bytes = digest.to_array();

    // Convert to hex string for inspection and golden pinning
    let mut hex_str = std::string::String::with_capacity(64);
    for b in digest_bytes.iter() {
        use std::fmt::Write;
        let _ = write!(hex_str, "{:02x}", b);
    }

    // Pin against golden reference digest
    const GOLDEN_HEX: &str = "888234f87a6dfff69268a492244703c94009749fbba91b9b63f7a2e04fac741f";
    const GOLDEN_BYTES: [u8; 32] = [
        0x88, 0x82, 0x34, 0xf8, 0x7a, 0x6d, 0xff, 0xf6, 0x92, 0x68, 0xa4, 0x92, 0x24, 0x47, 0x03,
        0xc9, 0x40, 0x09, 0x74, 0x9f, 0xbb, 0xa9, 0x1b, 0x9b, 0x63, 0xf7, 0xa2, 0xe0, 0x4f, 0xac,
        0x74, 0x1f,
    ];

    assert_eq!(hex_str.as_str(), GOLDEN_HEX);
    assert_eq!(digest_bytes, GOLDEN_BYTES);
}
