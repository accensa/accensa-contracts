//! Soulbound-token tests (issue #450).
//!
//! Covers the issue's acceptance criteria: successful mints, transfer
//! reverts, governance revoke/slash — plus the auth gates and the
//! tombstone semantics around the only mutators.

extern crate std;

use super::*;
use soroban_sdk::{
    events::Event as _,
    testutils::{Address as _, EnvTestConfig, Events as _},
    String as SdkString,
};

/// Assert the last invocation emitted exactly `events` (nothing else), in
/// publish order. `env.events().all()` covers only the last invocation, so
/// assertions must run immediately after the call under test.
fn assert_emitted(
    env: &Env,
    contract: &Address,
    events: std::vec::Vec<soroban_sdk::xdr::ContractEvent>,
) {
    assert_eq!(env.events().all().filter_by_contract(contract), events);
}

struct Setup {
    env: Env,
    client: ReputationClient<'static>,
    admin: Address,
    merchant: Address,
}

fn setup() -> Setup {
    // Snapshots off: this suite pins observable behavior with explicit
    // event assertions instead of golden JSON files.
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);
    client.initialize(&admin);

    Setup {
        env,
        client,
        admin,
        merchant,
    }
}

// ── Construction ─────────────────────────────────────────────────────────

#[test]
fn initialize_resets_sbt_counters() {
    let s = setup();
    assert_eq!(s.client.total_sbt(), 0);
    assert_eq!(s.client.get_sbt(&s.merchant), None);
}

#[test]
fn initialize_twice_fails() {
    let s = setup();
    assert_eq!(
        s.client.try_initialize(&s.admin),
        Err(Ok(Error::AlreadyInitialized))
    );
}

// ── Successful mints ─────────────────────────────────────────────────────

#[test]
fn issue_mints_verified_credential() {
    let s = setup();
    let token_id = s.client.issue(&s.merchant, &1);
    assert_eq!(token_id, 0);
    assert_eq!(s.client.total_sbt(), 1);

    let sbt = s.client.get_sbt(&s.merchant).unwrap();
    assert_eq!(sbt.token_id, 0);
    assert_eq!(sbt.class, CredentialClass::Verified);
    assert_eq!(sbt.issued_at, s.env.ledger().sequence());
    assert!(!sbt.slashed);
    assert_eq!(sbt.slashed_at, None);
    assert_eq!(sbt.reason, None);
}

#[test]
fn issue_emits_issued_event() {
    let s = setup();
    s.client.issue(&s.merchant, &2);
    assert_emitted(
        &s.env,
        &s.client.address,
        std::vec![SbtIssuedEvent {
            token_id: 0,
            owner: s.merchant.clone(),
            class: CredentialClass::Trusted,
        }
        .to_xdr(&s.env, &s.client.address)],
    );
}

#[test]
fn token_ids_are_monotonic_across_merchants() {
    let s = setup();
    let other = Address::generate(&s.env);
    assert_eq!(s.client.issue(&s.merchant, &1), 0);
    assert_eq!(s.client.issue(&other, &3), 1);
    assert_eq!(
        s.client.get_sbt(&other).unwrap().class,
        CredentialClass::Premium
    );
    assert_eq!(s.client.total_sbt(), 2);
}

#[test]
fn issue_rejects_invalid_class() {
    let s = setup();
    assert_eq!(
        s.client.try_issue(&s.merchant, &0),
        Err(Ok(SbtError::InvalidClass))
    );
    assert_eq!(
        s.client.try_issue(&s.merchant, &4),
        Err(Ok(SbtError::InvalidClass))
    );
    assert_eq!(s.client.get_sbt(&s.merchant), None);
}

#[test]
fn double_issue_fails() {
    let s = setup();
    s.client.issue(&s.merchant, &1);
    assert_eq!(
        s.client.try_issue(&s.merchant, &2),
        Err(Ok(SbtError::AlreadyIssued))
    );
    // The original credential is untouched.
    assert_eq!(
        s.client.get_sbt(&s.merchant).unwrap().class,
        CredentialClass::Verified
    );
}

// ── Transfer reverts (non-transferability) ───────────────────────────────

#[test]
fn transfer_always_reverts() {
    let s = setup();
    let buyer = Address::generate(&s.env);
    s.client.issue(&s.merchant, &1);

    // Even the holder and the governance authority cannot move it.
    assert_eq!(
        s.client.try_transfer(&s.merchant, &buyer, &0),
        Err(Ok(SbtError::SbtNonTransferable))
    );
    assert_eq!(
        s.client.try_transfer(&s.admin, &buyer, &0),
        Err(Ok(SbtError::SbtNonTransferable))
    );
    // Nothing moved, nothing was created.
    assert_eq!(s.client.get_sbt(&buyer), None);
    assert_eq!(s.client.get_sbt(&s.merchant).unwrap().token_id, 0);
    assert_eq!(s.client.total_sbt(), 1);
}

#[test]
fn approve_always_reverts() {
    let s = setup();
    let spender = Address::generate(&s.env);
    s.client.issue(&s.merchant, &1);

    assert_eq!(
        s.client.try_approve(&spender, &0),
        Err(Ok(SbtError::SbtApprovalDisabled))
    );
    assert_eq!(
        s.client.try_approve(&s.admin, &0),
        Err(Ok(SbtError::SbtApprovalDisabled))
    );
}

