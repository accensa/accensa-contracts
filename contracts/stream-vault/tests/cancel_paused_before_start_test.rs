use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};
use stream_vault::{StreamVault, StreamVaultClient};

const NOW: u32 = 50;
const START: u32 = NOW + 50;
const STOP: u32 = NOW + 150;
const DEPOSIT: i128 = 1_000;

#[test]
fn cancel_after_pausing_before_start_refunds_the_full_deposit() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(NOW);

    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(token_admin);
    let token_client = TokenClient::new(&env, &token.address());
    StellarAssetClient::new(&env, &token.address()).mint(&buyer, &DEPOSIT);

    let contract_id = env.register(StreamVault, (merchant.clone(), token.address()));
    let client = StreamVaultClient::new(&env, &contract_id);
    let stream_id = client.create_stream(&buyer, &START, &STOP, &10, &DEPOSIT);

    env.ledger().set_sequence_number(NOW + 10);
    client.pause_stream(&stream_id);
    assert_eq!(
        client.get_stream(&stream_id).unwrap().paused_at,
        Some(NOW + 10)
    );

    env.ledger().set_sequence_number(NOW + 80);
    assert_eq!(client.cancel_stream(&stream_id), (0, DEPOSIT));

    assert_eq!(token_client.balance(&buyer), DEPOSIT);
    assert_eq!(token_client.balance(&merchant), 0);
    assert_eq!(token_client.balance(&contract_id), 0);
    assert_eq!(client.get_stream(&stream_id), None);
}
