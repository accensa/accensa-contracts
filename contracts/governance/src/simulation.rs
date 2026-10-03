//! Governance proposal simulation hooks (issue #483).
//!
//! A proposal that reverts when executed still consumes its quorum: members
//! vote, the window elapses, and only then does [`crate::Governance::execute`]
//! fail. Simulation closes that gap by making every proposal carry a
//! **simulation report** produced before the vote.
//!
//! # What "dry-run" can mean on Soroban
//!
//! Soroban has no sandboxed cross-contract call: an `invoke_contract` inside
//! [`crate::Governance::propose`] would *actually run* the proposal's
//! effects. A true in-contract dry-run is therefore not expressible — the
//! same reason the host rejects contract re-entry outright (see the
//! `#[ignore]`d `set_treasury_token` test). The issue's second suggested
//! mechanism is the one that maps onto this reality: **off-chain simulation
//! verification via oracle**.
//!
//! # Mechanism
//!
//! 1. A **simulator contract** (registered by a member with
//!    `set_simulation_config`) implements the [`ProposalSimulator`]
//!    interface: it dry-runs `target::function(args)` off-chain (or via the
//!    RPC's `simulateTransaction` against the real calldata) and returns a
//!    `u32` outcome, `0` = would not revert ([`SIM_OK`]).
//! 2. The proposer submits the proposal with `propose_with_simulation`,
//!    attaching a [`SimulationReport`] that names the simulator, carries the
//!    outcome, and carries `sim_hash` — the SHA-256 of the canonical
//!    [`sim_payload`] binding the report to *this* proposal id and *this*
//!    exact calldata. The contract re-derives the payload hash, so a report
//!    cannot be transplanted onto a different proposal.
//! 3. The contract enforces:
//!    - the report's simulator is the registered one
//!      ([`Error::SimulationMismatch`]),
//!    - the binding hash matches ([`Error::SimulationMismatch`]),
//!    - the outcome is [`SIM_OK`] ([`Error::SimulationFailed`]) — a proposal
//!      guaranteed to revert is rejected at creation and can never reach a
//!      vote, satisfying the issue's "block proposals that revert during
//!      dry-run".
//!
//! # MVP cuts
//!
//! - `required = false` (the default) keeps the plain
//!   [`crate::Governance::propose`] path available; with `required = true`
//!   every new proposal must carry a successful report.
//! - The registered simulator is one address, set by any member (membership
//!   auth). A production body should rotate it only through an executed
//!   proposal.
//! - The trust anchor is **registration**: a report is accepted only when it
//!   names the registered simulator, and only members can register one. The
//!   binding hash proves *what* was simulated; it cannot prove the simulator
//!   was honest — that is what member-gated registration (and eventually a
//!   governance-elected simulator) is for. Note that the simulator's
//!   `require_auth` cannot be used as a co-signature here: Soroban
//!   auto-authorizes a contract address only when it is the direct caller,
//!   and the proposer — not the simulator — calls this contract.

use crate::{DataKey, Error, ProposalCreated};
use soroban_sdk::{
    contractclient, contracttype, xdr::ToXdr, Address, Bytes, BytesN, Env, Symbol, Val, Vec,
};

/// The outcome code every successful simulation must report.
pub const SIM_OK: u32 = 0;

/// Domain separator binding a simulation report to one proposal.
const SIM_PAYLOAD_DOMAIN: &[u8] = b"accensa:proposal-sim:v1";

/// The interface a proposal-simulation oracle implements.
#[contractclient(name = "ProposalSimulatorClient")]
pub trait ProposalSimulator {
    /// Dry-run `target::function(args)` and return an outcome code:
    /// [`SIM_OK`] when the call would not revert, anything else is failure.
    fn simulate(env: Env, target: Address, function: Symbol, args: Vec<Val>) -> u32;
}

/// A simulator's off-chain dry-run result for one proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulationReport {
    /// The simulator contract that produced the report. Must be the
    /// registered simulator.
    pub simulator: Address,
    /// `0` ([`SIM_OK`]) when the dry-run did not revert.
    pub outcome: u32,
    /// SHA-256 of [`sim_payload`] for this proposal — binds the report to
    /// the exact calldata and proposal id.
    pub sim_hash: BytesN<32>,
}

