//! Regression test for issue #692: `token_client` must report missing state
//! with the crate's typed error instead of aborting the host call.
//!
//! The test reaches `token_client` on a registered contract whose
//! constructor-written `Token` key is absent — the state a caller observes
//! before initialization — and asserts the typed `NotInitialized` result. The
//! happy path is pinned alongside it so the typed arm did not change the
//! answer for an initialized vault.

use super::{token_client, DataKey};
use crate::{Error, StreamVault, StreamVaultClient};
use soroban_sdk::{testutils::Address as _, Address, Env};

#[test]
fn token_client_before_initialization_returns_not_initialized() {
    let env = Env::default();
    let merchant = Address::generate(&env);
    let token = Address::generate(&env);
    let contract_id = env.register(StreamVault, (merchant, token));

    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Token);
        assert_eq!(
            token_client(&env).err(),
            Some(Error::NotInitialized),
            "an absent token must return NotInitialized, not trap"
        );
    });
}

#[test]
fn token_client_after_initialization_returns_the_bound_token() {
    let env = Env::default();
    let merchant = Address::generate(&env);
    let token = Address::generate(&env);
    let contract_id = env.register(StreamVault, (merchant, token.clone()));
    let client = StreamVaultClient::new(&env, &contract_id);

    // The generated client unwraps a contract-level `Result`, so the happy
    // path still answers with the bound token address.
    assert_eq!(client.get_token(), token);
}
