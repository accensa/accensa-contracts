use accensa_common::Error;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};
use state_channel::{StateChannel, StateChannelClient};

struct Fixture {
    env: Env,
    client: StateChannelClient<'static>,
    sender: Address,
    receiver: Address,
    token: Address,
    sender_pubkey: BytesN<32>,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);
    let sender_pubkey = BytesN::from_array(&env, &[7u8; 32]);

    let sac = env.register_stellar_asset_contract_v2(admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000);

    let contract_id = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract_id);
    client.initialize(&token);

    Fixture {
        env,
        client,
        sender,
        receiver,
        token,
        sender_pubkey,
    }
}

#[test]
fn open_channel_rejects_the_contract_as_receiver() {
    let fx = setup();

    assert_eq!(
        fx.client.try_open_channel(
            &fx.sender,
            &fx.client.address,
            &fx.sender_pubkey,
            &1_000,
            &100
        ),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(fx.client.get_channel_count(), 0);
    assert_eq!(
        TokenClient::new(&fx.env, &fx.token).balance(&fx.client.address),
        0
    );
    assert_eq!(
        TokenClient::new(&fx.env, &fx.token).balance(&fx.sender),
        1_000_000
    );
}

#[test]
fn open_channel_rejects_the_sender_as_receiver() {
    let fx = setup();

    assert_eq!(
        fx.client
            .try_open_channel(&fx.sender, &fx.sender, &fx.sender_pubkey, &1_000, &100),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(fx.client.get_channel_count(), 0);
    assert_eq!(
        TokenClient::new(&fx.env, &fx.token).balance(&fx.client.address),
        0
    );
    assert_eq!(
        TokenClient::new(&fx.env, &fx.token).balance(&fx.sender),
        1_000_000
    );
}

#[test]
fn open_channel_accepts_distinct_sender_and_receiver() {
    let fx = setup();

    let channel_id =
        fx.client
            .open_channel(&fx.sender, &fx.receiver, &fx.sender_pubkey, &1_000, &100);

    assert_eq!(channel_id, 1);
    assert_eq!(fx.client.get_channel_count(), 1);
    let channel = fx.client.get_channel(&channel_id);
    assert_eq!(channel.sender, fx.sender);
    assert_eq!(channel.receiver, fx.receiver);
    assert_eq!(channel.amount, 1_000);
    assert_eq!(
        TokenClient::new(&fx.env, &fx.token).balance(&fx.client.address),
        1_000
    );
    assert_eq!(
        TokenClient::new(&fx.env, &fx.token).balance(&fx.sender),
        999_000
    );
}
