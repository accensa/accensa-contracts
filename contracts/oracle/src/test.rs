#![cfg(test)]

use crate::{chainlink::RoundData, ChainlinkConsumer, ChainlinkConsumerClient, Error};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env,
};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.sequence_number = 1000);
    let admin = Address::generate(&env);
    let id = env.register(ChainlinkConsumer, ());
    ChainlinkConsumerClient::new(&env, &id).initialize(&admin);
    (env, id, admin)
}

fn round(round_id: u64, answer: i128, updated_at: u32) -> RoundData {
    RoundData {
        round_id,
        answer,
        started_at: updated_at,
        updated_at,
        answered_in_round: round_id,
    }
}

#[test]
fn fresh_price_accepted() {
    let (env, id, _) = setup();
    let c = ChainlinkConsumerClient::new(&env, &id);
    c.set_round_data(&round(1, 100_000_000, env.ledger().sequence()));
    assert_eq!(c.latest_answer(&3_600), 100_000_000);
    assert_eq!(
        c.get_price(&BytesN::from_array(&env, &[1; 32])),
        100_000_000
    );
}

#[test]
fn stale_price_rejected() {
    let (env, id, _) = setup();
    let c = ChainlinkConsumerClient::new(&env, &id);
    c.set_round_data(&round(1, 100, env.ledger().sequence()));
    env.ledger().with_mut(|l| l.sequence_number += 10_000);
    assert_eq!(c.try_latest_answer(&100), Err(Ok(Error::StalePrice)));
}

#[test]
fn incomplete_round_rejected() {
    let (env, id, _) = setup();
    let c = ChainlinkConsumerClient::new(&env, &id);
    // answered_in_round < round_id
    let bad = RoundData {
        round_id: 2,
        answer: 50,
        started_at: 1,
        updated_at: 1,
        answered_in_round: 1,
    };
    assert_eq!(c.try_set_round_data(&bad), Err(Ok(Error::IncompleteRound)));
    // zero updated_at
    let bad = RoundData {
        round_id: 1,
        answer: 50,
        started_at: 0,
        updated_at: 0,
        answered_in_round: 1,
    };
    assert_eq!(c.try_set_round_data(&bad), Err(Ok(Error::IncompleteRound)));
    // non-positive answer
    assert_eq!(
        c.try_set_round_data(&round(1, 0, 1)),
        Err(Ok(Error::IncompleteRound))
    );
    assert_eq!(
        c.try_set_round_data(&round(1, -5, 1)),
        Err(Ok(Error::IncompleteRound))
    );
}

#[test]
fn empty_feed_fails_closed() {
    let (env, id, _) = setup();
    let c = ChainlinkConsumerClient::new(&env, &id);
    assert_eq!(c.try_latest_answer(&0), Err(Ok(Error::NoData)));
    assert_eq!(
        c.try_get_price(&BytesN::from_array(&env, &[0; 32])),
        Err(Ok(Error::NoData))
    );
    assert_eq!(
        c.get_last_update_ledger(&BytesN::from_array(&env, &[0; 32])),
        0
    );
}

#[test]
fn monotonic_round_ids_enforced() {
    let (env, id, _) = setup();
    let c = ChainlinkConsumerClient::new(&env, &id);
    c.set_round_data(&round(2, 10, env.ledger().sequence()));
    assert_eq!(
        c.try_set_round_data(&round(2, 11, env.ledger().sequence())),
        Err(Ok(Error::StaleRoundId))
    );
    assert_eq!(
        c.try_set_round_data(&round(1, 11, env.ledger().sequence())),
        Err(Ok(Error::StaleRoundId))
    );
}
