//! Emergency pause circuit breaker tests.
//!
//! Tests that exercise the account's authorization run without
//! `mock_all_auths()` wherever possible, so the host runs the real
//! `__check_auth` against hand-built delegated-signer auth entries.

extern crate std;

use soroban_sdk::{
    auth::{Context, ContractContext},
    contract, contractimpl,
    testutils::{Address as _, Events},
    vec, Address, BytesN, Env, IntoVal, Map, Symbol, Val,
};

use crate::testutils::make_auth_entry_with_nonce;
use crate::{Error, MultisigAccount, MultisigAccountClient};

/// Stand-in for a contract the multisig administers (e.g. a vault): its only
/// function demands the admin's authorization, like `withdraw` would.
#[contract]
struct Target;

#[contractimpl]
impl Target {
    pub fn dispatch(_env: Env, admin: Address) {
        admin.require_auth();
    }
}

struct Setup {
    env: Env,
    account: Address,
    target: Address,
    s1: Address,
    s2: Address,
}

/// Two signers, threshold 2, plus a `Target` administered by the account.
fn setup() -> Setup {
    let env = Env::default();
    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);
    let account = env.register(MultisigAccount, (vec![&env, s1.clone(), s2.clone()], 2u32));
    let target = env.register(Target, ());
    Setup {
        env,
        account,
        target,
        s1,
        s2,
    }
}

impl Setup {
    fn client(&self) -> MultisigAccountClient<'_> {
        MultisigAccountClient::new(&self.env, &self.account)
    }

    fn target_client(&self) -> TargetClient<'_> {
        TargetClient::new(&self.env, &self.target)
    }

    /// Authorize one call to `fn_name` on `contract` on behalf of the
    /// account, carrying `delegates` as signers.
    fn authorize(
        &self,
        contract: &Address,
        fn_name: &str,
        args: &[Val],
        delegates: &[Address],
        nonce: i64,
    ) {
        self.env.set_auths(&[make_auth_entry_with_nonce(
            &self.env,
            &self.account,
            contract,
            fn_name,
            args,
            delegates,
            nonce,
        )]);
    }

    /// Pause through the account's own threshold authorization.
    fn pause_by_threshold(&self, nonce: i64) {
        let args = [self.account.into_val(&self.env)];
        let delegates = [self.s1.clone(), self.s2.clone()];
        self.authorize(&self.account, "pause", &args, &delegates, nonce);
        self.client().pause(&self.account);
    }

    /// Call `__check_auth` directly so the account's own error code is
    /// observable (through a real call the host wraps it as an auth error).
    fn check_auth(&self, contexts: soroban_sdk::Vec<Context>) -> Result<(), Error> {
        self.env
            .try_invoke_contract_check_auth::<Error>(
                &self.account,
                &BytesN::from_array(&self.env, &[0; 32]),
                ().into_val(&self.env),
                &contexts,
            )
            .map_err(|e| e.expect("__check_auth returned a non-contract error"))
    }

    fn context(&self, contract: &Address, fn_name: &str) -> Context {
        Context::Contract(ContractContext {
            contract: contract.clone(),
            fn_name: Symbol::new(&self.env, fn_name),
            args: vec![&self.env],
        })
    }
}

fn event_data(env: &Env, by: &Address) -> Map<Val, Val> {
    let mut m = Map::new(env);
    m.set(Symbol::new(env, "by").into_val(env), by.into_val(env));
    m
}

// ── Authorization of pause / unpause ────────────────────────────────────────

#[test]
fn threshold_signers_can_pause_and_unpause() {
    let t = setup();
    assert!(!t.client().is_paused());

    t.pause_by_threshold(1);
    assert!(t.client().is_paused());

    // Unpausing must still be authorizable while paused.
    let args = [t.account.into_val(&t.env)];
    t.authorize(
        &t.account,
        "unpause",
        &args,
        &[t.s1.clone(), t.s2.clone()],
        2,
    );
    t.client().unpause(&t.account);
    assert!(!t.client().is_paused());
}

#[test]
fn pause_below_threshold_is_rejected() {
    let t = setup();
    let args = [t.account.into_val(&t.env)];
    t.authorize(&t.account, "pause", &args, core::slice::from_ref(&t.s1), 1);

    assert!(t.client().try_pause(&t.account).is_err());
    assert!(!t.client().is_paused());
}

