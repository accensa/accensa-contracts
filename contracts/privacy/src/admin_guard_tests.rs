//! Admin-guard tests (issue #617): a missing `DataKey::Admin` resolves to a
//! typed `Error::NotInitialized` from every admin-guarded entry point — and
//! from the `admin` lookup itself — instead of trapping inside `expect`.
//!
//! `__constructor` is the only writer of `DataKey::Admin`, so an absent key is
//! exactly the pre-initialisation state, and also the state an entry-level TTL
//! eviction of that key leaves behind. `Env::register` always runs the
//! constructor, so the tests model the absence by removing the entry the
//! constructor wrote.

use super::*;
use soroban_sdk::{testutils::Address as _, Bytes};

struct Setup {
    env: Env,
    client: PrivacyClient<'static>,
    contract: Address,
    admin: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract = env.register(Privacy, (admin.clone(),));
    let client = PrivacyClient::new(&env, &contract);

    Setup {
        env,
        client,
        contract,
        admin,
    }
}

impl Setup {
    /// Remove the instance's admin entry. This models both states this issue
    /// covers: an instance the constructor never bound an admin to
    /// (pre-initialisation) and an instance whose admin entry was evicted
    /// (post-eviction). In both, `get(&DataKey::Admin)` yields `None`.
    fn evict_admin(&self) {
        self.env.as_contract(&self.contract, || {
            self.env.storage().instance().remove(&DataKey::Admin);
        });
    }
}

// ── Helpers (kept local; test.rs's copies are private to that module) ────

fn valid_proof(env: &Env) -> Groth16Proof {
    Groth16Proof {
        a: Bytes::from_slice(env, &[1u8; 64]),
        b: Bytes::from_slice(env, &[2u8; 128]),
        c: Bytes::from_slice(env, &[3u8; 64]),
    }
}

fn valid_vk(env: &Env, public_input_count: u32) -> VerifyingKey {
    let mut ic = Vec::new(env);
    for i in 0..=public_input_count {
        ic.push_back(Bytes::from_slice(env, &[(i + 10) as u8; 64]));
    }
    VerifyingKey {
        alpha_g1: Bytes::from_slice(env, &[4u8; 64]),
        beta_g2: Bytes::from_slice(env, &[5u8; 128]),
        gamma_g2: Bytes::from_slice(env, &[6u8; 128]),
        delta_g2: Bytes::from_slice(env, &[7u8; 128]),
        ic,
    }
}

fn value_n32(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

fn inputs_with_commitment(
    env: &Env,
    amount: i128,
    value: &BytesN<32>,
    blinding: &BytesN<32>,
) -> Vec<BytesN<32>> {
    let mut inputs = Vec::new(env);
    inputs.push_back(public_input_from_i128(env, amount));
    inputs.push_back(value.clone());
    inputs.push_back(blinding.clone());
    inputs
}

// ── The lookup itself (contracts/privacy/src/lib.rs:140) ─────────────────

#[test]
fn admin_lookup_returns_the_constructor_admin_when_present() {
    let s = setup();
    s.env.as_contract(&s.contract, || {
        assert_eq!(admin(&s.env), Ok(s.admin.clone()));
    });
}

#[test]
fn admin_lookup_returns_typed_not_initialized_when_the_key_is_absent() {
    let s = setup();
    s.evict_admin();
    // Typed error, not a host trap: the `Result` arm is asserted directly on
    // the lookup that used to carry `.expect("initialized in the constructor")`.
    s.env.as_contract(&s.contract, || {
        assert_eq!(admin(&s.env), Err(Error::NotInitialized));
    });
}

// ── Pre-initialisation ───────────────────────────────────────────────────

#[test]
fn set_verifying_key_before_initialisation_returns_not_initialized() {
    let s = setup();
    s.evict_admin();
    assert_eq!(
        s.client.try_set_verifying_key(&valid_vk(&s.env, 3)),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn create_escrow_before_initialisation_returns_not_initialized() {
    let s = setup();
    s.evict_admin();
    assert_eq!(
        s.client.try_create_escrow(&value_n32(&s.env, 7), &50),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn verify_escrow_before_initialisation_returns_not_initialized() {
    let s = setup();
    s.evict_admin();
    // No escrow exists yet: the instance-level guard answers `NotInitialized`
    // ahead of the per-escrow `Ok(false)` arm, proving it runs first.
    let inputs = inputs_with_commitment(&s.env, 150, &value_n32(&s.env, 9), &value_n32(&s.env, 8));
    assert_eq!(
        s.client
            .try_verify_escrow(&0u64, &valid_proof(&s.env), &inputs),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn get_admin_before_initialisation_returns_not_initialized() {
    let s = setup();
    s.evict_admin();
    assert_eq!(s.client.try_get_admin(), Err(Ok(Error::NotInitialized)));
}

// ── Post-eviction on a configured contract ───────────────────────────────

#[test]
fn eviction_of_the_admin_entry_upgrades_working_calls_to_not_initialized() {
    let s = setup();

    // A fully configured contract that behaves exactly as before: install the
    // key, create an escrow, and verify it with a valid proof.
    let vk = valid_vk(&s.env, 3);
    s.client.set_verifying_key(&vk);
    let value = value_n32(&s.env, 9);
    let blinding = value_n32(&s.env, 8);
    let commitment = commit(&s.env, &value, &blinding);
    let id = s.client.create_escrow(&commitment, &100);
    let inputs = inputs_with_commitment(&s.env, 150, &value, &blinding);
    assert!(s.client.verify_escrow(&id, &valid_proof(&s.env), &inputs));
    assert_eq!(
        s.client.get_escrow(&id).unwrap().status,
        EscrowStatus::Verified
    );

    // The admin entry is evicted. Every admin-guarded call now surfaces the
    // typed error instead of trapping, while the escrow state survives.
    s.evict_admin();
    assert_eq!(
        s.client.try_set_verifying_key(&vk),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        s.client.try_create_escrow(&value_n32(&s.env, 7), &50),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        s.client
            .try_verify_escrow(&id, &valid_proof(&s.env), &inputs),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(s.client.get_verifying_key(), vk);
    assert_eq!(
        s.client.get_escrow(&id).unwrap().status,
        EscrowStatus::Verified
    );
}
