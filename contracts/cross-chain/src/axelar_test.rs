#![cfg(test)]

//! Axelar cross-chain deposits (issue #454).

use super::*;
use accensa_common::Error;
use soroban_sdk::{
    contract, contractimpl, testutils::Address as _, Address, Bytes, BytesN, Env, String,
};

#[contract]
pub struct MockAxelarGateway;

#[contractimpl]
impl MockAxelarGateway {
    pub fn validate_message(
        _env: Env,
        _source_chain: String,
        _message_id: String,
        _source_address: String,
        _payload_hash: BytesN<32>,
    ) -> bool {
        true
    }
}

#[contract]
pub struct RejectingAxelarGateway;

#[contractimpl]
impl RejectingAxelarGateway {
    pub fn validate_message(
        _env: Env,
        _source_chain: String,
        _message_id: String,
        _source_address: String,
        _payload_hash: BytesN<32>,
    ) -> bool {
        false
    }
}

fn setup() -> (Env, Address, Address, CrossChainBridgeClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let bridge_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &bridge_id);
    client.initialize(&admin, &token, &1u32);

    let gateway_id = env.register(MockAxelarGateway, ());
    client.set_axelar_gateway(&admin, &gateway_id);

    (env, admin, gateway_id, client)
}

fn message(env: &Env, id: &str) -> (String, String, String) {
    (
        String::from_str(env, "ethereum"),
        String::from_str(env, id),
        String::from_str(env, "0xabc"),
    )
}

#[test]
fn valid_deposit_is_credited_and_accumulates() {
    let (env, _admin, gateway, client) = setup();
    let recipient = BytesN::from_array(&env, &[3u8; 32]);
    let (chain, msg1, addr) = message(&env, "msg-1");

    let payload = axelar::encode_deposit_payload(&env, &recipient, 500);
    let total = client.axelar_execute(&gateway, &chain, &msg1, &addr, &payload);
    assert_eq!(total, 500);

    let (_, msg2, _) = message(&env, "msg-2");
    let payload2 = axelar::encode_deposit_payload(&env, &recipient, 250);
    let total2 = client.axelar_execute(&gateway, &chain, &msg2, &addr, &payload2);
    assert_eq!(total2, 750);
}

#[test]
fn replayed_message_is_rejected() {
    let (env, _admin, gateway, client) = setup();
    let recipient = BytesN::from_array(&env, &[3u8; 32]);
    let (chain, msg1, addr) = message(&env, "msg-1");
    let payload = axelar::encode_deposit_payload(&env, &recipient, 500);

    client.axelar_execute(&gateway, &chain, &msg1, &addr, &payload);
    assert_eq!(
        client.try_axelar_execute(&gateway, &chain, &msg1, &addr, &payload),
        Err(Ok(Error::AlreadyRefunded))
    );
}

#[test]
fn untrusted_gateway_is_rejected() {
    let (env, _admin, _gateway, client) = setup();
    let rogue = Address::generate(&env);
    let (chain, msg1, addr) = message(&env, "msg-1");
    let payload = axelar::encode_deposit_payload(&env, &BytesN::from_array(&env, &[1u8; 32]), 5);

    assert_eq!(
        client.try_axelar_execute(&rogue, &chain, &msg1, &addr, &payload),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn malformed_payload_is_rejected() {
    let (env, _admin, gateway, client) = setup();
    let (chain, msg1, addr) = message(&env, "msg-1");
    let payload = Bytes::from_slice(&env, &[0u8, 1, 2, 3]);

    assert_eq!(
        client.try_axelar_execute(&gateway, &chain, &msg1, &addr, &payload),
        Err(Ok(Error::InvalidProof))
    );
}

#[test]
fn rejecting_gateway_validation_is_rejected() {
    let (env, admin, _gateway, client) = setup();
    let rejecting = env.register(RejectingAxelarGateway, ());
    client.set_axelar_gateway(&admin, &rejecting);
    let (chain, msg1, addr) = message(&env, "msg-1");
    let payload = axelar::encode_deposit_payload(&env, &BytesN::from_array(&env, &[1u8; 32]), 5);

    assert_eq!(
        client.try_axelar_execute(&rejecting, &chain, &msg1, &addr, &payload),
        Err(Ok(Error::InvalidProof))
    );
}
