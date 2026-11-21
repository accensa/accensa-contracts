//! #969 — One emergency-stop guard, shared by all nine entry points.
//!
//! `deposit`, `refund`, `claim_batch`, `process_batch`, `withdraw`,
//! `distribute_yield`, `deploy_to_yield`, `withdraw_from_yield` and
//! `harvest_yield` each used to carry their own copy of the `IsPaused` read.
//! The copies could drift, and two of the most-reviewed paths already ordered
//! their checks differently (`refund` looked at pause before the caller's
//! nonce, `withdraw` before `InvalidAmount`), so a single divergent copy was
//! invisible to CI. They now call `ensure_not_paused`, and this file pins both
//! halves of that claim:
//!
//!   1. while the vault is paused, every one of the nine answers the *shared*
//!      `Error::Paused` — including the four whose own first check would
//!      otherwise have answered a different error, which is what proves the
//!      guard still precedes it; and
//!   2. once unpaused, the same nine calls reach their own first check, so no
//!      copy was widened into a permanent block.
//!
//! Each call below is made with the arguments that make the first check *after*
//! the guard fail: `amount <= 0` for the four money-moving entry points, a
//! nonce of `1` against a stored user nonce of `0` for the three refund paths,
//! a missing yield strategy for `harvest_yield`, and an empty harvested balance
//! for `distribute_yield`. Those calls all revert, so the same vault can be
//! driven through both phases.
//!
//! Setup shape follows `tests/multisig_admin_vault.rs` /
//! `tests/yield_events.rs`. No yield strategy is registered on purpose: it is
//! what makes the yield entry points report their own configuration error
//! rather than succeed.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundClaim, RefundParam, RefundVault, RefundVaultClient};
use soroban_sdk::token::StellarAssetClient;
use soroban_sdk::{testutils::Address as _, vec, Address, BytesN, Env, Vec};

const FLOAT: i128 = 1_000_000;

struct Vault {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
    merchant: Address,
    recipient: Address,
    token: Address,
}

fn setup() -> Vault {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 100,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);

    // A funded vault: nothing below needs the float, but a stop that only
    // blocks an empty vault would not be the emergency stop the flag claims.
    client.deposit(&merchant, &FLOAT, &None);

    let recipient = Address::generate(&env);

    Vault {
        env,
        contract,
        client,
        merchant,
        recipient,
        token,
    }
}

/// One refund item, with a nonce of `1` while the stored user nonce is still
/// `0` — see the module note. The amount is irrelevant: the call never reaches
/// the float.
fn single_claim(env: &Env, v: &Vault) -> Vec<RefundClaim> {
    vec![
        env,
        RefundClaim {
            payment_ref: BytesN::from_array(env, &[1u8; 32]),
            recipient: v.recipient.clone(),
            amount: 100,
            paid_at_ledger: 0,
            payment_amount: 100,
            vdf_proof: None,
        },
    ]
}

/// The same item in the `process_batch` shape.
fn single_param(env: &Env, v: &Vault) -> Vec<RefundParam> {
    vec![
        env,
        RefundParam {
            payment_ref: BytesN::from_array(env, &[2u8; 32]),
            recipient: v.recipient.clone(),
            amount: 100,
            paid_at_ledger: 0,
            payment_amount: 100,
            vdf_proof: None,
        },
    ]
}

