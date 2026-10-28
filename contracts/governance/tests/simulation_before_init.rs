#![cfg(test)]

//! Before-initialisation coverage for `Governance::propose_with_simulation`
//! (issue #674).
//!
//! `simulation::propose_with_simulation` read the instance-storage
//! `VotingPeriod` with `.unwrap()`, so a caller reaching the governance
//! contract with that key absent — a caller before `__constructor` has
//! landed, or after the instance entry has aged out — aborted the host call
//! with a `WasmRuntimeError` instead of reporting the missing state. The
//! rewritten read answers `Error::InvalidVotingPeriod`, matching the
//! `propose` and `execute` paths that read the same key.

use governance::simulation::{sim_hash, SimulationReport, SIM_OK};
use governance::{DataKey, Error, Governance, GovernanceClient};
use soroban_sdk::{testutils::Address as _, Address, Env, Symbol, Val, Vec};

/// Register a one-member governance body and enable mandatory simulation so
/// the `propose_with_simulation` gates ahead of the `VotingPeriod` read are
/// all satisfied.
fn setup(env: &Env) -> (GovernanceClient<'static>, Address, Address, Address) {
    env.mock_all_auths();
    let member = Address::generate(env);
    let members = Vec::from_array(env, [member.clone()]);
    let deposits = Vec::from_array(env, [1u64]);
    let gov_id = env.register(Governance, (members, deposits, 6000u32, 100u32));
    let gov = GovernanceClient::new(env, &gov_id);

    let simulator = Address::generate(env);
    gov.set_simulation_config(&member, &Some(simulator.clone()), &true);

    (gov, gov_id, member, simulator)
}

/// A `propose_with_simulation` call that reaches the function with no
/// `DataKey::VotingPeriod` in instance storage — a caller reaching the
/// contract before it is initialised — yields the crate's typed error, not a
/// host abort.
#[test]
fn propose_with_simulation_before_initialisation_returns_typed_error() {
    let env = Env::default();
    let (gov, gov_id, member, simulator) = setup(&env);

    let target = Address::generate(&env);
    let function = Symbol::new(&env, "set_value");
    let args: Vec<Val> = Vec::new(&env);
    let report = SimulationReport {
        simulator,
        outcome: SIM_OK,
        sim_hash: sim_hash(&env, &gov_id, 1, &target, &function, &args),
    };

    // Drop the constructor-written voting period: the governance instance now
    // looks exactly as it does to a caller before `__constructor` has landed.
    env.as_contract(&gov_id, || {
        env.storage().instance().remove(&DataKey::VotingPeriod);
    });

    assert_eq!(
        gov.try_propose_with_simulation(&member, &target, &function, &args, &report),
        Err(Ok(Error::InvalidVotingPeriod)),
        "a missing VotingPeriod must surface as InvalidVotingPeriod, not a host abort"
    );
}
