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

/// Pins the storage keys behind `axelar.rs`'s replay guard and running
/// credited total, and their separation from the router-level `crate::DataKey`
/// namespace.
mod replay_guard_and_credited_keys {
    use super::*;
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::Event;

    fn credited_total(env: &Env, bridge: &Address, recipient: &BytesN<32>) -> Option<i128> {
        env.as_contract(bridge, || {
            env.storage()
                .persistent()
                .get(&axelar::DataKey::Credited(recipient.clone()))
        })
    }

    fn message_executed(env: &Env, bridge: &Address, message_id: &String) -> bool {
        env.as_contract(bridge, || {
            env.storage()
                .persistent()
                .has(&axelar::DataKey::MessageExecuted(message_id.clone()))
        })
    }

    #[test]
    fn replayed_message_id_is_rejected_without_a_second_credit_or_event() {
        let (env, _admin, gateway, client) = setup();
        let bridge = client.address.clone();
        let recipient = BytesN::from_array(&env, &[7u8; 32]);
        let (chain, msg, addr) = message(&env, "replay-1");
        let payload = axelar::encode_deposit_payload(&env, &recipient, 500);

        assert_eq!(
            client.axelar_execute(&gateway, &chain, &msg, &addr, &payload),
            500
        );
        assert!(message_executed(&env, &bridge, &msg));
        assert_eq!(credited_total(&env, &bridge, &recipient), Some(500));

        // The replay is refused by the message-id guard before any credit.
        assert_eq!(
            client.try_axelar_execute(&gateway, &chain, &msg, &addr, &payload),
            Err(Ok(Error::AlreadyRefunded))
        );
        // `events().all()` covers the last invocation only: the refused replay.
        assert!(
            env.events()
                .all()
                .filter_by_contract(&bridge)
                .events()
                .is_empty(),
            "a rejected replay must emit no AxelarDepositCreditedEvent"
        );
        assert_eq!(
            credited_total(&env, &bridge, &recipient),
            Some(500),
            "the credited total must not change on replay"
        );
    }

    #[test]
    fn two_distinct_messages_accumulate_instead_of_overwriting() {
        let (env, _admin, gateway, client) = setup();
        let bridge = client.address.clone();
        let recipient = BytesN::from_array(&env, &[8u8; 32]);
        let (chain, msg1, addr) = message(&env, "accumulate-1");
        let (_, msg2, _) = message(&env, "accumulate-2");

        let payload1 = axelar::encode_deposit_payload(&env, &recipient, 500);
        let payload2 = axelar::encode_deposit_payload(&env, &recipient, 250);

        assert_eq!(
            client.axelar_execute(&gateway, &chain, &msg1, &addr, &payload1),
            500
        );
        assert_eq!(
            client.axelar_execute(&gateway, &chain, &msg2, &addr, &payload2),
            750
        );

        // `events().all()` covers the last invocation only, so read it before
        // any `as_contract` storage probes.
        let events = env.events().all().filter_by_contract(&bridge);
        assert_eq!(
            events.events().len(),
            1,
            "expected exactly one credit event"
        );
        let expected = axelar::AxelarDepositCreditedEvent {
            message_id: msg2.clone(),
            recipient: recipient.clone(),
            amount: 250,
            source_chain: chain.clone(),
            credited_total: 750,
            credited_ledger: env.ledger().sequence(),
        };
        assert_eq!(&events.events()[0], &expected.to_xdr(&env, &bridge));

        assert_eq!(
            credited_total(&env, &bridge, &recipient),
            Some(750),
            "Credited must be the sum, not the latest deposit"
        );
        assert!(message_executed(&env, &bridge, &msg1));
        assert!(message_executed(&env, &bridge, &msg2));
    }

    #[test]
    fn replay_guard_keys_on_the_full_string_not_its_length() {
        let (env, _admin, gateway, client) = setup();
        let bridge = client.address.clone();
        let recipient = BytesN::from_array(&env, &[9u8; 32]);
        // Same prefix, different length: distinct `String` keys, both valid.
        let (chain, short, addr) = message(&env, "id");
        let (_, long, _) = message(&env, "id ");

        let payload1 = axelar::encode_deposit_payload(&env, &recipient, 100);
        let payload2 = axelar::encode_deposit_payload(&env, &recipient, 200);

        assert_eq!(
            client.axelar_execute(&gateway, &chain, &short, &addr, &payload1),
            100
        );
        assert_eq!(
            client.axelar_execute(&gateway, &chain, &long, &addr, &payload2),
            300
        );
        assert_eq!(credited_total(&env, &bridge, &recipient), Some(300));
        assert!(message_executed(&env, &bridge, &short));
        assert!(message_executed(&env, &bridge, &long));
    }

    #[test]
    fn credited_balance_is_isolated_from_router_level_keys() {
        let (env, _admin, gateway, client) = setup();
        let bridge = client.address.clone();
        let recipient = BytesN::from_array(&env, &[10u8; 32]);
        let (chain, msg, addr) = message(&env, "namespace-1");
        let payload = axelar::encode_deposit_payload(&env, &recipient, 500);

        client.axelar_execute(&gateway, &chain, &msg, &addr, &payload);

        env.as_contract(&bridge, || {
            // The credit lives only under the Axelar namespace...
            assert_eq!(
                env.storage()
                    .persistent()
                    .get::<_, i128>(&axelar::DataKey::Credited(recipient.clone())),
                Some(500)
            );
            // ...never under a router-level persistent key.
            assert!(!env.storage().persistent().has(&crate::DataKey::Admin));
            assert!(!env.storage().persistent().has(&crate::DataKey::Token));
            assert!(!env
                .storage()
                .persistent()
                .has(&crate::DataKey::AxelarGateway));
            // Router keys keep their own instance-storage values.
            let configured_gateway: Address = env
                .storage()
                .instance()
                .get(&crate::DataKey::AxelarGateway)
                .unwrap();
            assert_eq!(configured_gateway, gateway);
        });
    }
}
