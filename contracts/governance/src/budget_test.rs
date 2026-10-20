#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `Governance` using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if
//! the invocation's measured CPU instruction count exceeds `N`. `N` is the
//! committed failure threshold = `measured_baseline * 1.15` (the 15% tolerance
//! defined in `budget.toml`). Re-measure deliberately and commit the new
//! ceiling; never regenerate automatically.
//!
//! These tests register the real `.wasm`, so the numbers are the WASM-mode
//! estimates that track on-chain cost. `Env::cost_estimate().cpu_instruction_cost`
//! explicitly *underestimates* natively-registered contracts because native Rust
//! is not metered per instruction (see the SDK docs); the in-tree
//! `test::maximum_anonymity_set_fits_soroban_cpu_budget` test runs natively and
//! therefore reports a number far below the real WASM cost.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p governance --target wasm32v1-none --release`
//!
//! ## Known over-budget entry point: `vote_anonymous`
//!
//! `propose` and `vote` fit the Mainnet per-transaction CPU limit
//! (`TX_MAX_CPU_INSTRUCTIONS`) with room to spare, so they are gated as budget
//! assertions. `vote_anonymous` does **not**: its LSAG verification costs
//! ~0.74B CPU instructions at ring size 2 and ~2.95B at ring size 8 — 7x and
//! 30x the 100M Mainnet limit respectively (`measure_vote_anonymous_by_ring_size`
//! prints the exact multiple). Its gate below is therefore a **regression
//! ceiling**, not a statement that the call is affordable on-chain. Bringing
//! anonymous voting within budget is a contract-performance task, tracked
//! separately from this file.

extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{
    contract, contractimpl, testutils::Address as _, Address, Bytes, BytesN, Env, IntoVal, Symbol,
    Val, Vec,
};