/// Whether simulation is mandatory and which simulator accepts reports.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulationConfig {
    /// The registered simulator contract, if any.
    pub simulator: Option<Address>,
    /// Whether every new proposal must carry a successful report.
    pub required: bool,
}

/// Canonical bytes a report is bound to: the domain separator, this
/// governance instance, the proposal id the report will be stored under, and
/// the exact calldata. Both the simulator (off-chain) and this contract
/// (on-chain) hash the same construction, so reports cannot be replayed
/// against a different proposal or different arguments.
pub fn sim_payload(
    env: &Env,
    governance: &Address,
    proposal_id: u64,
    target: &Address,
    function: &Symbol,
    args: &Vec<Val>,
) -> Bytes {
    let mut buf = Bytes::from_slice(env, SIM_PAYLOAD_DOMAIN);
    buf.append(&governance.clone().to_xdr(env));
    buf.extend_from_slice(&proposal_id.to_be_bytes());
    buf.append(&target.clone().to_xdr(env));
    buf.append(&function.clone().to_xdr(env));
    // Arguments are hashed in their canonical XDR form, so any change to the
    // call — value, type, or order — invalidates the report.
    for arg in args.iter() {
        buf.append(&arg.to_xdr(env));
    }
    buf
}

/// Compute [`sim_payload`]'s SHA-256 digest.
pub fn sim_hash(
    env: &Env,
    governance: &Address,
    proposal_id: u64,
    target: &Address,
    function: &Symbol,
    args: &Vec<Val>,
) -> BytesN<32> {
    env.crypto()
        .sha256(&sim_payload(
            env,
            governance,
            proposal_id,
            target,
            function,
            args,
        ))
        .into()
}

fn config_key() -> DataKey {
    DataKey::SimulationConfig
}

fn report_key(proposal_id: u64) -> DataKey {
    DataKey::SimAttestation(proposal_id)
}

/// Read the stored simulation config, if any has been set.
pub(crate) fn config(env: &Env) -> Option<SimulationConfig> {
    env.storage().persistent().get(&config_key())
}

/// Whether every new proposal must carry a successful simulation report.
pub(crate) fn is_required(env: &Env) -> bool {
    config(env).is_some_and(|cfg| cfg.required)
}

/// Store the simulation config. Membership auth is checked by the entry
/// point; see the module docs for the trust notes.
pub(crate) fn set_config(
    env: &Env,
    simulator: Option<Address>,
    required: bool,
) -> Result<(), Error> {
    if required && simulator.is_none() {
        return Err(Error::SimulationNotConfigured);
    }
    let cfg = SimulationConfig {
        simulator,
        required,
    };
    env.storage().persistent().set(&config_key(), &cfg);
    Ok(())
}

