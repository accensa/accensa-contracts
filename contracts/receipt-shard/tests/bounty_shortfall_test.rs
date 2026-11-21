//! Shortfall coverage for `ReceiptShard::claim_prune_bounty` (issue #994).
//!
//! A shard pays bounties from whatever token balance it holds, so an accrual
//! larger than that balance is a normal state, not an error. These tests pin
//! that the unpayable remainder stays accrued: it is written back before the
//! transfer, so funding the shard later pays it out and the payouts sum to the
//! original accrual instead of the shortfall being burned.

use receipt_shard::pruning::{PRUNE_BOUNTY_PER_BATCH, RETENTION_LEDGERS};
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

/// Base ledger sequence the fixture anchors at (see `pruning_test.rs`).
const BASE_LEDGER: u32 = 100_000;

struct Fixture {
    env: Env,
    client: ReceiptShardClient<'static>,
    shard: Address,
    pruner: Address,
    token: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let pruner = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();

    let shard = env.register(ReceiptShard, (router, 1u64, 1001u64, token.clone()));
    let client = ReceiptShardClient::new(&env, &shard);
    env.ledger().with_mut(|li| li.sequence_number = BASE_LEDGER);

    Fixture {
        env,
        client,
        shard,
        pruner,
        token,
    }
}

impl Fixture {
    /// Anchor three batches, age them past retention and prune them, leaving
    /// `pruner` with exactly three bounties accrued.
    fn accrue_three_bounties(&self) {
        let root = BytesN::from_array(&self.env, &[7u8; 32]);
        for id in 1..=3u64 {
            self.client.anchor_batch(&id, &root, &1, &0, &100);
        }
        let seq = self.env.ledger().sequence();
        self.env
            .ledger()
            .with_mut(|li| li.sequence_number = seq + RETENTION_LEDGERS + 1);

        assert_eq!(self.client.prune_expired_receipts(&self.pruner, &3), 3);
        assert_eq!(
            self.client.get_prune_bounty(&self.pruner),
            3 * PRUNE_BOUNTY_PER_BATCH
        );
    }

    fn fund_shard(&self, amount: i128) {
        StellarAssetClient::new(&self.env, &self.token).mint(&self.shard, &amount);
    }

    fn token_balance(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.token).balance(who)
    }
}

#[test]
fn shortfall_stays_accrued_and_pays_out_when_funded() {
    let fx = setup();
    fx.accrue_three_bounties();
    let accrued = 3 * PRUNE_BOUNTY_PER_BATCH;

    // The shard holds one bounty against an accrual of three.
    fx.fund_shard(PRUNE_BOUNTY_PER_BATCH);

    let first = fx.client.claim_prune_bounty(&fx.token, &fx.pruner);
    assert_eq!(first, PRUNE_BOUNTY_PER_BATCH);
    // The two unpayable bounties were not burned: they are still accrued.
    assert_eq!(
        fx.client.get_prune_bounty(&fx.pruner),
        2 * PRUNE_BOUNTY_PER_BATCH
    );
    assert_eq!(fx.token_balance(&fx.pruner), PRUNE_BOUNTY_PER_BATCH);

    // Funding the shard makes the remainder payable on the next claim.
    fx.fund_shard(2 * PRUNE_BOUNTY_PER_BATCH);
    let second = fx.client.claim_prune_bounty(&fx.token, &fx.pruner);
    assert_eq!(second, 2 * PRUNE_BOUNTY_PER_BATCH);

    // The two payouts sum to the original accrual, and nothing is left over.
    assert_eq!(first + second, accrued);
    assert_eq!(fx.client.get_prune_bounty(&fx.pruner), 0);
    assert_eq!(fx.token_balance(&fx.pruner), accrued);
}

#[test]
fn empty_shard_balance_keeps_the_whole_accrual() {
    let fx = setup();
    fx.accrue_three_bounties();

    // Nothing minted: the claim pays nothing but burns nothing either.
    assert_eq!(fx.client.claim_prune_bounty(&fx.token, &fx.pruner), 0);
    assert_eq!(
        fx.client.get_prune_bounty(&fx.pruner),
        3 * PRUNE_BOUNTY_PER_BATCH
    );

    fx.fund_shard(3 * PRUNE_BOUNTY_PER_BATCH);
    assert_eq!(
        fx.client.claim_prune_bounty(&fx.token, &fx.pruner),
        3 * PRUNE_BOUNTY_PER_BATCH
    );
    assert_eq!(fx.client.get_prune_bounty(&fx.pruner), 0);
}

#[test]
fn zero_accrual_makes_no_token_call_and_no_storage_write() {
    let fx = setup();

    // Calling with an unsupported token fails with UnsupportedAsset.
    let not_a_token = Address::generate(&fx.env);
    assert!(fx
        .client
        .try_claim_prune_bounty(&not_a_token, &fx.pruner)
        .is_err());

    // Calling with the configured bounty asset when accrual is zero returns 0 without writing storage.
    assert_eq!(fx.client.claim_prune_bounty(&fx.token, &fx.pruner), 0);

    // No storage entry was created for the caller.
    assert!(!fx.env.as_contract(&fx.shard, || fx
        .env
        .storage()
        .instance()
        .has(&DataKey::PruneBounty(fx.pruner.clone()))));

    // And a second claim still pays nothing.
    assert_eq!(fx.client.claim_prune_bounty(&fx.token, &fx.pruner), 0);
    assert_eq!(fx.client.get_prune_bounty(&fx.pruner), 0);
}