#[test]
fn pause_by_unregistered_delegates_is_rejected() {
    let t = setup();
    let args = [t.account.into_val(&t.env)];
    let outsiders = [Address::generate(&t.env), Address::generate(&t.env)];
    t.authorize(&t.account, "pause", &args, &outsiders, 1);

    assert!(t.client().try_pause(&t.account).is_err());
    assert!(!t.client().is_paused());
}

#[test]
fn unauthorized_caller_cannot_pause_or_unpause() {
    let t = setup();
    t.env.mock_all_auths();
    let stranger = Address::generate(&t.env);

    // Even with its own signature, a stranger is not a pause authority.
    assert_eq!(
        t.client().try_pause(&stranger),
        Err(Ok(Error::Unauthorized))
    );
    assert!(!t.client().is_paused());

    t.client().pause(&t.account);
    assert_eq!(
        t.client().try_unpause(&stranger),
        Err(Ok(Error::Unauthorized))
    );
    assert!(t.client().is_paused());
}

#[test]
fn signer_cannot_pause_alone_by_naming_itself() {
    let t = setup();
    t.env.mock_all_auths();
    // A registered signer is not the account: one signer must not be able to
    // bypass the threshold by passing its own address as `caller`.
    assert_eq!(t.client().try_pause(&t.s1), Err(Ok(Error::Unauthorized)));
}

#[test]
fn guardian_can_pause_and_unpause() {
    let t = setup();
    let guardian = Address::generate(&t.env);
    t.env.mock_all_auths();
    t.client().set_guardian(&Some(guardian.clone()));
    assert_eq!(t.client().get_guardian(), Some(guardian.clone()));

    t.client().pause(&guardian);
    assert!(t.client().is_paused());
    t.client().unpause(&guardian);
    assert!(!t.client().is_paused());
}

#[test]
fn guardian_pause_requires_guardian_signature() {
    let t = setup();
    let guardian = Address::generate(&t.env);
    t.env.mock_all_auths();
    t.client().set_guardian(&Some(guardian.clone()));

    // Drop the mocks: naming the guardian without its signature must fail.
    t.env.set_auths(&[]);
    assert!(t.client().try_pause(&guardian).is_err());
    assert!(!t.client().is_paused());
}

