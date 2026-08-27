//! #97 — RefundVault administered by a multisig *contract account*.
//!
//! On Soroban an `Address` is not necessarily a keypair: it can be a contract
//! that implements `__check_auth`, and `require_auth()` delegates to that
//! implementation. These tests prove that a `RefundVault` initialised with a
//! `MultisigAccount` as its merchant enforces the account's threshold on
//! privileged calls — with no threshold logic in the vault itself.
//!
//! The auth entries are built by hand with [`multisig_account::testutils`],
//! mirroring what wallet tooling produces for a contract account with
//! delegated signers.

use multisig_account::testutils::make_auth_entry_no_args;
use multisig_account::{MultisigAccount, MultisigAccountClient};
use refund_vault::{RefundVault, RefundVaultClient};
use refund_window_policy::RefundWindowPolicy;
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

/// Deploy the multisig account (signers `s1`, `s2`, threshold 2) and a
/// `RefundVault` initialised with the account as merchant.
///
/// No `mock_all_auths()`: the point of these tests is that the host runs the
/// account's real `__check_auth`.
fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();

    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);

    let multisig_id = env.register(MultisigAccount, (vec![&env, s1.clone(), s2.clone()], 2u32));
    let _ = MultisigAccountClient::new(&env, &multisig_id);

    let policy_id = env.register(RefundWindowPolicy, ());
    let vault_token = Address::generate(&env);
    let vault_id = env.register(RefundVault, (multisig_id.clone(), vault_token.clone(), 100, policy_id));
    let vault = RefundVaultClient::new(&env, &vault_id);

    (env, vault_id, multisig_id, s1, s2)
}

/// A privileged call (`pause`) completes when the account's full threshold of
/// signers authorises it.
#[test]
fn vault_privileged_call_succeeds_under_multisig_admin() {
    let (env, vault_id, multisig_id, s1, s2) = setup();

    let entry = make_auth_entry_no_args(&env, &multisig_id, &vault_id, "pause", &[s1, s2]);
    env.set_auths(&[entry]);

    let vault = RefundVaultClient::new(&env, &vault_id);
    // Returns `()` — panics if the account's `__check_auth` rejects the call.
    vault.pause();
}

/// The account's own rule (threshold 2) rejects a call carrying only one
/// signer, even though every attached signer is legitimate.
#[test]
fn vault_rejects_call_below_multisig_threshold() {
    let (env, vault_id, multisig_id, s1, _s2) = setup();

    let entry = make_auth_entry_no_args(&env, &multisig_id, &vault_id, "pause", &[s1]);
    env.set_auths(&[entry]);

    let vault = RefundVaultClient::new(&env, &vault_id);
    assert!(
        vault.try_pause().is_err(),
        "a single signer must not clear a threshold of two"
    );
}

/// A signer that is not registered on the account is rejected even when the
/// count would otherwise meet the threshold.
#[test]
fn vault_rejects_unknown_signer() {
    let (env, vault_id, multisig_id, s1, s2) = setup();
    let stranger = Address::generate(&env);

    let entry =
        make_auth_entry_no_args(&env, &multisig_id, &vault_id, "pause", &[s1, s2, stranger]);
    env.set_auths(&[entry]);

    let vault = RefundVaultClient::new(&env, &vault_id);
    assert!(
        vault.try_pause().is_err(),
        "an unregistered signer must be rejected"
    );
}
