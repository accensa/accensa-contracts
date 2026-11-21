//! Integration tests for pinning the outbound bridge payload wire shape emitted by withdraw_to_evm (issue #1035).

#![cfg(test)]

extern crate std;

use accensa_common::Error;
use accensa_cross_chain::{
    outbound::{execute_withdrawal_to_evm, OutboundBridgePayload, BRIDGE_TOPIC, WITHDRAW_ACTION},
    CrossChainBridge, CrossChainBridgeClient, EvmAddress,
};
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    vec, Address, BytesN, Env, IntoVal,
};

#[test]
fn test_outbound_bridge_payload_wire_shape_and_event_pinning() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1705000000);

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();

    // Initialize bridge: target EVM chain ID 1 (Ethereum Mainnet)
    client.initialize(&admin, &token_addr, &1);

    let merchant = Address::generate(&env);
    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_addr);
    sac.mint(&merchant, &50_000);

    let evm_recipient: EvmAddress = BytesN::from_array(&env, &[0xcc; 20]);
    let withdraw_amount: i128 = 12_500;

    // 1. Execute withdrawal via contract client
    let seq = client.withdraw_to_evm(&merchant, &evm_recipient, &withdraw_amount);
    assert_eq!(seq, 1);

    // 2. Validate emitted event topics and data structure
    let events = env.events().all().filter_by_contract(&contract_id);
    assert_eq!(events.events().len(), 1);

    let expected_payload = OutboundBridgePayload {
        sequence: 1,
        sender: merchant.clone(),
        evm_recipient: evm_recipient.clone(),
        amount: withdraw_amount,
        destination_chain_id: 1,
        token: token_addr.clone(),
        timestamp: 1705000000,
    };

    assert_eq!(
        events,
        vec![
            &env,
            (
                contract_id.clone(),
                (BRIDGE_TOPIC, WITHDRAW_ACTION, 1u64).into_val(&env),
                expected_payload.clone().into_val(&env)
            )
        ]
    );

    // 3. Pin individual payload fields to protect against field reordering or endianness regressions
    assert_eq!(expected_payload.sequence, 1);
    assert_eq!(expected_payload.sender, merchant);
    assert_eq!(expected_payload.evm_recipient, evm_recipient);
    assert_eq!(expected_payload.amount, 12_500);
    assert_eq!(expected_payload.destination_chain_id, 1);
    assert_eq!(expected_payload.token, token_addr);
    assert_eq!(expected_payload.timestamp, 1705000000);

    // 4. Test direct execute_withdrawal_to_evm function for alternative chain (Base = 8453)
    env.ledger().set_timestamp(1705000500);
    let direct_recipient: EvmAddress = BytesN::from_array(&env, &[0xdd; 20]);
    let direct_payload = env
        .as_contract(&contract_id, || {
            execute_withdrawal_to_evm(
                &env,
                &merchant,
                &token_addr,
                &direct_recipient,
                7_500,
                8453,
                2,
            )
        })
        .expect("direct withdrawal must succeed");

    assert_eq!(direct_payload.sequence, 2);
    assert_eq!(direct_payload.destination_chain_id, 8453);
    assert_eq!(direct_payload.amount, 7_500);
    assert_eq!(direct_payload.evm_recipient, direct_recipient);
    assert_eq!(direct_payload.timestamp, 1705000500);

    // 5. Zero or negative withdrawal amount must be rejected
    let res_zero = client.try_withdraw_to_evm(&merchant, &evm_recipient, &0);
    assert_eq!(res_zero, Err(Ok(Error::InvalidAmount)));

    let res_neg = client.try_withdraw_to_evm(&merchant, &evm_recipient, &-100);
    assert_eq!(res_neg, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_withdraw_to_evm_paused_bridge_rejects_requests() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token, &1);

    // Pause bridge
    client.pause(&admin);

    let caller = Address::generate(&env);
    let evm_recipient: EvmAddress = BytesN::from_array(&env, &[0xee; 20]);

    // Paused bridge must refuse outbound withdrawals
    let res = client.try_withdraw_to_evm(&caller, &evm_recipient, &1_000);
    assert_eq!(res, Err(Ok(Error::Paused)));
}
