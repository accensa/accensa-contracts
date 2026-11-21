//! Integration tests validating the documented StreamVault lifecycle (issue #1002).

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};
use stream_vault::{Error, StreamVault, StreamVaultClient};

const START: u32 = 1_000;
const STOP: u32 = 1_100;
const RATE: i128 = 100;
const DEPOSIT: i128 = 10_000; // (STOP - START) * RATE

struct TestFixture {
    env: Env,
    client: StreamVaultClient<'static>,
    token: TokenClient<'static>,
    merchant: Address,
    buyer: Address,
}

fn setup() -> TestFixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(START - 50);

    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token_address = sac.address();
    StellarAssetClient::new(&env, &token_address).mint(&buyer, &1_000_000);

    let contract_id = env.register(StreamVault, (merchant.clone(), token_address.clone()));
    let client = StreamVaultClient::new(&env, &contract_id);
    let token = TokenClient::new(&env, &token_address);

    TestFixture {
        env,
        client,
        token,
        merchant,
        buyer,
    }
}

#[test]
fn test_documented_stream_vault_lifecycle() {
    let fx = setup();

    // 1. Create Stream (State: Running)
    let stream_id = fx
        .client
        .create_stream(&fx.buyer, &START, &STOP, &RATE, &DEPOSIT);
    assert_eq!(stream_id, 0);

    // Initial balances
    assert_eq!(fx.token.balance(&fx.merchant), 0);

    // 2. Advance to START + 20 -> Accrued: 20 * 100 = 2,000
    fx.env.ledger().set_sequence_number(START + 20);
    let claimed = fx.client.claim_stream(&stream_id);
    assert_eq!(claimed, 2_000);
    assert_eq!(fx.token.balance(&fx.merchant), 2_000);

    // Calling claim again immediately fails with NothingToWithdraw
    let res = fx.client.try_claim_stream(&stream_id);
    assert_eq!(res, Err(Ok(Error::NothingToWithdraw)));

    // 3. Pause Stream (State: Paused) at START + 30
    fx.env.ledger().set_sequence_number(START + 30);
    fx.client.pause_stream(&stream_id);

    // Pausing again fails with StreamNotActive
    let res = fx.client.try_pause_stream(&stream_id);
    assert_eq!(res, Err(Ok(Error::StreamNotActive)));

    // While paused, advancing ledger accrues nothing new
    fx.env.ledger().set_sequence_number(START + 50); // 20 ledgers passed while paused

    // 4. Resume Stream (State: Running)
    fx.client.resume_stream(&stream_id);

    // Resuming again fails with StreamNotActive
    let res = fx.client.try_resume_stream(&stream_id);
    assert_eq!(res, Err(Ok(Error::StreamNotActive)));

    // 5. Cancel Stream (State: Cancelled)
    // At pause (START + 30), total accrued was 30 * 100 = 3,000.
    // Since 2,000 was already claimed, merchant is owed 1,000.
    // Buyer gets refund of unspent principal: 10,000 - 3,000 = 7,000.
    let buyer_balance_before = fx.token.balance(&fx.buyer);
    fx.client.cancel_stream(&stream_id);

    assert_eq!(fx.token.balance(&fx.merchant), 3_000);
    assert_eq!(fx.token.balance(&fx.buyer), buyer_balance_before + 7_000);

    // Stream is removed; subsequent operations return StreamNotFound
    let res = fx.client.try_claim_stream(&stream_id);
    assert_eq!(res, Err(Ok(Error::StreamNotFound)));
}
