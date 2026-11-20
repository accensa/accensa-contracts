use multisig_account::{
    limits::{SpendingLimit, WINDOW_SECONDS},
    DataKey, MultisigAccount, MultisigAccountClient,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Env,
};

#[test]
fn remaining_today_full_limit_when_unused() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let signer = Address::generate(&env);
    let token = Address::generate(&env);

    let id = env.register(MultisigAccount, (vec![&env, signer.clone()], 1u32));
    let client = MultisigAccountClient::new(&env, &id);

    client.set_daily_limit(&token, &5_000);
    assert_eq!(client.remaining_today(&signer, &token), 5_000);
}

#[test]
fn remaining_today_unconfigured_token_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let signer = Address::generate(&env);
    let unconfigured_token = Address::generate(&env);

    let id = env.register(MultisigAccount, (vec![&env, signer.clone()], 1u32));
    let client = MultisigAccountClient::new(&env, &id);

    assert_eq!(client.remaining_today(&signer, &unconfigured_token), 0);
}

#[test]
fn remaining_today_partially_spent_returns_difference() {
    let env = Env::default();
    env.mock_all_auths();
    let current_time = 100_000 * WINDOW_SECONDS + 500;
    env.ledger().set_timestamp(current_time);

    let signer = Address::generate(&env);
    let token = Address::generate(&env);

    let id = env.register(MultisigAccount, (vec![&env, signer.clone()], 1u32));
    let client = MultisigAccountClient::new(&env, &id);

    client.set_daily_limit(&token, &10_000);

    let window_start = current_time - current_time % WINDOW_SECONDS;
    env.as_contract(&id, || {
        env.storage().instance().set(
            &DataKey::Spending(signer.clone(), token.clone()),
            &SpendingLimit {
                window_start,
                spent: 3_500,
            },
        );
    });

    assert_eq!(client.remaining_today(&signer, &token), 6_500);
}

#[test]
fn remaining_today_exhausted_returns_zero_without_underflow() {
    let env = Env::default();
    env.mock_all_auths();
    let current_time = 100_000 * WINDOW_SECONDS + 500;
    env.ledger().set_timestamp(current_time);

    let signer = Address::generate(&env);
    let token = Address::generate(&env);

    let id = env.register(MultisigAccount, (vec![&env, signer.clone()], 1u32));
    let client = MultisigAccountClient::new(&env, &id);

    client.set_daily_limit(&token, &5_000);

    let window_start = current_time - current_time % WINDOW_SECONDS;

    // Exact exhaustion
    env.as_contract(&id, || {
        env.storage().instance().set(
            &DataKey::Spending(signer.clone(), token.clone()),
            &SpendingLimit {
                window_start,
                spent: 5_000,
            },
        );
    });
    assert_eq!(client.remaining_today(&signer, &token), 0);

    // Over-exhaustion (e.g. edge-case state)
    env.as_contract(&id, || {
        env.storage().instance().set(
            &DataKey::Spending(signer.clone(), token.clone()),
            &SpendingLimit {
                window_start,
                spent: 6_000,
            },
        );
    });
    assert_eq!(client.remaining_today(&signer, &token), 0);
}

#[test]
fn remaining_today_resets_after_window_elapsed() {
    let env = Env::default();
    env.mock_all_auths();
    let current_time = 100_000 * WINDOW_SECONDS + 500;
    env.ledger().set_timestamp(current_time);

    let signer = Address::generate(&env);
    let token = Address::generate(&env);

    let id = env.register(MultisigAccount, (vec![&env, signer.clone()], 1u32));
    let client = MultisigAccountClient::new(&env, &id);

    client.set_daily_limit(&token, &10_000);

    let window_start = current_time - current_time % WINDOW_SECONDS;
    env.as_contract(&id, || {
        env.storage().instance().set(
            &DataKey::Spending(signer.clone(), token.clone()),
            &SpendingLimit {
                window_start,
                spent: 8_000,
            },
        );
    });

    assert_eq!(client.remaining_today(&signer, &token), 2_000);

    // Advance timestamp into the next 24-hour window
    env.ledger().set_timestamp(current_time + WINDOW_SECONDS);

    // Spending window has elapsed: remaining allowance returns the full limit
    assert_eq!(client.remaining_today(&signer, &token), 10_000);
}