/// Create a proposal carrying a simulation report. Mirrors
/// [`crate::Governance::propose`] exactly (membership, id allocation,
/// window, storage, TTL, event) and adds the report gates. When simulation
/// is required, every gate is mandatory; when it is not, a well-formed
/// report is still recorded for voters and indexers.
///
/// Returns the new proposal id.
pub(crate) fn propose_with_simulation(
    env: &Env,
    proposer: &Address,
    target: Address,
    function: Symbol,
    args: Vec<Val>,
    report: SimulationReport,
) -> Result<u64, Error> {
    // Membership, same as `propose`.
    env.storage()
        .persistent()
        .get::<_, u64>(&DataKey::MemberDeposit(proposer.clone()))
        .ok_or(Error::NotAMember)?;

    let cfg = config(env);
    // A submitted report must name the registered simulator, whether or not
    // simulation is mandatory — an unregistered report is never recorded.
    // (When simulation is mandatory with no simulator registered, the plain
    // `propose` path is already closed; this path fails closed too.)
    // Registration itself is member-gated (see `set_simulation_config`);
    // there is deliberately no `require_auth` on the simulator here —
    // Soroban auto-authorizes a contract address only when it is the direct
    // caller, and the proposer calls in.
    let registered = cfg
        .as_ref()
        .and_then(|c| c.simulator.clone())
        .ok_or(Error::SimulationNotConfigured)?;
    if report.simulator != registered {
        return Err(Error::SimulationMismatch);
    }

    // The proposal id the report will be stored under — allocated exactly
    // like `propose` allocates it.
    let id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::ProposalCount)
        .unwrap_or(0);
    let next_id = id + 1;

    // The report must be bound to this proposal and this calldata.
    let expected = sim_hash(
        env,
        &env.current_contract_address(),
        next_id,
        &target,
        &function,
        &args,
    );
    if report.sim_hash != expected {
        return Err(Error::SimulationMismatch);
    }

    // A proposal the simulator says will revert is rejected before it ever
    // exists — no vote can be wasted on it.
    if report.outcome != SIM_OK {
        return Err(Error::SimulationFailed);
    }

    let voting_period: u32 = env
        .storage()
        .instance()
        .get(&DataKey::VotingPeriod)
        .unwrap();
    let deadline_ledger = env.ledger().sequence() + voting_period;

    let proposal = crate::Proposal {
        proposer: proposer.clone(),
        target: target.clone(),
        function: function.clone(),
        args: args.clone(),
        yes_weight: 0,
        no_weight: 0,
        deadline_ledger,
        executed: false,
    };
    let key = DataKey::Proposal(next_id);
    env.storage().persistent().set(&key, &proposal);
    // Cover the voting window plus the ragequit window, exactly like
    // `propose` does.
    let ttl = voting_period
        .saturating_add(crate::ragequit::RAGEQUIT_WINDOW)
        .saturating_add(crate::PROPOSAL_TTL_GRACE);
    env.storage().persistent().extend_ttl(&key, ttl, ttl);

    // The report lives under its own key so the `Proposal` record's shape —
    // and every existing stored proposal — stays unchanged.
    let report_key = report_key(next_id);
    env.storage().persistent().set(&report_key, &report);
    env.storage().persistent().extend_ttl(&report_key, ttl, ttl);

    env.storage()
        .instance()
        .set(&DataKey::ProposalCount, &next_id);

    ProposalCreated {
        proposal_id: next_id,
        proposer: proposer.clone(),
        target,
        function,
        deadline_ledger,
    }
    .publish(env);

    Ok(next_id)
}

