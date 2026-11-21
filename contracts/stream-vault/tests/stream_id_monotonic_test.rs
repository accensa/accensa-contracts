//! Integration coverage for stream id monotonicity across cancellation (issue #1001).

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env,
};
use stream_vault::{Error, StreamVault, StreamVaultClient};

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
fn stream_ids_are_never_reused_after_cancel() {
    let (env, client, buyer) = setup();

    let id0 = client.create_stream(&buyer, &100, &200, &10, &1000);
    let id1 = client.create_stream(&buyer, &100, &200, &10, &1000);
    assert_eq!(id0, 0);
    assert_eq!(id1, 1);

    client.cancel_stream(&id0);
    assert_eq!(client.get_stream(&id0), None);

    let id2 = client.create_stream(&buyer, &100, &200, &10, &1000);
    assert_eq!(id2, 2);

    assert_eq!(client.get_stream(&id0), None);
    assert_eq!(
        client.try_claim_stream(&id0),
        Err(Ok(Error::StreamNotFound))
    );
    assert_eq!(
        client.try_cancel_stream(&id0),
        Err(Ok(Error::StreamNotFound))
    );

    // Streams 1 and 2 maintain independent accounting.
    env.ledger().set_sequence_number(150);
    assert_eq!(client.get_stream_claimable(&id1), 500);
    assert_eq!(client.get_stream_claimable(&id2), 500);
    assert_eq!(client.claim_stream(&id1), 500);
    assert_eq!(client.get_stream_claimable(&id1), 0);
    assert_eq!(client.get_stream_claimable(&id2), 500);
}