/// `true` when the call answered the shared guard.
fn answers_paused(v: &Vault) -> [(&'static str, bool); 9] {
    let payment_ref = BytesN::from_array(&v.env, &[3u8; 32]);
    let claims = single_claim(&v.env, v);
    let params = single_param(&v.env, v);
    [
        (
            "deposit",
            matches!(
                v.client.try_deposit(&v.merchant, &0, &None),
                Err(Ok(Error::Paused))
            ),
        ),
        (
            "refund",
            matches!(
                v.client
                    .try_refund(&payment_ref, &v.recipient, &100, &0, &100, &None, &1),
                Err(Ok(Error::Paused))
            ),
        ),
        (
            "claim_batch",
            matches!(
                v.client.try_claim_batch(&claims, &1),
                Err(Ok(Error::Paused))
            ),
        ),
        (
            "process_batch",
            matches!(
                v.client.try_process_batch(&params, &1),
                Err(Ok(Error::Paused))
            ),
        ),
        (
            "withdraw",
            matches!(
                v.client.try_withdraw(&0, &v.merchant),
                Err(Ok(Error::Paused))
            ),
        ),
        (
            "distribute_yield",
            matches!(v.client.try_distribute_yield(), Err(Ok(Error::Paused))),
        ),
        (
            "deploy_to_yield",
            matches!(v.client.try_deploy_to_yield(&0), Err(Ok(Error::Paused))),
        ),
        (
            "withdraw_from_yield",
            matches!(v.client.try_withdraw_from_yield(&0), Err(Ok(Error::Paused))),
        ),
        (
            "harvest_yield",
            matches!(v.client.try_harvest_yield(), Err(Ok(Error::Paused))),
        ),
    ]
}

/// The same nine calls, each paired with the error its *own* first check after
/// the guard produces. A guard copy that got widened — or a check that got
/// reordered behind it — fails here rather than in production.
fn answers_own_check(v: &Vault) -> [(&'static str, bool); 9] {
    let payment_ref = BytesN::from_array(&v.env, &[3u8; 32]);
    let claims = single_claim(&v.env, v);
    let params = single_param(&v.env, v);
    [
        (
            "deposit",
            matches!(
                v.client.try_deposit(&v.merchant, &0, &None),
                Err(Ok(Error::InvalidAmount))
            ),
        ),
        (
            "refund",
            matches!(
                v.client
                    .try_refund(&payment_ref, &v.recipient, &100, &0, &100, &None, &1),
                Err(Ok(Error::StaleState))
            ),
        ),
        (
            "claim_batch",
            matches!(
                v.client.try_claim_batch(&claims, &1),
                Err(Ok(Error::StaleState))
            ),
        ),
        (
            "process_batch",
            matches!(
                v.client.try_process_batch(&params, &1),
                Err(Ok(Error::StaleState))
            ),
        ),
        (
            "withdraw",
            matches!(
                v.client.try_withdraw(&0, &v.merchant),
                Err(Ok(Error::InvalidAmount))
            ),
        ),
        (
            "distribute_yield",
            matches!(
                v.client.try_distribute_yield(),
                Err(Ok(Error::NothingToHarvest))
            ),
        ),
        (
            "deploy_to_yield",
            matches!(
                v.client.try_deploy_to_yield(&0),
                Err(Ok(Error::InvalidAmount))
            ),
        ),
        (
            "withdraw_from_yield",
            matches!(
                v.client.try_withdraw_from_yield(&0),
                Err(Ok(Error::InvalidAmount))
            ),
        ),
        (
            "harvest_yield",
            matches!(v.client.try_harvest_yield(), Err(Ok(Error::StrategyNotSet))),
        ),
    ]
}

#[test]
fn all_nine_entry_points_refuse_with_the_shared_guard_while_paused() {
    let v = setup();
    v.client.pause();

    for (name, refused) in answers_paused(&v) {
        assert!(
            refused,
            "`{name}` did not answer the shared `Error::Paused` while the vault was paused",
        );
    }
}

#[test]
fn all_nine_entry_points_reach_their_own_first_check_after_unpause() {
    let v = setup();
    v.client.pause();
    v.client.unpause();

    for (name, reached) in answers_own_check(&v) {
        assert!(
            reached,
            "`{name}` did not reach its own first check after `unpause`",
        );
    }
}

/// The pause check still precedes the checks it preceded before the guard was
/// consolidated — `withdraw`'s `InvalidAmount` and `refund`'s nonce — and the
/// rejected calls leave no trace: the guard fires before anything is written.
#[test]
fn pause_keeps_priority_over_the_checks_it_preceded_and_rejects_leave_no_state() {
    let v = setup();
    v.client.pause();

    let nonce_before = v.client.get_nonce();
    let balance_before = StellarAssetClient::new(&v.env, &v.token).balance(&v.contract);

    let payment_ref = BytesN::from_array(&v.env, &[4u8; 32]);
    assert_eq!(
        v.client.try_withdraw(&FLOAT, &v.merchant),
        Err(Ok(Error::Paused)),
        "a valid withdrawal is still refused by the guard, not by `InvalidAmount`"
    );
    assert_eq!(
        v.client
            .try_refund(&payment_ref, &v.recipient, &100, &0, &100, &None, &0),
        Err(Ok(Error::Paused)),
        "a call carrying the *correct* nonce (0) is refused by the guard, proving it runs \
         before `check_and_bump_user_nonce`"
    );

    assert_eq!(v.client.get_nonce(), nonce_before);
    assert_eq!(
        StellarAssetClient::new(&v.env, &v.token).balance(&v.contract),
        balance_before
    );
}

/// A real stop-and-restart: the nine refuse while paused, and after `unpause`
/// the vault works again — the shared helper blocks, it does not wedge.
#[test]
fn the_vault_works_again_after_unpause() {
    let v = setup();
    v.client.pause();
    assert!(v.client.is_paused());
    for (name, refused) in answers_paused(&v) {
        assert!(refused, "`{name}` must refuse while paused");
    }

    v.client.unpause();
    assert!(!v.client.is_paused());

    let buyer = Address::generate(&v.env);
    // The setup deposit took the merchant's whole float, so it needs funding
    // again before it can prove the vault accepts calls once more.
    StellarAssetClient::new(&v.env, &v.token).mint(&v.merchant, &100_000);
    v.client.deposit(&v.merchant, &100_000, &None);
    v.client.withdraw(&50_000, &buyer);

    assert_eq!(
        StellarAssetClient::new(&v.env, &v.token).balance(&buyer),
        50_000
    );
    assert_eq!(
        v.client.get_nonce(),
        3,
        "deposit at setup, then deposit + withdraw"
    );
}