/// Read-only: the simulation report stored for `proposal_id`, if the
/// proposal was created through `propose_with_simulation`.
pub(crate) fn get_report(env: &Env, proposal_id: u64) -> Option<SimulationReport> {
    env.storage().persistent().get(&report_key(proposal_id))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::{Error, Governance, GovernanceClient};
    use soroban_sdk::{
        contract, contractimpl, symbol_short, testutils::Address as _, Address, Env, IntoVal,
        Symbol, Val, Vec,
    };

    /// Minimal governed target: `set_value` requires the stored admin's
    /// auth; `explode` always traps — the "guaranteed to revert" call.
    #[contract]
    struct Target;

    #[contractimpl]
    impl Target {
        pub fn init(env: Env, admin: Address) {
            env.storage()
                .instance()
                .set(&symbol_short!("admin"), &admin);
        }

        pub fn set_value(env: Env, value: u32) {
            let admin: Address = env
                .storage()
                .instance()
                .get(&symbol_short!("admin"))
                .unwrap();
            admin.require_auth();
            env.storage()
                .instance()
                .set(&symbol_short!("value"), &value);
        }

        pub fn get_value(env: Env) -> u32 {
            env.storage()
                .instance()
                .get(&symbol_short!("value"))
                .unwrap_or(0)
        }

        pub fn explode(_env: Env) {
            panic!("guaranteed revert");
        }
    }

    /// Stand-in simulator: reports success for `set_value`, failure for
    /// anything else. On a real deployment this contract dry-runs the
    /// calldata off-chain (or via `simulateTransaction`) instead of pattern
    /// matching — the on-chain side cannot execute the call for it.
    #[contract]
    struct MockSimulator;

    #[contractimpl]
    impl MockSimulator {
        pub fn simulate(env: Env, _target: Address, function: Symbol, _args: Vec<Val>) -> u32 {
            if function == Symbol::new(&env, "set_value") {
                SIM_OK
            } else {
                1
            }
        }
    }

    struct Harness {
        env: Env,
        gov: GovernanceClient<'static>,
        gov_id: Address,
        target: Address,
        simulator: Address,
        simulator_client: MockSimulatorClient<'static>,
        m1: Address,
        m2: Address,
    }

    /// Two members (weights 1/1, total 2), 60% quorum, 100-ledger window,
    /// mandatory simulation through `MockSimulator`.
    fn setup() -> Harness {
        let env = Env::default();
        env.mock_all_auths();

        let m1 = Address::generate(&env);
        let m2 = Address::generate(&env);
        let members = Vec::from_array(&env, [m1.clone(), m2.clone()]);
        let deposits = Vec::from_array(&env, [1u64, 1u64]);
        let gov_id = env.register(Governance, (members, deposits, 6000u32, 100u32));
        let gov = GovernanceClient::new(&env, &gov_id);

        let target_id = env.register(Target, ());
        TargetClient::new(&env, &target_id).init(&gov_id);

        let simulator = env.register(MockSimulator, ());
        // Enable mandatory simulation: any member may configure it.
        gov.set_simulation_config(&m1, &Some(simulator.clone()), &true);

        let simulator_client = MockSimulatorClient::new(&env, &simulator);

        Harness {
            env,
            gov,
            gov_id,
            target: target_id,
            simulator,
            simulator_client,
            m1,
            m2,
        }
    }

    /// Build a report exactly the way an off-chain simulator would: run the
    /// simulation through the simulator's client, then bind the outcome to
    /// this proposal's canonical payload.
    fn report_for(
        h: &Harness,
        proposal_id: u64,
        target: &Address,
        function: &Symbol,
        args: &Vec<Val>,
    ) -> SimulationReport {
        let outcome = h.simulator_client.simulate(target, function, args);
        SimulationReport {
            simulator: h.simulator.clone(),
            outcome,
            sim_hash: sim_hash(&h.env, &h.gov_id, proposal_id, target, function, args),
        }
    }

    fn set_value_args(env: &Env, value: u32) -> Vec<Val> {
        Vec::from_array(env, [value.into_val(env)])
    }

    #[test]
    fn simulated_proposal_is_created_and_stores_the_report() {
        let h = setup();
        let args = set_value_args(&h.env, 42);
        let function = Symbol::new(&h.env, "set_value");
        let report = report_for(&h, 1, &h.target, &function, &args);

        let id = h
            .gov
            .propose_with_simulation(&h.m1, &h.target, &function, &args, &report);
        assert_eq!(id, 1);

        let stored = h.gov.get_simulation_report(&id).unwrap();
        assert_eq!(stored, report);
        assert_eq!(stored.outcome, SIM_OK);

        // The proposal itself is a normal, votable proposal.
        let proposal = h.gov.get_proposal(&id);
        assert_eq!(proposal.target, h.target);
        assert_eq!(proposal.deadline_ledger, h.env.ledger().sequence() + 100);
    }

    #[test]
    fn simulation_is_mandatory_when_configured() {
        let h = setup();
        let args = set_value_args(&h.env, 1);
        let function = Symbol::new(&h.env, "set_value");

        // The plain propose path must be refused: every proposal needs a
        // report once `required` is on.
        assert_eq!(
            h.gov.try_propose(&h.m1, &h.target, &function, &args),
            Err(Ok(Error::SimulationRequired))
        );
    }

    #[test]
    fn guaranteed_to_fail_proposal_is_rejected_at_creation() {
        let h = setup();
        let args: Vec<Val> = Vec::from_array(&h.env, []);
        let function = Symbol::new(&h.env, "explode");

        // The simulator reports "would revert" for this calldata.
        let report = report_for(&h, 1, &h.target, &function, &args);
        assert_ne!(report.outcome, SIM_OK);

        assert_eq!(
            h.gov
                .try_propose_with_simulation(&h.m1, &h.target, &function, &args, &report),
            Err(Ok(Error::SimulationFailed))
        );
        // Nothing was created and no report was stored.
        assert_eq!(h.gov.get_proposal_count(), 0);
        assert_eq!(h.gov.get_simulation_report(&1), None);
    }

    #[test]
    fn report_is_bound_to_the_exact_calldata() {
        let h = setup();
        let function = Symbol::new(&h.env, "set_value");
        let args = set_value_args(&h.env, 7);
        let report = report_for(&h, 1, &h.target, &function, &args);

        // Replaying the same report for *different* arguments must fail:
        // the recomputed binding hash no longer matches.
        let tampered_args = set_value_args(&h.env, 8);
        assert_eq!(
            h.gov
                .try_propose_with_simulation(&h.m1, &h.target, &function, &tampered_args, &report,),
            Err(Ok(Error::SimulationMismatch))
        );

        // A stale report bound to a different proposal id is equally
        // rejected for this one.
        let stale = SimulationReport {
            simulator: report.simulator.clone(),
            outcome: SIM_OK,
            sim_hash: sim_hash(&h.env, &h.gov_id, 2, &h.target, &function, &args),
        };
        assert_eq!(
            h.gov
                .try_propose_with_simulation(&h.m1, &h.target, &function, &args, &stale),
            Err(Ok(Error::SimulationMismatch))
        );
    }

    #[test]
    fn unregistered_simulator_is_rejected() {
        let h = setup();
        let function = Symbol::new(&h.env, "set_value");
        let args = set_value_args(&h.env, 1);

        let mut report = report_for(&h, 1, &h.target, &function, &args);
        // An impostor simulator, correctly bound but not the registered one.
        report.simulator = Address::generate(&h.env);
        assert_eq!(
            h.gov
                .try_propose_with_simulation(&h.m1, &h.target, &function, &args, &report),
            Err(Ok(Error::SimulationMismatch))
        );
    }

    #[test]
    fn simulation_off_records_reports_but_allows_plain_propose() {
        let h = setup();
        // A member turns the requirement off (simulator stays registered).
        h.gov
            .set_simulation_config(&h.m1, &Some(h.simulator.clone()), &false);

        let cfg = h.gov.get_simulation_config().unwrap();
        assert!(!cfg.required);
        assert_eq!(cfg.simulator, Some(h.simulator.clone()));

        let args = set_value_args(&h.env, 1);
        let function = Symbol::new(&h.env, "set_value");

        // Plain propose works again...
        let plain_id = h.gov.propose(&h.m1, &h.target, &function, &args);
        assert_eq!(plain_id, 1);
        assert_eq!(h.gov.get_simulation_report(&plain_id), None);

        // ...and a reported proposal is still recorded.
        let report = report_for(&h, 2, &h.target, &function, &args);
        let id = h
            .gov
            .propose_with_simulation(&h.m1, &h.target, &function, &args, &report);
        assert_eq!(id, 2);
        assert_eq!(h.gov.get_simulation_report(&id), Some(report));
    }

    #[test]
    fn config_requires_a_simulator_when_mandatory() {
        let env = Env::default();
        env.mock_all_auths();
        let m1 = Address::generate(&env);
        let members = Vec::from_array(&env, [m1.clone()]);
        let deposits = Vec::from_array(&env, [1u64]);
        let gov_id = env.register(Governance, (members, deposits, 6000u32, 100u32));
        let gov = GovernanceClient::new(&env, &gov_id);

        assert_eq!(
            gov.try_set_simulation_config(&m1, &None, &true),
            Err(Ok(Error::SimulationNotConfigured))
        );
        assert_eq!(gov.get_simulation_config(), None);

        // Registering without the requirement is fine.
        let simulator = env.register(MockSimulator, ());
        gov.set_simulation_config(&m1, &Some(simulator), &false);
        let cfg = gov.get_simulation_config().unwrap();
        assert!(!cfg.required);
    }

    #[test]
    fn non_member_cannot_configure_simulation() {
        let h = setup();
        let outsider = Address::generate(&h.env);
        assert_eq!(
            h.gov
                .try_set_simulation_config(&outsider, &Some(h.simulator.clone()), &true),
            Err(Ok(Error::NotAMember))
        );
    }

    #[test]
    fn simulated_proposal_votes_and_executes_end_to_end() {
        let h = setup();
        let args = set_value_args(&h.env, 99);
        let function = Symbol::new(&h.env, "set_value");
        let report = report_for(&h, 1, &h.target, &function, &args);

        // m1 (1) + m2 (1) = 2 of 2 = 100% >= 60% quorum.
        let id = h
            .gov
            .propose_with_simulation(&h.m1, &h.target, &function, &args, &report);
        h.gov.vote(&h.m1, &id, &true);
        h.gov.vote(&h.m2, &id, &true);
        h.gov.execute(&id);

        let target_client = TargetClient::new(&h.env, &h.target);
        assert_eq!(target_client.get_value(), 99);
    }
}
