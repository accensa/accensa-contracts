//! Oracle-assisted dispute resolution tests (issue #426).

use super::*;
use crate::oracle::{
    DeliveryReport, DeliveryStatus, OracleResolutionApplied, TimeOraclePolicyParams,
};
use accensa_common::RefundPolicyClient;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events as _, Ledger},
    xdr::ToXdr,
    Address, BytesN, Env, Event,
};

// ── Mock delivery oracle ─────────────────────────────────────────────────────

#[contracttype]
enum MockKey {
    Report,
    Broken,
}

/// Returns whatever report the test stored; traps when marked broken.
#[contract]
pub struct MockDeliveryOracle;

#[contractimpl]
impl MockDeliveryOracle {
    pub fn set_report(env: Env, report: DeliveryReport) {
        env.storage().instance().set(&MockKey::Report, &report);
    }

    pub fn set_broken(env: Env) {
        env.storage().instance().set(&MockKey::Broken, &true);
    }

    pub fn get_delivery_status(env: Env, _payment_ref: BytesN<32>) -> DeliveryReport {
        if env.storage().instance().has(&MockKey::Broken) {
            panic!("oracle offline");
        }
        env.storage().instance().get(&MockKey::Report).unwrap()
    }
}

// ── Fixture ──────────────────────────────────────────────────────────────────

const NOW: u64 = 50_000;
const MAX_AGE: u64 = 3_600;
const WINDOW: u32 = 100;

struct Fx {
    env: Env,
    policy: Address,
    oracle: Address,
    payment_ref: BytesN<32>,
}

fn setup() -> Fx {
    let env = Env::default();
    env.ledger().with_mut(|li| {
        li.sequence_number = 1_000;
        li.timestamp = NOW;
    });
    Fx {
        policy: env.register(TimePolicy, ()),
        oracle: env.register(MockDeliveryOracle, ()),
        payment_ref: BytesN::from_array(&env, &[9u8; 32]),
        env,
    }
}

impl Fx {
    fn report(&self, status: DeliveryStatus, timestamp: u64) {
        MockDeliveryOracleClient::new(&self.env, &self.oracle).set_report(&DeliveryReport {
            payment_ref: self.payment_ref.clone(),
            status,
            timestamp,
            proof: BytesN::from_array(&self.env, &[0xAB; 32]),
        });
    }

    fn params_for(&self, oracle: &Address) -> Bytes {
        TimeOraclePolicyParams {
            window: WINDOW,
            deadline: 0,
            oracle: oracle.clone(),
            max_report_age: MAX_AGE,
        }
        .to_xdr(&self.env)
    }

    /// Evaluate a claim for a payment made at `paid_at_ledger`.
    fn evaluate_with(&self, oracle: &Address, paid_at_ledger: u32) -> Result<(), Error> {
        let ctx = PolicyContext {
            payment_ref: self.payment_ref.clone(),
            amount: 100,
            paid_at_ledger,
            current_ledger: self.env.ledger().sequence(),
            timestamp: self.env.ledger().timestamp(),
            vdf_proof: None,
        };
        match RefundPolicyClient::new(&self.env, &self.policy)
            .try_evaluate(&self.params_for(oracle), &ctx)
        {
            Ok(Ok(())) => Ok(()),
            Err(Ok(e)) => Err(e),
            other => panic!("unexpected result {other:?}"),
        }
    }

    fn evaluate(&self, paid_at_ledger: u32) -> Result<(), Error> {
        self.evaluate_with(&self.oracle, paid_at_ledger)
    }

    /// Claim paid long ago: outside the standard window.
    fn late(&self) -> Result<(), Error> {
        self.evaluate(1_000 - WINDOW - 1)
    }

    /// Claim paid recently: inside the standard window.
    fn timely(&self) -> Result<(), Error> {
        self.evaluate(1_000 - 10)
    }
}

// ── Delivered / pending / lost ───────────────────────────────────────────────

