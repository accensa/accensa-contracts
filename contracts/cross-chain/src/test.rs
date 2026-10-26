#![cfg(test)]

extern crate std;

use accensa_common::Error;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    vec, Address, BytesN, Env, IntoVal,
};

use crate::{
    outbound::{OutboundBridgePayload, BRIDGE_TOPIC, WITHDRAW_ACTION},
    CrossChainBridge, CrossChainBridgeClient, GuardianSet, PeerAddress,
};

#[test]
fn test_withdraw_to_evm_burns_asset_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1700000000);

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_addr = token_id.address();

    // Initialize bridge: chain ID 1 (Ethereum Mainnet)
    client.initialize(&admin, &token_addr, &1);

    let merchant = Address::generate(&env);
    let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token_addr);
    token_client.mint(&merchant, &10_000);

    let evm_recipient: BytesN<20> = BytesN::from_array(&env, &[0xab; 20]);

    // Withdraw 4,000 to EVM
    let seq = client.withdraw_to_evm(&merchant, &evm_recipient, &4_000);
    assert_eq!(seq, 1);

    // Verify bridging event emission immediately after call
    let events = env.events().all().filter_by_contract(&contract_id);
    assert_eq!(events.events().len(), 1);

    let expected_payload = OutboundBridgePayload {
        sequence: 1,
        sender: merchant.clone(),
        evm_recipient: evm_recipient.clone(),
        amount: 4_000,
        destination_chain_id: 1,
        token: token_addr.clone(),
        timestamp: 1700000000,
    };

    assert_eq!(
        events,
        vec![
            &env,
            (
                contract_id.clone(),
                (BRIDGE_TOPIC, WITHDRAW_ACTION, 1u64).into_val(&env),
                expected_payload.into_val(&env)
            )
        ]
    );

    // Verify token balance of merchant was reduced by exact burned amount
    let standard_token = soroban_sdk::token::Client::new(&env, &token_addr);
    assert_eq!(standard_token.balance(&merchant), 6_000);

    // Verify next sequence incremented to 2
    assert_eq!(client.get_next_sequence(), 2);

    // Second withdrawal
    env.ledger().set_timestamp(1700000100);
    let seq2 = client.withdraw_to_evm(&merchant, &evm_recipient, &2_500);
    assert_eq!(seq2, 2);
    let events2 = env.events().all().filter_by_contract(&contract_id);
    assert_eq!(events2.events().len(), 1);

    assert_eq!(standard_token.balance(&merchant), 3_500);
    assert_eq!(client.get_next_sequence(), 3);
    let expected_payload2 = OutboundBridgePayload {
        sequence: 2,
        sender: merchant,
        evm_recipient,
        amount: 2_500,
        destination_chain_id: 1,
        token: token_addr,
        timestamp: 1700000100,
    };
    assert_eq!(
        events2,
        vec![
            &env,
            (
                contract_id,
                (BRIDGE_TOPIC, WITHDRAW_ACTION, 2u64).into_val(&env),
                expected_payload2.into_val(&env)
            )
        ]
    );
}

#[test]
fn test_withdraw_invalid_amount_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();

    client.initialize(&admin, &token_addr, &137);

    let merchant = Address::generate(&env);
    let evm_recipient: BytesN<20> = BytesN::from_array(&env, &[0xcd; 20]);

    // Zero amount
    assert_eq!(
        client.try_withdraw_to_evm(&merchant, &evm_recipient, &0),
        Err(Ok(Error::InvalidAmount))
    );

    // Negative amount
    assert_eq!(
        client.try_withdraw_to_evm(&merchant, &evm_recipient, &-100),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_withdraw_when_paused_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();

    client.initialize(&admin, &token_addr, &8453);

    let merchant = Address::generate(&env);
    let evm_recipient: BytesN<20> = BytesN::from_array(&env, &[0xef; 20]);

    // Pause contract
    client.pause(&admin);
    assert!(client.is_paused());

    // Attempt withdrawal while paused
    assert_eq!(
        client.try_withdraw_to_evm(&merchant, &evm_recipient, &500),
        Err(Ok(Error::Paused))
    );

    // Unpause contract
    client.unpause(&admin);
    assert!(!client.is_paused());
}

#[test]
#[should_panic]
fn test_withdraw_insufficient_balance_panics_on_burn() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();

    client.initialize(&admin, &token_addr, &1);

    let merchant = Address::generate(&env);
    // Merchant has 0 tokens, trying to burn 1000 will panic in token client
    let evm_recipient: BytesN<20> = BytesN::from_array(&env, &[0x11; 20]);
    let _ = client.withdraw_to_evm(&merchant, &evm_recipient, &1_000);
}