/// The credential's storage binding lives under a key derived from its
/// owner, inside contract storage; peek at it the way the contract would.
fn sbt_present(env: &Env, contract: &Address, owner: &Address) -> bool {
    env.as_contract(contract, || {
        env.storage()
            .persistent()
            .has(&SbtDataKey::Sbt(owner.clone()))
    })
}

#[test]
fn credentials_cannot_move_between_addresses() {
    let s = setup();
    let attacker = Address::generate(&s.env);
    s.client.issue(&s.merchant, &1);

    // The credential is stored under a key derived from its owner and there
    // is no working transfer/approve entry point, so the only way `attacker`
    // ends up holding *a* credential is a fresh, separate mint by
    // governance — never the merchant's.
    s.client.issue(&attacker, &1);
    assert_eq!(s.client.get_sbt(&s.merchant).unwrap().token_id, 0);
    assert_eq!(s.client.get_sbt(&attacker).unwrap().token_id, 1);
    assert!(sbt_present(&s.env, &s.client.address, &s.merchant));
    assert!(sbt_present(&s.env, &s.client.address, &attacker));
}

// ── Governance revoke ────────────────────────────────────────────────────

#[test]
fn revoke_burns_the_credential() {
    let s = setup();
    s.client.issue(&s.merchant, &1);
    let token_id = s.client.revoke(&s.merchant);
    assert_eq!(token_id, 0);

    // The revoke invocation is the last one, so its event is observable.
    assert_emitted(
        &s.env,
        &s.client.address,
        std::vec![SbtRevokedEvent {
            token_id: 0,
            owner: s.merchant.clone(),
        }
        .to_xdr(&s.env, &s.client.address)],
    );

    assert_eq!(s.client.get_sbt(&s.merchant), None);
    assert!(!sbt_present(&s.env, &s.client.address, &s.merchant));
}

#[test]
fn revoked_credential_cannot_be_reissued() {
    let s = setup();
    s.client.issue(&s.merchant, &1);
    s.client.revoke(&s.merchant);

    assert_eq!(
        s.client.try_issue(&s.merchant, &1),
        Err(Ok(SbtError::Revoked))
    );
    assert_eq!(s.client.get_sbt(&s.merchant), None);
}

#[test]
fn revoke_missing_credential_fails() {
    let s = setup();
    assert_eq!(
        s.client.try_revoke(&s.merchant),
        Err(Ok(SbtError::SbtNotFound))
    );
}

// ── Governance slash ─────────────────────────────────────────────────────

#[test]
fn slash_flags_the_credential_in_place() {
    let s = setup();
    s.client.issue(&s.merchant, &1);
    let token_id = s
        .client
        .slash(&s.merchant, &SdkString::from_str(&s.env, "fraud"));
    assert_eq!(token_id, 0);

    // The slash invocation is the last one, so its event is observable.
    assert_emitted(
        &s.env,
        &s.client.address,
        std::vec![SbtSlashedEvent {
            token_id: 0,
            owner: s.merchant.clone(),
            slashed_at: s.env.ledger().sequence(),
            reason: SdkString::from_str(&s.env, "fraud"),
        }
        .to_xdr(&s.env, &s.client.address)],
    );

    let sbt = s.client.get_sbt(&s.merchant).unwrap();
    assert!(sbt.slashed);
    assert_eq!(sbt.slashed_at, Some(s.env.ledger().sequence()));
    // Still bound to the same owner and token id.
    assert_eq!(sbt.token_id, 0);
    assert!(sbt_present(&s.env, &s.client.address, &s.merchant));
}

#[test]
fn double_slash_fails() {
    let s = setup();
    s.client.issue(&s.merchant, &1);
    s.client
        .slash(&s.merchant, &SdkString::from_str(&s.env, "fraud"));
    assert_eq!(
        s.client
            .try_slash(&s.merchant, &SdkString::from_str(&s.env, "again")),
        Err(Ok(SbtError::AlreadySlashed))
    );
}

#[test]
fn slash_missing_credential_fails() {
    let s = setup();
    assert_eq!(
        s.client
            .try_slash(&s.merchant, &SdkString::from_str(&s.env, "fraud")),
        Err(Ok(SbtError::SbtNotFound))
    );
}

// ── Access control ───────────────────────────────────────────────────────

#[test]
#[should_panic]
fn issue_requires_governance_auth() {
    let s = setup();
    s.env.set_auths(&[]);
    s.client.issue(&s.merchant, &1);
}

#[test]
#[should_panic]
fn revoke_requires_governance_auth() {
    let s = setup();
    s.client.issue(&s.merchant, &1);
    s.env.set_auths(&[]);
    s.client.revoke(&s.merchant);
}

#[test]
#[should_panic]
fn slash_requires_governance_auth() {
    let s = setup();
    s.client.issue(&s.merchant, &1);
    s.env.set_auths(&[]);
    s.client
        .slash(&s.merchant, &SdkString::from_str(&s.env, "fraud"));
}

// ── NotInitialized ───────────────────────────────────────────────────────

#[test]
fn uninitialized_contract_rejects_issue() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);
    assert_eq!(
        client.try_issue(&Address::generate(&env), &1),
        Err(Ok(SbtError::NotInitialized))
    );
}
