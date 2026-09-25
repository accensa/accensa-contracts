#![cfg(test)]

extern crate std;

use accensa_common::Error;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    vec, Address, BytesN, Env, IntoVal,
};

use crate::{
    outbound::{OutboundBridgePayload, BRIDGE_TOPIC, WITHDRAW_ACTION},
    CrossChainBridge, CrossChainBridgeClient,
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
    assert_eq!(client.is_paused(), true);

    // Attempt withdrawal while paused
    assert_eq!(
        client.try_withdraw_to_evm(&merchant, &evm_recipient, &500),
        Err(Ok(Error::Paused))
    );

    // Unpause contract
    client.unpause(&admin);
    assert_eq!(client.is_paused(), false);
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
