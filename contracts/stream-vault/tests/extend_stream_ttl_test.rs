use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token, Address, Env,
};
use stream_vault::{DataKey, Error, StreamVault, StreamVaultClient};

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

fn open_stream(client: &StreamVaultClient<'_>, buyer: &Address) -> u64 {
    client.create_stream(buyer, &100, &200, &10, &1_000)
}

fn stream_ttl(env: &Env, client: &StreamVaultClient<'_>, stream_id: u64) -> u32 {
    env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Stream(stream_id))
    })
}

#[test]
fn extend_stream_ttl_is_permissionless_and_extends_the_record() {
    let (env, client, buyer) = setup();
    let stream_id = open_stream(&client, &buyer);
    let stream = client.get_stream(&stream_id);

    let fresh_ttl = stream_ttl(&env, &client, stream_id);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + fresh_ttl - 50);
    let aged_ttl = stream_ttl(&env, &client, stream_id);
    assert!(aged_ttl <= 50);

    env.set_auths(&[]);
    client.extend_stream_ttl(&stream_id);

    assert!(env.auths().is_empty());
    assert!(stream_ttl(&env, &client, stream_id) > aged_ttl);
    assert_eq!(client.get_stream(&stream_id), stream);
}

#[test]
fn extend_stream_ttl_rejects_unknown_and_closed_streams() {
    let (_env, client, buyer) = setup();
    let stream_id = open_stream(&client, &buyer);

    assert_eq!(
        client.try_extend_stream_ttl(&(stream_id + 1)),
        Err(Ok(Error::StreamNotFound))
    );

    client.cancel_stream(&stream_id);
    assert_eq!(
        client.try_extend_stream_ttl(&stream_id),
        Err(Ok(Error::StreamNotFound))
    );
}
