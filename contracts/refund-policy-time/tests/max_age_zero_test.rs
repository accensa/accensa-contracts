//! `max_report_age = 0` trusts only a report stamped at the claim's own second
//! (issue #979).
//!
//! The crate documents `window` and `deadline` with `0` meaning "disabled"
//! (`src/lib.rs:8-11`), so a merchant who reads `max_report_age: 0` the same
//! way expects the age check to switch off. It does the opposite:
//! `is_usable` compares the report's age against the field literally, so zero
//! is the strictest possible gate — a report from any earlier second is
//! ignored and the oracle's verdict is discarded in favour of the clock.
//!
//! Nothing pinned that. The freshness cases in `src/oracle_test.rs` all run at
//! `MAX_AGE = 3600`, so loosening `<=` to `<` there moved no test at all. These
//! two assertions are the pair that does: same params, same oracle answer, one
//! second apart.

use accensa_common::{Error, PolicyContext, RefundPolicyClient};
use refund_policy_time::oracle::{DeliveryReport, DeliveryStatus, TimeOraclePolicyParams};
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Ledger, xdr::ToXdr, Address, BytesN, Env,
};

const NOW: u64 = 50_000;
/// A window short enough that a claim paid before it started is expired, so an
/// admitted verdict can only have come from the oracle.
const WINDOW: u32 = 100;
const PAID_AT: u32 = 1;

#[contracttype]
enum MockKey {
    Report,
}

/// Minimal delivery oracle: answers with whatever report the test stored.
#[contract]
struct MockDeliveryOracle;

#[contractimpl]
impl MockDeliveryOracle {
    pub fn set_report(env: Env, report: DeliveryReport) {
        env.storage().instance().set(&MockKey::Report, &report);
    }

    pub fn get_delivery_status(env: Env, _payment_ref: BytesN<32>) -> DeliveryReport {
        env.storage().instance().get(&MockKey::Report).unwrap()
    }
}

struct Fixture {
    env: Env,
    policy: Address,
    oracle: Address,
    payment_ref: BytesN<32>,
}

/// Set up a policy and an oracle with `max_report_age: 0` and a claim that is
/// already outside the window.
fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.sequence_number = NOW as u32;
        li.timestamp = NOW;
    });
    Fixture {
        oracle: env.register(MockDeliveryOracle, ()),
        policy: env.register(refund_policy_time::TimePolicy, ()),
        payment_ref: BytesN::from_array(&env, &[21u8; 32]),
        env,
    }
}

impl Fixture {
    fn report(&self, status: DeliveryStatus, timestamp: u64) {
        MockDeliveryOracleClient::new(&self.env, &self.oracle).set_report(&DeliveryReport {
            payment_ref: self.payment_ref.clone(),
            status,
            timestamp,
            proof: BytesN::from_array(&self.env, &[0xEE; 32]),
        });
    }

    /// `max_report_age` is the field under test; the gates are set so that the
    /// only way to be admitted is by the oracle.
    fn params(&self, max_report_age: u64) -> soroban_sdk::Bytes {
        TimeOraclePolicyParams {
            window: WINDOW,
            deadline: 0,
            oracle: self.oracle.clone(),
            max_report_age,
        }
        .to_xdr(&self.env)
    }

    fn evaluate(&self, max_report_age: u64) -> Result<(), Error> {
        let ctx = PolicyContext {
            payment_ref: self.payment_ref.clone(),
            amount: 100,
            paid_at_ledger: PAID_AT,
            current_ledger: self.env.ledger().sequence(),
            timestamp: self.env.ledger().timestamp(),
            vdf_proof: None,
        };
        match RefundPolicyClient::new(&self.env, &self.policy)
            .try_evaluate(&self.params(max_report_age), &ctx)
        {
            Ok(Ok(())) => Ok(()),
            Err(Ok(error)) => Err(error),
            other => panic!("expected a policy verdict, got {other:?}"),
        }
    }
}

#[test]
fn a_zero_age_gate_trusts_a_report_stamped_in_the_claim_second() {
    // `ctx.timestamp - report.timestamp == 0 == max_report_age`: usable, so
    // the oracle decides and the expired window is overridden.
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW);
    assert_eq!(fx.evaluate(0), Ok(()));
}

#[test]
fn a_zero_age_gate_ignores_a_report_one_second_older() {
    // One second of age is already past a zero allowance, so the oracle answer
    // is discarded and the claim is judged by the window it is outside of.
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW - 1);
    assert_eq!(fx.evaluate(0), Err(Error::WindowExpired));
}

#[test]
fn zero_is_the_strictest_age_not_a_disabled_check() {
    // The reading a merchant has to be warned about: the same one-second-old
    // report that `max_report_age: 0` ignores is trusted at `1`.
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW - 1);
    assert_eq!(fx.evaluate(0), Err(Error::WindowExpired));
    assert_eq!(fx.evaluate(1), Ok(()));
}

#[test]
fn a_future_dated_report_is_ignored_at_any_age() {
    // Freshness is one-sided: a report stamped after the claim is not "zero
    // seconds old", it is not usable at all, so the window answers.
    let fx = setup();
    fx.report(DeliveryStatus::Lost, NOW + 1);
    assert_eq!(fx.evaluate(0), Err(Error::WindowExpired));
    assert_eq!(fx.evaluate(3_600), Err(Error::WindowExpired));
}