#[test]
fn cleared_guardian_loses_pause_authority() {
    let t = setup();
    let guardian = Address::generate(&t.env);
    t.env.mock_all_auths();
    t.client().set_guardian(&Some(guardian.clone()));
    t.client().set_guardian(&None);

    assert_eq!(t.client().get_guardian(), None);
    assert_eq!(
        t.client().try_pause(&guardian),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn set_guardian_requires_threshold() {
    let t = setup();
    let guardian = Some(Address::generate(&t.env));
    let args = [guardian.into_val(&t.env)];

    t.authorize(
        &t.account,
        "set_guardian",
        &args,
        core::slice::from_ref(&t.s1),
        1,
    );
    assert!(t.client().try_set_guardian(&guardian).is_err());
    assert_eq!(t.client().get_guardian(), None);

    t.authorize(
        &t.account,
        "set_guardian",
        &args,
        &[t.s1.clone(), t.s2.clone()],
        2,
    );
    t.client().set_guardian(&guardian);
    assert_eq!(t.client().get_guardian(), guardian);
}

// ── Behaviour while paused ──────────────────────────────────────────────────

#[test]
fn dispatch_is_blocked_while_paused() {
    let t = setup();
    let args = [t.account.into_val(&t.env)];
    let both = [t.s1.clone(), t.s2.clone()];

    // Sanity: the call goes through while running.
    t.authorize(&t.target, "dispatch", &args, &both, 1);
    t.target_client().dispatch(&t.account);

    t.pause_by_threshold(2);

    // Same fully-signed authorization, now refused.
    t.authorize(&t.target, "dispatch", &args, &both, 3);
    assert!(t.target_client().try_dispatch(&t.account).is_err());
}

#[test]
fn dispatch_resumes_after_unpause() {
    let t = setup();
    let args = [t.account.into_val(&t.env)];
    let both = [t.s1.clone(), t.s2.clone()];

    t.pause_by_threshold(1);
    t.authorize(&t.account, "unpause", &args, &both, 2);
    t.client().unpause(&t.account);

    t.authorize(&t.target, "dispatch", &args, &both, 3);
    t.target_client().dispatch(&t.account);
}

#[test]
fn check_auth_returns_paused_for_outbound_contexts() {
    let t = setup();
    t.env.mock_all_auths();
    t.client().pause(&t.account);

    let outbound = t.context(&t.target, "dispatch");
    assert_eq!(
        t.check_auth(vec![&t.env, outbound.clone()]),
        Err(Error::Paused)
    );

    // An allowed context bundled with an outbound one does not smuggle it through.
    let unpause = t.context(&t.account, "unpause");
    assert_eq!(
        t.check_auth(vec![&t.env, unpause, outbound]),
        Err(Error::Paused)
    );

    // A function of the same name on another contract is not allowed either.
    let foreign_pause = t.context(&t.target, "pause");
    assert_eq!(
        t.check_auth(vec![&t.env, foreign_pause]),
        Err(Error::Paused)
    );

    // Nor is any other function on the account itself.
    let other = t.context(&t.account, "__constructor");
    assert_eq!(t.check_auth(vec![&t.env, other]), Err(Error::Paused));
}

#[test]
fn check_auth_allows_recovery_contexts_while_paused() {
    let t = setup();
    t.env.mock_all_auths();
    t.client().pause(&t.account);

    for name in [
        "pause",
        "unpause",
        "set_guardian",
        "rotate_signers_and_threshold",
    ] {
        // Passes the pause gate. A direct invocation carries no delegated
        // signers, so it still fails afterwards — just not with `Paused`.
        let res = t.env.try_invoke_contract_check_auth::<Error>(
            &t.account,
            &BytesN::from_array(&t.env, &[0; 32]),
            ().into_val(&t.env),
            &vec![&t.env, t.context(&t.account, name)],
        );
        assert_ne!(
            res,
            Err(Ok(Error::Paused)),
            "{name} should pass the pause gate"
        );
    }
}

#[test]
fn guardian_can_be_replaced_by_threshold_while_paused() {
    let t = setup();
    t.pause_by_threshold(1);

    let guardian = Some(Address::generate(&t.env));
    let args = [guardian.into_val(&t.env)];
    t.authorize(
        &t.account,
        "set_guardian",
        &args,
        &[t.s1.clone(), t.s2.clone()],
        2,
    );
    t.client().set_guardian(&guardian);
    assert_eq!(t.client().get_guardian(), guardian);
}

#[test]
fn signer_rotation_works_while_paused() {
    let t = setup();
    t.env.mock_all_auths();
    t.client().pause(&t.account);

    let s3 = Address::generate(&t.env);
    t.client().rotate_signers_and_threshold(
        &vec![&t.env, s3.clone()],
        &vec![&t.env, t.s1.clone()],
        &2,
    );
    assert!(t.client().is_signer(&s3));
    assert!(!t.client().is_signer(&t.s1));
    assert!(t.client().is_paused());
}

#[test]
fn read_only_queries_work_while_paused() {
    let t = setup();
    t.pause_by_threshold(1);

    assert!(t.client().is_paused());
    assert_eq!(t.client().get_threshold(), 2);
    assert!(t.client().is_signer(&t.s1));
    assert!(t.client().is_signer(&t.s2));
    assert_eq!(t.client().get_guardian(), None);
}

// ── Events ──────────────────────────────────────────────────────────────────

#[test]
fn pause_and_unpause_emit_events() {
    let t = setup();
    let guardian = Address::generate(&t.env);
    t.env.mock_all_auths();
    t.client().set_guardian(&Some(guardian.clone()));
    let ledger = t.env.ledger().sequence();

    t.client().pause(&guardian);
    assert_eq!(
        t.env.events().all().filter_by_contract(&t.account),
        vec![
            &t.env,
            (
                t.account.clone(),
                (Symbol::new(&t.env, "paused_event"), ledger).into_val(&t.env),
                event_data(&t.env, &guardian).into_val(&t.env),
            )
        ]
    );

    t.client().unpause(&t.account);
    assert_eq!(
        t.env.events().all().filter_by_contract(&t.account),
        vec![
            &t.env,
            (
                t.account.clone(),
                (Symbol::new(&t.env, "unpaused_event"), ledger).into_val(&t.env),
                event_data(&t.env, &t.account).into_val(&t.env),
            )
        ]
    );
}

#[test]
fn rejected_pause_emits_no_event() {
    let t = setup();
    t.env.mock_all_auths();
    let _ = t.client().try_pause(&Address::generate(&t.env));
    let none: soroban_sdk::Vec<(Address, soroban_sdk::Vec<Val>, Val)> = vec![&t.env];
    assert_eq!(t.env.events().all().filter_by_contract(&t.account), none);
}
