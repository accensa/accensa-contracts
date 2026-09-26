//! Channel splicing tests (issue #460).

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Bytes, BytesN, Env,
};

struct Setup {
    env: Env,
    client: StateChannelClient<'static>,
    contract: Address,
    token: Address,
    sender: Address,
    receiver: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);

    let token = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000);

    Setup {
        env,
        client,
        contract,
        token,
        sender,
        receiver,
    }
}

impl Setup {
    fn open(&self, amount: i128) -> u64 {
        let pk = BytesN::from_array(&self.env, &[9u8; 32]);
        self.client
            .open_channel(&self.sender, &self.receiver, &pk, &amount, &720)
    }

    fn hash(&self, data: &[u8]) -> BytesN<32> {
        self.env
            .crypto()
            .sha256(&Bytes::from_slice(&self.env, data))
            .into()
    }

    fn contract_balance(&self) -> i128 {
        TokenClient::new(&self.env, &self.token).balance(&self.contract)
    }

    fn sender_balance(&self) -> i128 {
        TokenClient::new(&self.env, &self.token).balance(&self.sender)
    }
}

#[test]
fn splice_in_raises_capacity_and_funds_it() {
    let s = setup();
    let channel_id = s.open(1_000);
    let sender_before = s.sender_balance();
    let escrow_before = s.contract_balance();

    s.client.splice_in(&channel_id, &500);

    assert_eq!(s.client.get_channel(&channel_id).amount, 1_500);
    assert_eq!(s.sender_balance(), sender_before - 500);
    assert_eq!(s.contract_balance(), escrow_before + 500);
    // Off-chain state is untouched by a splice.
    assert_eq!(s.client.get_channel(&channel_id).nonce, 0);
    assert_eq!(s.client.get_channel(&channel_id).balance, 0);
}

#[test]
fn splice_out_returns_free_funds_to_sender() {
    let s = setup();
    let channel_id = s.open(1_000);
    let sender_before = s.sender_balance();
    let escrow_before = s.contract_balance();

    s.client.splice_out(&channel_id, &400);

    assert_eq!(s.client.get_channel(&channel_id).amount, 600);
    assert_eq!(s.sender_balance(), sender_before + 400);
    assert_eq!(s.contract_balance(), escrow_before - 400);
}

#[test]
fn splice_out_cannot_touch_reserved_escrow() {
    let s = setup();
    let channel_id = s.open(1_000);
    let h = s.hash(b"lock");
    // Reserve 300 for a pending HTLC; only 700 remains free.
    s.client.add_htlc(&channel_id, &h, &300, &1_000, &None);

    assert_eq!(s.client.get_channel_free_balance(&channel_id), 700);
    assert_eq!(
        s.client.try_splice_out(&channel_id, &701),
        Err(Ok(Error::InsufficientChannelBalance))
    );
    // Exactly the free amount is allowed.
    s.client.splice_out(&channel_id, &700);
    assert_eq!(s.client.get_channel(&channel_id).amount, 300);
    assert_eq!(s.client.get_channel_free_balance(&channel_id), 0);
}

#[test]
fn splice_rejects_non_positive_amounts() {
    let s = setup();
    let channel_id = s.open(1_000);

    assert_eq!(
        s.client.try_splice_in(&channel_id, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        s.client.try_splice_in(&channel_id, &-5),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        s.client.try_splice_out(&channel_id, &0),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn unknown_channel_is_not_found() {
    let s = setup();
    assert_eq!(
        s.client.try_splice_in(&42, &100),
        Err(Ok(Error::ChannelNotFound))
    );
}
