//! #998 — pruning bounties settle only in the shard's configured asset.
//!
//! A shard may hold balances of several tokens (fees, donations, test
//! assets), so a caller-chosen payout token would let anyone drain whatever
//! the contract happens to hold. Each shard is constructed with one
//! `bounty_asset`; `claim_prune_bounty` rejects every other token with
//! [`Error::UnsupportedAsset`], and fails closed with the same error when no
//! asset is recorded at all.

#![cfg(test)]

use accensa_common::Error;
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::Address as _,
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

fn asset(env: &Env) -> Address {
    env.register_stellar_asset_contract_v2(Address::generate(env))
        .address()
}

fn register(env: &Env, bounty_asset: &Address) -> ReceiptShardClient<'static> {
    env.mock_all_auths();
    let id = env.register(
        ReceiptShard,
        (Address::generate(env), 1u64, 1001u64, bounty_asset.clone()),
    );
    ReceiptShardClient::new(env, &id)
}

fn accrue(env: &Env, shard: &ReceiptShardClient, claimer: &Address, amount: i128) {
    env.as_contract(&shard.address, || {
        env.storage()
            .instance()
            .set(&DataKey::PruneBounty(claimer.clone()), &amount);
    });
}

#[test]
fn get_bounty_asset_reports_the_constructor_value() {
    let env = Env::default();
    let token = asset(&env);
    let shard = register(&env, &token);

    assert_eq!(shard.get_bounty_asset(), token);

    env.as_contract(&shard.address, || {
        env.storage().instance().remove(&DataKey::BountyAsset);
    });
    assert_eq!(
        shard.try_get_bounty_asset(),
        Err(Ok(Error::NotInitialized)),
        "a shard with no recorded asset must answer with the typed error"
    );
}

#[test]
fn claim_pays_the_accrual_in_the_configured_asset() {
    let env = Env::default();
    let token = asset(&env);
    let shard = register(&env, &token);
    let claimer = Address::generate(&env);

    accrue(&env, &shard, &claimer, 500);
    StellarAssetClient::new(&env, &token).mint(&shard.address, &500);

    assert_eq!(shard.claim_prune_bounty(&token, &claimer), 500);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&shard.address), 0);
    assert_eq!(token_client.balance(&claimer), 500);
    assert_eq!(shard.get_prune_bounty(&claimer), 0);

    // The accrual was zeroed before the transfer, so a repeated claim pays
    // nothing even though the shard still exists.
    assert_eq!(shard.claim_prune_bounty(&token, &claimer), 0);
}

#[test]
fn claim_rejects_a_token_other_than_the_configured_asset() {
    let env = Env::default();
    let configured = asset(&env);
    let other = asset(&env);
    let shard = register(&env, &configured);
    let claimer = Address::generate(&env);

    accrue(&env, &shard, &claimer, 500);
    StellarAssetClient::new(&env, &configured).mint(&shard.address, &500);
    StellarAssetClient::new(&env, &other).mint(&shard.address, &500);

    assert_eq!(
        shard.try_claim_prune_bounty(&other, &claimer),
        Err(Ok(Error::UnsupportedAsset)),
        "a claim naming any other token must be refused"
    );

    // Nothing moved: the wrong-token balance, the configured-token balance
    // and the accrual are all untouched.
    assert_eq!(TokenClient::new(&env, &other).balance(&shard.address), 500);
    assert_eq!(
        TokenClient::new(&env, &configured).balance(&shard.address),
        500
    );
    assert_eq!(shard.get_prune_bounty(&claimer), 500);

    // The configured asset still settles normally afterwards.
    assert_eq!(shard.claim_prune_bounty(&configured, &claimer), 500);
}

#[test]
fn claim_fails_closed_when_no_asset_is_recorded() {
    let env = Env::default();
    let token = asset(&env);
    let shard = register(&env, &token);
    let claimer = Address::generate(&env);

    accrue(&env, &shard, &claimer, 500);
    StellarAssetClient::new(&env, &token).mint(&shard.address, &500);
    env.as_contract(&shard.address, || {
        env.storage().instance().remove(&DataKey::BountyAsset);
    });

    assert_eq!(
        shard.try_claim_prune_bounty(&token, &claimer),
        Err(Ok(Error::UnsupportedAsset)),
        "a shard with no recorded asset must not pay an arbitrary token"
    );
    assert_eq!(TokenClient::new(&env, &token).balance(&shard.address), 500);
    assert_eq!(shard.get_prune_bounty(&claimer), 500);
}
