//! Quadratic voting weight calculation and proposal tallying.
//!
//! Voting power is calculated as the integer square root of deposited
//! governance tokens (`sqrt(deposit)`), preventing single-whale
//! domination in protocol decision making.

use soroban_sdk::{Address, Env};

use crate::math::isqrt;
use crate::{DataKey, Error};

/// Calculate the quadratic voting weight for a member based on their
/// deposited governance tokens.
///
/// Returns `sqrt(deposit)` where `deposit` is the stored token balance
/// for the member. Returns `0` if the member is not registered.
pub fn quadratic_weight(env: &Env, member: &Address) -> u64 {
    let deposit = env
        .storage()
        .persistent()
        .get(&DataKey::MemberDeposit(member.clone()))
        .unwrap_or(0u64);
    isqrt(deposit)
}

/// Accumulate quadratic weight into the proposal tally.
///
/// Called when a member votes on a proposal. Adds `quadratic_weight`
/// to either the `yes_weight` or `no_weight` of the proposal.
#[allow(dead_code)]
pub fn accumulate_quadratic_weight(
    env: &Env,
    proposal_id: u64,
    voter: &Address,
    support: bool,
) -> Result<(), Error> {
    let weight = quadratic_weight(env, voter);
    if weight == 0 {
        return Err(Error::NotAMember);
    }

    let key = DataKey::Proposal(proposal_id);
    let mut proposal: crate::Proposal = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::ProposalNotFound)?;

    if support {
        proposal.yes_weight = proposal.yes_weight.saturating_add(weight);
    } else {
        proposal.no_weight = proposal.no_weight.saturating_add(weight);
    }

    env.storage().persistent().set(&key, &proposal);
    Ok(())
}

/// Register a member's deposit for quadratic voting.
///
/// Must be called during initialization or by an authorized admin.
/// The deposit amount determines the member's voting power as `sqrt(deposit)`.
pub fn register_deposit(env: &Env, member: &Address, deposit: u64) {
    env.storage()
        .persistent()
        .set(&DataKey::MemberDeposit(member.clone()), &deposit);
}

/// Read-only: get a member's raw deposit amount.
#[allow(dead_code)]
pub fn get_deposit(env: &Env, member: &Address) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::MemberDeposit(member.clone()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl, testutils::Address as _, Address, Env};

    #[contract]
    struct StorageHost;

    #[contractimpl]
    impl StorageHost {}

    #[test]
    fn test_quadratic_weight_basic() {
        let env = Env::default();
        env.mock_all_auths();

        let host = env.register(StorageHost, ());
        let member = Address::generate(&env);
        // Deposit 100 tokens -> weight = sqrt(100) = 10
        env.as_contract(&host, || register_deposit(&env, &member, 100));
        assert_eq!(
            env.as_contract(&host, || quadratic_weight(&env, &member)),
            10
        );

        // Deposit 25 tokens -> weight = sqrt(25) = 5
        env.as_contract(&host, || register_deposit(&env, &member, 25));
        assert_eq!(
            env.as_contract(&host, || quadratic_weight(&env, &member)),
            5
        );
    }

    #[test]
    fn test_quadratic_weight_large_deposits() {
        let env = Env::default();
        env.mock_all_auths();

        let host = env.register(StorageHost, ());
        let member = Address::generate(&env);
        // Large deposit with 7 decimal places
        env.as_contract(&host, || register_deposit(&env, &member, 10_0000000)); // 10 million tokens
        assert_eq!(
            env.as_contract(&host, || quadratic_weight(&env, &member)),
            10000
        );
    }
}