#[test]
fn lost_parcel_is_refunded_even_outside_the_window() {
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW - 60);
    assert_eq!(fx.late(), Ok(()));

    let expected = OracleResolutionApplied {
        payment_ref: fx.payment_ref.clone(),
        oracle: fx.oracle.clone(),
        status: DeliveryStatus::Lost,
        reported_at: NOW - 60,
        proof: BytesN::from_array(&fx.env, &[0xAB; 32]),
    };
    assert_eq!(
        fx.env.events().all().filter_by_contract(&fx.policy),
        [expected.to_xdr(&fx.env, &fx.policy)]
    );
}

#[test]
fn delivered_parcel_denies_refund_even_inside_the_window() {
    let fx = setup();
    fx.report(DeliveryStatus::Delivered, NOW - 60);
    assert_eq!(fx.timely(), Err(Error::OraclePolicyDenied));
}

#[test]
fn pending_parcel_falls_back_to_the_window() {
    let fx = setup();
    fx.report(DeliveryStatus::Pending, NOW - 60);
    assert_eq!(fx.timely(), Ok(()));
    assert_eq!(fx.late(), Err(Error::WindowExpired));
    assert!(fx
        .env
        .events()
        .all()
        .filter_by_contract(&fx.policy)
        .events()
        .is_empty());
}

// ── Freshness ────────────────────────────────────────────────────────────────

#[test]
fn stale_report_is_ignored() {
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW - MAX_AGE - 1);
    assert_eq!(fx.late(), Err(Error::WindowExpired));

    fx.report(DeliveryStatus::Delivered, NOW - MAX_AGE - 1);
    assert_eq!(fx.timely(), Ok(()));
}

#[test]
fn report_exactly_at_max_age_is_trusted() {
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW - MAX_AGE);
    assert_eq!(fx.late(), Ok(()));
}

#[test]
fn future_dated_report_is_ignored() {
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW + 1);
    assert_eq!(fx.late(), Err(Error::WindowExpired));
}

#[test]
fn report_for_another_payment_is_ignored() {
    let fx = setup();
    MockDeliveryOracleClient::new(&fx.env, &fx.oracle).set_report(&DeliveryReport {
        payment_ref: BytesN::from_array(&fx.env, &[1u8; 32]),
        status: DeliveryStatus::Lost,
        timestamp: NOW,
        proof: BytesN::from_array(&fx.env, &[0u8; 32]),
    });
    assert_eq!(fx.late(), Err(Error::WindowExpired));
}

// ── Unreachable oracle ───────────────────────────────────────────────────────

#[test]
fn trapping_oracle_falls_back_to_the_window() {
    let fx = setup();
    MockDeliveryOracleClient::new(&fx.env, &fx.oracle).set_broken();
    assert_eq!(fx.timely(), Ok(()));
    assert_eq!(fx.late(), Err(Error::WindowExpired));
}

#[test]
fn nonexistent_oracle_falls_back_to_the_window() {
    let fx = setup();
    let missing = Address::generate(&fx.env);
    assert_eq!(fx.evaluate_with(&missing, 1_000 - 10), Ok(()));
    assert_eq!(
        fx.evaluate_with(&missing, 1_000 - WINDOW - 1),
        Err(Error::WindowExpired)
    );
}

// ── Backward compatibility ───────────────────────────────────────────────────

#[test]
fn plain_time_params_still_decode_without_an_oracle() {
    let fx = setup();
    let params = TimePolicyParams {
        window: WINDOW,
        deadline: 0,
    }
    .to_xdr(&fx.env);
    let ctx = PolicyContext {
        payment_ref: fx.payment_ref.clone(),
        amount: 100,
        paid_at_ledger: 1_000 - WINDOW - 1,
        current_ledger: 1_000,
        timestamp: NOW,
        vdf_proof: None,
    };
    assert_eq!(
        RefundPolicyClient::new(&fx.env, &fx.policy).try_evaluate(&params, &ctx),
        Err(Ok(Error::WindowExpired))
    );
}
