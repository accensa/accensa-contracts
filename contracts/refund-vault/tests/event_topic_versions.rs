//! Refund topic shapes pinned per deployed version (issue #942).
//!
//! The live testnet deployment still emits 0.1.0's `("refunded", payment_ref)`
//! while current source publishes `("refund_event", payment_ref)` (and the
//! batch-only `("batch_refund_event",)`); the shapes live in
//! `contracts/receipt-anchor/event-topic-vectors.json`, included here by
//! relative path so there is exactly one generated copy. These tests drive the
//! real entry points and assert the published topics equal the current-source
//! entry, and pin that the refund arms differ in lead symbol so a topic filter
//! cannot match the wrong deployment.

#[path = "../../receipt-anchor/tests/fixtures/event_topic_vectors.rs"]
mod event_topic_vectors;

use accensa_common::VaultInit;
use event_topic_vectors::{
    BATCH_REFUND_CURRENT, BATCH_REFUND_LEGACY_0_1_0, REFUND_CURRENT, REFUND_LEGACY_0_1_0,
};
use refund_policy_time::TimePolicy;
use refund_policy_vdf::VdfPolicy;
use refund_vault::{RefundParam, RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Events as _},
    token::StellarAssetClient,
    vec,
    xdr::{ContractEventBody, ScVal},
    Address, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;
/// Ledger-count refund window. Wide enough that no assertion here is stopped
/// by the time gate, and 0 ledgers of elapsed time means `paid_at_ledger = 0`
/// is always inside it.
const WINDOW: u32 = 17_280;

struct TestEnv<'a> {
    env: Env,
    vault: RefundVaultClient<'a>,
    #[allow(dead_code)]
    merchant: Address,
    #[allow(dead_code)]
    token: Address,
}

fn setup<'a>() -> TestEnv<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: Some(env.register(TimePolicy, ())),
            vdf_policy: Some(env.register(VdfPolicy, ())),
            fee_bps: 0,
            fee_recipient: None,
            refund_window: WINDOW,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let vault = RefundVaultClient::new(&env, &vault_id);
    vault.deposit(&merchant, &(FLOAT / 4), &None);

    TestEnv {
        env,
        vault,
        merchant,
        token,
    }
}

/// Resolve vector tags to the topic `ScVal`s an indexer filters on: bare
/// words are Symbols, "bytes32" is the payment ref the test chose. Unknown
/// tags fail loudly so a vector typo cannot silently pass.
fn resolve_topics(shape: &[&str], payment_ref: &[u8; 32]) -> std::vec::Vec<ScVal> {
    shape
        .iter()
        .map(|tag| match *tag {
            "bytes32" => ScVal::Bytes(payment_ref.to_vec().try_into().expect("32-byte ref")),
            sym => ScVal::Symbol(sym.try_into().expect("vector symbol tag")),
        })
        .collect()
}

/// Topic tuples the last invocation published on `contract`, one entry per
/// event. `env.events()` only reflects the last top-level call, so this runs
/// immediately after the entry point under test.
fn published_topic_sets(env: &Env, contract: &Address) -> std::vec::Vec<std::vec::Vec<ScVal>> {
    env.events()
        .all()
        .filter_by_contract(contract)
        .events()
        .iter()
        .map(|event| match &event.body {
            ContractEventBody::V0(v0) => v0.topics.to_vec(),
        })
        .collect()
}

fn payment_ref(byte: u8) -> [u8; 32] {
    [byte; 32]
}

#[test]
fn refund_publishes_current_topics_and_differs_from_legacy() {
    let TestEnv {
        env,
        vault,
        merchant: _,
        token: _,
    } = setup();

    let buyer = Address::generate(&env);
    let pref = payment_ref(9);
    vault.refund(
        &BytesN::from_array(&env, &pref),
        &buyer,
        &100,
        &0,
        &100,
        &None,
        &0,
    );

    let expected = resolve_topics(REFUND_CURRENT, &pref);
    let published = published_topic_sets(&env, &vault.address);
    assert_eq!(
        published
            .iter()
            .filter(|topics| **topics == expected)
            .count(),
        1,
        "refund must publish exactly the current vector topics"
    );

    // Both arms are 2-tuples, so the lead symbol is what keeps a legacy
    // filter from matching current events.
    let legacy = REFUND_LEGACY_0_1_0.expect("legacy refund arm pinned");
    assert_eq!(
        REFUND_CURRENT.len(),
        legacy.len(),
        "both refund arms are 2-tuples"
    );
    assert_ne!(
        REFUND_CURRENT[0], legacy[0],
        "current vs legacy lead symbol"
    );
}

#[test]
fn batch_refund_publishes_current_topics_and_has_no_legacy_arm() {
    let TestEnv {
        env,
        vault,
        merchant: _,
        token: _,
    } = setup();

    let params = vec![
        &env,
        RefundParam {
            payment_ref: BytesN::from_array(&env, &payment_ref(11)),
            recipient: Address::generate(&env),
            amount: 100,
            paid_at_ledger: 0,
            payment_amount: 100,
            vdf_proof: None,
        },
        RefundParam {
            payment_ref: BytesN::from_array(&env, &payment_ref(12)),
            recipient: Address::generate(&env),
            amount: 100,
            paid_at_ledger: 0,
            payment_amount: 100,
            vdf_proof: None,
        },
    ];
    let results = vault.process_batch(&params, &0);
    assert_eq!(results, vec![&env, true, true]);

    let expected = resolve_topics(BATCH_REFUND_CURRENT, &payment_ref(0));
    let published = published_topic_sets(&env, &vault.address);
    assert_eq!(
        published
            .iter()
            .filter(|topics| **topics == expected)
            .count(),
        1,
        "process_batch must publish exactly the current vector topics"
    );

    assert!(
        BATCH_REFUND_LEGACY_0_1_0.is_none(),
        "no legacy batch shape is pinned: the 0.1.0 sources name only anchored/refunded"
    );
}
