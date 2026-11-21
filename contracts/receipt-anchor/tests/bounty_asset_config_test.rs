//! #998 — the router arms every shard it deploys with one bounty asset.
//!
//! The pruning-bounty asset is router configuration, set by the merchant and
//! threaded into the `ReceiptShard` constructor at deploy time. A router with
//! no configured asset refuses to deploy rather than creating shards whose
//! bounty claims would all fail closed.

#![cfg(test)]

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use receipt_shard::ReceiptShardClient;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    vec, Address, BytesN, Env, IntoVal, Map, Symbol, Val,
};

const DEFAULT_SHARD: u64 = 0;

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn setup() -> (Env, ReceiptAnchorClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &id);
    let merchant = Address::generate(&env);
    let wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    client.initialize(&merchant, &wasm_hash);
    (env, client, merchant)
}

#[test]
#[should_panic]
fn set_bounty_asset_requires_admin_auth() {
    let (env, client, _merchant) = setup();

    // Enforcing mode with no signatures: the admin's `require_auth` aborts.
    env.set_auths(&[]);
    client.set_bounty_asset(&Address::generate(&env));
}

#[test]
fn set_bounty_asset_emits_event_and_overwrites() {
    let (env, client, _merchant) = setup();
    let first = Address::generate(&env);

    env.ledger().with_mut(|li| li.sequence_number = 900);
    client.set_bounty_asset(&first);

    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "ledger").into_val(&env),
        900u32.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "new_asset").into_val(&env),
        first.clone().into_val(&env),
    );
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (
                    Symbol::new(&env, "bounty_asset_updated_event"),
                    Option::<Address>::None,
                )
                    .into_val(&env),
                data.into_val(&env)
            )
        ]
    );
    assert_eq!(client.get_bounty_asset(), first);

    // Reconfiguration overwrites the previous value; the topic records the
    // asset it replaced.
    let second = Address::generate(&env);
    client.set_bounty_asset(&second);

    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "ledger").into_val(&env),
        900u32.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "new_asset").into_val(&env),
        second.clone().into_val(&env),
    );
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "bounty_asset_updated_event"), first).into_val(&env),
                data.into_val(&env)
            )
        ]
    );
    assert_eq!(client.get_bounty_asset(), second);
}

#[test]
fn router_without_bounty_asset_refuses_to_deploy_a_shard() {
    let (env, client, _merchant) = setup();
    let root = BytesN::from_array(&env, &[1u8; 32]);

    assert_eq!(
        client.try_anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &1),
        Err(Ok(Error::NotInitialized)),
        "a router with no bounty asset must refuse to deploy"
    );
    assert!(
        client.try_get_shard_address(&DEFAULT_SHARD, &0).is_err(),
        "the refused deploy must not record a shard address"
    );
}

#[test]
fn deployed_shard_receives_the_configured_asset() {
    let (env, client, _merchant) = setup();
    let token = Address::generate(&env);
    client.set_bounty_asset(&token);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &1);

    let shard_addr = client.get_shard_address(&DEFAULT_SHARD, &0);
    let shard = ReceiptShardClient::new(&env, &shard_addr);
    assert_eq!(shard.get_bounty_asset(), token);
}