fn load_wasm(env: &Env, path: &str) -> Bytes {
    let buf = std::fs::read(path).expect(
        "contract wasm not found; run `cargo build -p governance \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Minimal contract that wraps `deploy_v2`, so tests can deploy the real
/// `governance` wasm with its constructor args from inside a contract context.
/// `deploy_v2`/`with_current_contract` only run inside a contract context (the
/// test harness has no current contract ID), and `register_contract_wasm`
/// cannot pass constructor args to a contract that has a `__constructor` — it
/// would invoke it with zero args and trap.
#[contract]
pub struct TestDeployer;

#[contractimpl]
impl TestDeployer {
    pub fn deploy(
        env: Env,
        wasm_hash: BytesN<32>,
        salt: BytesN<32>,
        members: Vec<Address>,
        deposits: Vec<u64>,
        threshold_bps: u32,
        voting_period_ledgers: u32,
    ) -> Address {
        env.deployer().with_current_contract(salt).deploy_v2(
            wasm_hash,
            (members, deposits, threshold_bps, voting_period_ledgers),
        )
    }
}

/// Deploys the real `governance.wasm` with the given constructor args.
fn deploy_governance(
    env: &Env,
    members: Vec<Address>,
    deposits: Vec<u64>,
    threshold_bps: u32,
    voting_period_ledgers: u32,
) -> Address {
    let wasm = load_wasm(env, "../../target/wasm32v1-none/release/governance.wasm");
    let wasm_hash = env.deployer().upload_contract_wasm(wasm);
    env.mock_all_auths();
    let deployer = env.register(TestDeployer, ());
    TestDeployerClient::new(env, &deployer).deploy(
        &wasm_hash,
        &BytesN::from_array(env, &[0u8; 32]),
        &members,
        &deposits,
        &threshold_bps,
        &voting_period_ledgers,
    )
}

/// Registers the real `governance.wasm`, constructs with 8 members (deposit 1
/// each, weight 1 each, total 8), registers a voting key for each member, and
/// returns the client, members, and the ring of 8 public keys.
fn setup_governance(env: &Env) -> (GovernanceClient<'static>, Vec<Address>, Vec<BytesN<32>>) {
    // The measured call is the only thing the `budget_cpu_lt` gate is about;
    // host resource-limit enforcement would trap the Wasm upload/registration
    // setup before the gate can run. Every test points the CPU budget at just
    // the measured call via `budget().reset_unlimited()`.
    env.cost_estimate().disable_resource_limits();
    let mut members = Vec::new(env);
    for _ in 0..8u32 {
        members.push_back(Address::generate(env));
    }
    let deposits = Vec::from_array(env, [1u64; 8]);
    let id = deploy_governance(env, members.clone(), deposits, 5000u32, 100u32);
    let client = GovernanceClient::new(env, &id);

    let mut ring = Vec::new(env);
    for i in 0..8u32 {
        let secret = crate::ring_sig::test_support::secret(env, (100 + i) as u64);
        let public_key = crate::ring_sig::test_support::public(env, &secret);
        client.register_voting_key(&members.get(i).unwrap(), &public_key);
        ring.push_back(public_key);
    }

    (client, members, ring)
}

fn propose_call(env: &Env) -> (Address, Symbol, Vec<Val>) {
    let target = Address::generate(env);
    (
        target,
        Symbol::new(env, "set_value"),
        Vec::from_array(env, [42u32.into_val(env)]),
    )
}

/// Baseline measured for `propose` with an 8-member set: 1_013_355 * 1.15.
#[test]
#[budget_cpu_lt(1_165_359)]
fn budget_propose() {
    let env = Env::default();
    let (client, members, _ring) = setup_governance(&env);
    let (target, function, args) = propose_call(&env);
    env.cost_estimate().budget().reset_unlimited();
    client.propose(&members.get(0).unwrap(), &target, &function, &args);
}

/// Baseline measured for `vote` on a pre-populated proposal: 1_027_580 * 1.15.
#[test]
#[budget_cpu_lt(1_181_717)]
fn budget_vote() {
    let env = Env::default();
    let (client, members, _ring) = setup_governance(&env);
    let (target, function, args) = propose_call(&env);
    let proposal_id = client.propose(&members.get(0).unwrap(), &target, &function, &args);
    env.cost_estimate().budget().reset_unlimited();
    client.vote(&members.get(0).unwrap(), &proposal_id, &true);
}

/// Regression ceiling for `vote_anonymous` with an 8-member ring:
/// 2_954_752_614 * 1.15. NOTE: this is ~30x the 100M Mainnet per-transaction
/// CPU limit — see the module docs. The gate catches cost regressions; it is
/// not a claim that anonymous voting is affordable on-chain.
#[test]
#[budget_cpu_lt(3_397_965_507)]
fn budget_vote_anonymous() {
    let env = Env::default();
    let (client, members, ring) = setup_governance(&env);
    let (target, function, args) = propose_call(&env);
    let proposal_id = client.propose(&members.get(0).unwrap(), &target, &function, &args);

    // Same seeds as `setup_governance` used to register the ring's public keys,
    // so `secrets[i]` is the private key of `ring[i]`.
    let secrets: [BytesN<32>; 8] = [
        crate::ring_sig::test_support::secret(&env, 100),
        crate::ring_sig::test_support::secret(&env, 101),
        crate::ring_sig::test_support::secret(&env, 102),
        crate::ring_sig::test_support::secret(&env, 103),
        crate::ring_sig::test_support::secret(&env, 104),
        crate::ring_sig::test_support::secret(&env, 105),
        crate::ring_sig::test_support::secret(&env, 106),
        crate::ring_sig::test_support::secret(&env, 107),
    ];

    let signature = env.as_contract(&client.address, || {
        crate::ring_sig::test_support::sign(&env, proposal_id, true, &ring, &secrets, 0)
    });

    env.cost_estimate().budget().reset_unlimited();
    client.vote_anonymous(&proposal_id, &true, &ring, &signature);
}

/// Soroban per-transaction limits (mainnet network config).
const TX_MAX_CPU_INSTRUCTIONS: u64 = 100_000_000;
const TX_MAX_MEMORY_BYTES: u64 = 41_943_040;

/// Measures `vote_anonymous` CPU at ring sizes 2, 4, 8, prints each cost and
/// its multiple of the Mainnet per-transaction CPU limit, and gates against
/// regressions.
///
/// This deliberately does **not** assert that the cost fits the network limit:
/// every ring size measured so far exceeds it (ring 2 ≈ 7.4x, ring 8 ≈ 29.5x).
/// The assertion is a regression ceiling at `measured * 1.15`, so a cost
/// regression is caught while the current, over-budget cost is recorded rather
/// than hidden. See the module docs.
#[test]
fn measure_vote_anonymous_by_ring_size() {
    // Committed ceilings: measured * 1.15, indexed by ring size (2, 4, 8).
    let ceilings: [(u32, u64); 3] = [(2, 856_570_780), (4, 1_703_623_165), (8, 3_397_965_507)];
    for (ring_size, cpu_ceiling) in ceilings {
        let env = Env::new_with_config(soroban_sdk::testutils::EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.cost_estimate().disable_resource_limits();
        let mut members = Vec::new(&env);
        for _ in 0..ring_size {
            members.push_back(Address::generate(&env));
        }
        let mut deposits = Vec::new(&env);
        for _ in 0..ring_size {
            deposits.push_back(1u64);
        }

        let id = deploy_governance(&env, members.clone(), deposits, 5000u32, 100u32);
        let client = GovernanceClient::new(&env, &id);

        let mut ring = Vec::new(&env);
        for i in 0..ring_size {
            let secret = crate::ring_sig::test_support::secret(&env, (300 + i) as u64);
            let public_key = crate::ring_sig::test_support::public(&env, &secret);
            client.register_voting_key(&members.get(i).unwrap(), &public_key);
            ring.push_back(public_key);
        }

        let (target, function, args) = propose_call(&env);
        let proposal_id = client.propose(&members.get(0).unwrap(), &target, &function, &args);

        // Same seeds as the ring registration above, so `secrets[i]` is the
        // private key of `ring[i]`.
        let secrets: [BytesN<32>; 8] = [
            crate::ring_sig::test_support::secret(&env, 300),
            crate::ring_sig::test_support::secret(&env, 301),
            crate::ring_sig::test_support::secret(&env, 302),
            crate::ring_sig::test_support::secret(&env, 303),
            crate::ring_sig::test_support::secret(&env, 304),
            crate::ring_sig::test_support::secret(&env, 305),
            crate::ring_sig::test_support::secret(&env, 306),
            crate::ring_sig::test_support::secret(&env, 307),
        ];
        // Only the first `ring_size` secrets are used.
        let secrets_slice: &[BytesN<32>] = &secrets[..ring_size as usize];

        let signature = env.as_contract(&client.address, || {
            crate::ring_sig::test_support::sign(&env, proposal_id, true, &ring, secrets_slice, 0)
        });

        env.cost_estimate().budget().reset_unlimited();
        client.vote_anonymous(&proposal_id, &true, &ring, &signature);
        let cpu = env.cost_estimate().budget().cpu_instruction_cost();
        let mem = env.cost_estimate().budget().memory_bytes_cost();
        std::println!(
            "BENCHMARK vote_anonymous ring={ring_size} cpu={cpu} mem={mem} \
             cpu_x_limit={}",
            cpu * 100 / TX_MAX_CPU_INSTRUCTIONS
        );

        assert!(
            cpu <= cpu_ceiling,
            "ring {ring_size}: cpu {cpu} exceeded regression ceiling {cpu_ceiling}"
        );
        // Memory does fit the network limit, so it is gated as a real budget.
        assert!(
            mem * 10 < TX_MAX_MEMORY_BYTES,
            "ring {ring_size}: mem {mem}"
        );
    }
}