fn setup_bridge() -> (Env, CrossChainBridgeClient<'static>, Address) {
    let env = Env::default();
    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();
    client.initialize(&admin, &token_addr, &1);

    (env, client, admin)
}

fn sample_guardian_set(env: &Env) -> GuardianSet {
    GuardianSet {
        index: 1,
        keys: vec![
            env,
            BytesN::from_array(env, &[0x11; 20]),
            BytesN::from_array(env, &[0x22; 20]),
            BytesN::from_array(env, &[0x33; 20]),
        ],
    }
}

#[test]
fn test_set_guardian_set_rejects_non_admin() {
    // No `mock_all_auths`: the non-admin caller cannot satisfy `require_auth`,
    // so the call must fail (host auth error) before any storage write.
    let (env, client, _admin) = setup_bridge();
    let attacker = Address::generate(&env);

    assert!(client
        .try_set_guardian_set(&attacker, &sample_guardian_set(&env))
        .is_err());
    // Nothing was written.
    assert_eq!(client.get_guardian_set(&1), None);
    assert_eq!(client.get_current_guardian_set_index(), 0);
}

#[test]
fn test_set_guardian_set_admin_writes_expected_key() {
    let (env, client, admin) = setup_bridge();
    env.mock_all_auths();

    let set = sample_guardian_set(&env);
    client.set_guardian_set(&admin, &set);
    assert_eq!(client.get_guardian_set(&1), Some(set));
    assert_eq!(client.get_current_guardian_set_index(), 1);
}

#[test]
fn test_set_axelar_gateway_rejects_non_admin() {
    // No `mock_all_auths`: the non-admin caller cannot satisfy `require_auth`,
    // so the call must fail (host auth error) before any storage write.
    let (env, client, _admin) = setup_bridge();
    let attacker = Address::generate(&env);
    let gateway = Address::generate(&env);

    assert!(client.try_set_axelar_gateway(&attacker, &gateway).is_err());
    // Nothing was written.
    assert_eq!(client.get_axelar_gateway(), None);
}

#[test]
fn test_set_axelar_gateway_admin_writes_expected_key() {
    let (env, client, admin) = setup_bridge();
    env.mock_all_auths();

    let gateway = Address::generate(&env);
    client.set_axelar_gateway(&admin, &gateway);
    assert_eq!(client.get_axelar_gateway(), Some(gateway));
}

#[test]
fn test_set_trusted_peer_rejects_non_admin() {
    // No `mock_all_auths`: the non-admin caller cannot satisfy `require_auth`,
    // so the call must fail (host auth error) before any storage write.
    let (env, client, _admin) = setup_bridge();
    let attacker = Address::generate(&env);
    let peer = PeerAddress {
        chain_id: 101,
        sender: BytesN::from_array(&env, &[0xab; 32]),
    };

    assert!(client.try_set_trusted_peer(&attacker, &peer).is_err());
    // Nothing was written.
    assert!(!client.is_trusted_peer(&101, &BytesN::from_array(&env, &[0xab; 32])));
}

#[test]
fn test_set_trusted_peer_admin_writes_expected_key() {
    let (env, client, admin) = setup_bridge();
    env.mock_all_auths();

    let sender = BytesN::from_array(&env, &[0xab; 32]);
    client.set_trusted_peer(
        &admin,
        &PeerAddress {
            chain_id: 101,
            sender: sender.clone(),
        },
    );
    assert!(client.is_trusted_peer(&101, &sender));
}

#[test]
fn test_set_layerzero_endpoint_rejects_non_admin() {
    // No `mock_all_auths`: the non-admin caller cannot satisfy `require_auth`,
    // so the call must fail (host auth error) before any storage write.
    let (env, client, _admin) = setup_bridge();
    let attacker = Address::generate(&env);
    let endpoint = Address::generate(&env);

    assert!(client
        .try_set_layerzero_endpoint(&attacker, &endpoint)
        .is_err());
    // Nothing was written: endpoint still unconfigured.
    assert_eq!(
        client.try_get_layerzero_endpoint(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_set_layerzero_endpoint_admin_writes_expected_key() {
    let (env, client, admin) = setup_bridge();
    env.mock_all_auths();

    let endpoint = Address::generate(&env);
    client.set_layerzero_endpoint(&admin, &endpoint);
    assert_eq!(client.get_layerzero_endpoint(), endpoint);
}
