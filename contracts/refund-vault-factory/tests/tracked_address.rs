//! Tracked-address branch tests (issue #654).
//!
//! The deploy path consults the factory's private `is_tracked_vault` predicate
//! on the *derived* vault address before registering anything: a salt whose
//! derived address is already in the registry must take the collision branch
//! (the documented outcome is [`Error::SaltCollision`]) instead of
//! double-registering, and `next_salt`'s counter must advance exactly once per
//! allocation. Both private helpers are reached only through the public deploy
//! API — `deploy_vault` for the counter-derived salt family, and
//! `deploy_for_participants` (same `create_vault` path, deterministic
//! participant salt) to re-derive an address the registry already holds; a
//! counter-derived `deploy_vault` salt can never repeat, so the tracked branch
//! is only reachable through a salt-bearing deploy. The harness style is
//! reused from `factory_tests.rs` (no new fixture module): the factory deploys
//! the real `refund_vault` wasm, which must be built first — see the README's
//! "Build and test" section and `.github/workflows/ci.yml`.

#![cfg(test)]

use accensa_common::VaultInit;
use refund_vault_factory::{Error, RefundVaultFactory, RefundVaultFactoryClient};
use soroban_sdk::{testutils::Address as _, vec, Address, Env, Vec};

mod vault_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/refund_vault.wasm");
}

struct Ctx {
    env: Env,
    factory: RefundVaultFactoryClient<'static>,
    admin: Address,
    merchant: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();
    // Same test-only budget raise as `factory_tests.rs`: instantiating the
    // ~139KB vault wasm exceeds the default 100M test budget on its own.
    env.cost_estimate()
        .budget()
        .reset_limits(500_000_000, 500_000_000);

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);
    let wasm_hash = env.deployer().upload_contract_wasm(vault_wasm::WASM);
    let factory_id = env.register(
        RefundVaultFactory,
        (admin.clone(), wasm_hash, None::<Address>, None::<Address>),
    );
    let factory = RefundVaultFactoryClient::new(&env, &factory_id);

    Ctx {
        env,
        factory,
        admin,
        merchant,
    }
}

fn token_for(env: &Env, admin: &Address) -> Address {
    env.register_stellar_asset_contract_v2(admin.clone())
        .address()
}

fn vault_init(merchant: &Address, token: &Address) -> VaultInit {
    VaultInit {
        merchant: merchant.clone(),
        token: token.clone(),
        time_policy: None,
        vdf_policy: None,
        fee_bps: 0,
        fee_recipient: None,
        refund_window: 100,
        deadline: 0,
        vdf_delay: 0,
    }
}

/// No element of the registry may appear twice.
fn assert_no_duplicates(vaults: &Vec<Address>) {
    for (i, vault) in vaults.iter().enumerate() {
        let mut seen_before = 0;
        for (j, earlier) in vaults.iter().enumerate() {
            if j < i && earlier == vault {
                seen_before += 1;
            }
        }
        assert_eq!(
            seen_before, 0,
            "vault address {vault:?} is registered more than once"
        );
    }
}

#[test]
fn first_deploy_allocates_a_salt_and_registers_the_derived_address() {
    let ctx = setup();
    let token = token_for(&ctx.env, &ctx.admin);
    assert_eq!(ctx.factory.get_next_salt(), 0, "counter starts at zero");
    assert_eq!(
        ctx.factory.get_vaults(),
        Vec::new(&ctx.env),
        "registry starts empty"
    );

    let vault = ctx.factory.deploy_vault(&vault_init(&ctx.merchant, &token));

    assert_eq!(
        ctx.factory.get_next_salt(),
        1,
        "the first deployment must allocate exactly one salt"
    );
    assert_eq!(
        ctx.factory.get_vaults(),
        vec![&ctx.env, vault.clone()],
        "the derived address must be registered exactly once"
    );
}

#[test]
fn a_second_request_for_an_already_tracked_address_takes_the_collision_branch() {
    let ctx = setup();
    let token = token_for(&ctx.env, &ctx.admin);
    let buyer = Address::generate(&ctx.env);

    // First request registers the derived address.
    let first = ctx
        .factory
        .deploy_for_participants(&vault_init(&ctx.merchant, &token), &buyer);
    let vaults_after_first = ctx.factory.get_vaults();
    let salt_after_first = ctx.factory.get_next_salt();

    // A second request with the same participants derives the very same
    // address; the tracked-address branch must reject it with the documented
    // error rather than deploying/aliasing again.
    assert_eq!(
        ctx.factory
            .try_deploy_for_participants(&vault_init(&ctx.merchant, &token), &buyer),
        Err(Ok(Error::SaltCollision)),
        "re-deriving a tracked address must take the collision branch"
    );

    assert_eq!(
        ctx.factory.get_vaults(),
        vaults_after_first,
        "the collision attempt must not register a duplicate (or any new entry)"
    );
    assert!(
        ctx.factory.get_vaults().contains(&first),
        "the originally registered address stays tracked"
    );
    assert_eq!(
        ctx.factory.get_next_salt(),
        salt_after_first,
        "the collision attempt must not allocate another salt"
    );

    let vaults = ctx.factory.get_vaults();
    assert_no_duplicates(&vaults);
}

#[test]
fn two_merchants_salt_counters_stay_independent() {
    let ctx = setup();
    let merchant_b = Address::generate(&ctx.env);
    let token = token_for(&ctx.env, &ctx.admin);

    let a1 = ctx.factory.deploy_vault(&vault_init(&ctx.merchant, &token));
    assert_eq!(ctx.factory.get_next_salt(), 1);

    let a2 = ctx.factory.deploy_vault(&vault_init(&ctx.merchant, &token));
    assert_eq!(
        ctx.factory.get_next_salt(),
        2,
        "each allocation advances the counter exactly once, whatever the merchant"
    );

    let b1 = ctx.factory.deploy_vault(&vault_init(&merchant_b, &token));
    assert_eq!(
        ctx.factory.get_next_salt(),
        3,
        "the second merchant's allocations draw from the same once-per-allocation counter"
    );

    // Independent salt families: neither merchant can land on the other's
    // addresses, and repeated deployments never repeat within a family.
    assert_ne!(a1, a2, "one merchant's deployments must keep advancing");
    assert_ne!(a1, b1, "merchants must not share salt families");
    assert_ne!(a2, b1, "merchants must not share salt families");

    let vaults = ctx.factory.get_vaults();
    assert_eq!(
        vaults.len(),
        3,
        "every deployment is registered exactly once"
    );
    assert!(vaults.contains(&a1) && vaults.contains(&a2) && vaults.contains(&b1));
    assert_no_duplicates(&vaults);
}
