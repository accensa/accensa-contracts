//! Boundary coverage for the reservation sum in `hashlock::add` (issue #727).

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::Address as _, token::StellarAssetClient, Address, Bytes, BytesN, Env,
};

fn setup() -> (Env, StateChannelClient<'static>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);

    let token = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&alice, &i128::MAX);

    (env, client, contract, alice, bob)
}

fn pk(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

#[test]
fn reservation_sum_stops_at_i128_max() {
    let (env, client, _contract, alice, bob) = setup();
    // Fund the escrow with the largest amount the token can carry so the
    // reservation total can be driven all the way to `i128::MAX`.
    let channel = client.open_channel(&alice, &bob, &pk(&env), &i128::MAX, &720);
    let hash_lock: BytesN<32> = env
        .crypto()
        .sha256(&Bytes::from_slice(&env, b"boundary"))
        .into();

    client.add_hashlock_payment(&channel, &hash_lock, &(i128::MAX - 1));
    assert_eq!(client.get_hashlock_reserved(&channel), i128::MAX - 1);

    // Exactly `i128::MAX`: the largest representable reservation is accepted
    // and the sum does not wrap or trap.
    client.add_hashlock_payment(&channel, &hash_lock, &1);
    assert_eq!(client.get_hashlock_reserved(&channel), i128::MAX);
}

#[test]
fn overflowing_reservation_sum_returns_typed_error() {
    let (env, client, contract, alice, bob) = setup();
    let channel = client.open_channel(&alice, &bob, &pk(&env), &i128::MAX, &720);

    // No reachable channel has a negative balance, so the escrow ceiling
    // normally keeps the reservation sum in range. Force the update's failure
    // arm to be reachable to prove it returns a typed error instead of
    // aborting the host call on the release profile's overflow trap.
    env.as_contract(&contract, || {
        let channel_key = DataKey::Channel(channel);
        let mut stored: Channel = env.storage().instance().get(&channel_key).unwrap();
        stored.balance = -1;
        env.storage().instance().set(&channel_key, &stored);
        env.storage()
            .persistent()
            .set(&DataKey::HashlockReserved(channel), &i128::MAX);
    });
    assert_eq!(client.get_hashlock_reserved(&channel), i128::MAX);

    // `-1 + i128::MAX + 1 == i128::MAX` passes the escrow ceiling, but the
    // reservation update itself would need `i128::MAX + 1`.
    let hash_lock: BytesN<32> = env
        .crypto()
        .sha256(&Bytes::from_slice(&env, b"overflow"))
        .into();
    assert_eq!(
        client.try_add_hashlock_payment(&channel, &hash_lock, &1),
        Err(Ok(Error::MathOverflow))
    );
    assert_eq!(client.get_hashlock_reserved(&channel), i128::MAX);
}
