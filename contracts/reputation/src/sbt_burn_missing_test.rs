//! Regression test for issue #691: `burn` must report missing state with the
//! crate's typed error instead of aborting the host call.
//!
//! The test reaches `burn` on a freshly registered contract, before any
//! `initialize` or `issue` has written a credential, and asserts both the
//! typed `SbtNotFound` result and that the failure left no tombstone behind.

use super::{burn, SbtDataKey};
use crate::{Reputation, SbtError};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig},
    Address, Env,
};

#[test]
fn burn_before_initialization_returns_sbt_not_found() {
    // Snapshots off: like `sbt_test`, this suite pins observable behaviour
    // with explicit assertions instead of golden JSON files.
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let owner = Address::generate(&env);
    let contract_id = env.register(Reputation, ());

    env.as_contract(&contract_id, || {
        assert_eq!(
            burn(&env, &owner),
            Err(SbtError::SbtNotFound),
            "an absent credential must return SbtNotFound, not trap"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&SbtDataKey::Revoked(owner.clone())),
            "a failed burn must not write a re-issue-blocking tombstone"
        );
    });
}
