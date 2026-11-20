//! Integration coverage for `StreamVault::get_stream_count` (issue #999).

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env,
};
use stream_vault::{StreamVault, StreamVaultClient};

fn setup() -> (Env, StreamVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(50);

    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(token_admin);
    token::StellarAssetClient::new(&env, &token.address()).mint(&buyer, &10_000);

    let contract_id = env.register(StreamVault, (merchant, token.address()));
    let client = StreamVaultClient::new(&env, &contract_id);
    (env, client, buyer)
}

#[test]
fn get_stream_count_tracks_creations_and_ignores_cancels() {
    let (_env, client, buyer) = setup();
    assert_eq!(client.get_stream_count(), 0);

    let id0 = client.create_stream(&buyer, &100, &200, &10, &1000);
    assert_eq!(id0, 0);
    assert_eq!(client.get_stream_count(), 1);

    let id1 = client.create_stream(&buyer, &100, &200, &10, &1000);
    assert_eq!(id1, 1);
    assert_eq!(client.get_stream_count(), 2);

    client.cancel_stream(&id0);
    assert_eq!(client.get_stream_count(), 2);

    let id2 = client.create_stream(&buyer, &100, &200, &10, &1000);
    assert_eq!(id2, 2);
    assert_eq!(client.get_stream_count(), 3);
}
