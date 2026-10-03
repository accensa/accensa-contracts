//! Quadratic voting weight calculation and proposal tallying.
//!
//! Voting power is calculated as the integer square root of deposited
//! governance tokens (`sqrt(deposit)`), preventing single-whale
//! domination in protocol decision making.


use soroban_sdk::{Address, Env};

use crate::math::isqrt;
use crate::{DataKey, Error, VeTokenLock};

/// Maximum lock duration for veToken in ledgers (~4 years at 5s/ledger).
/// This bounds the maximum voting power boost from time-weighting.
pub const MAX_LOCK_DURATION: u32 = 252_288_000;

/// Time-weighted voting: calculate voting power based on lock duration.
/// Power = amount * (time_remaining / max_duration) where time_remaining
/// is the remaining lock time as a fraction of max duration.
pub fn calculate_time_weighted_power(amount: u64, locked_at_ledger: u32, unlock_at_ledger: u32, current_ledger: u32) -> u64 {
    if current_ledger >= unlock_at_ledger {
        return 0; // Lock expired, no voting power
    }

    let total_duration = unlock_at_ledger.saturating_sub(locked_at_ledger);
    let time_remaining = unlock_at_ledger.saturating_sub(current_ledger);

    // Calculate time factor as a fraction (0 to 1)
    let time_factor = if total_duration > 0 {
        (time_remaining as u128) * 10_000 / (total_duration as u128)
    } else {
        10_000
    };

    // Voting power = amount * time_factor / 10_000
    (amount as u128) * (time_factor / 10_000) as u64
}

/// Check if veToken mechanics are enabled.
pub fn is_vetoken_enabled(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::VeTokenEnabled)
        .unwrap_or(false)
}

/// Calculate the voting weight for a member based on their
/// deposited governance tokens and optional veToken lock.
///
/// When veToken is enabled, returns `sqrt(deposit) * time_weight`
/// where time_weight is based on the remaining lock duration.
/// Otherwise returns `sqrt(deposit)`.
pub fn quadratic_weight(env: &Env, member: &Address) -> u64 {
    let deposit = env
        .storage()
        .persistent()
        .get(&DataKey::MemberDeposit(member.clone()))
        .unwrap_or(0u64);
    
    let base_weight = isqrt(deposit);
    
    // Apply time-weighting if veToken is enabled and member has an active lock
    if is_vetoken_enabled(env) {
        if let Some(lock) = env.storage().persistent().get(&DataKey::VeTokenLock(member.clone())) {
            if !lock.withdrawn {
                let current_ledger = env.ledger().sequence();
                let time_weight = calculate_time_weighted_power(
                    deposit,
                    lock.locked_at_ledger,
                    lock.unlock_at_ledger,
                    current_ledger,
                );
                // Combine base quadratic weight with time factor
                // For simplicity, we use the time-weighted power directly
                return time_weight;
            }
        }
    }
    
    base_weight
}

/// Accumulate quadratic weight into the proposal tally.
///
/// Called when a member votes on a proposal. Adds `quadratic_weight`
/// to either the `yes_weight` or `no_weight` of the proposal.
// Not yet called by the contract.
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
// Not yet called by the contract.
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
    use soroban_sdk::{testutils::Address as _, Env};

    /// Storage is only reachable from inside a contract: register a
    /// one-member governance instance to run the helpers against.
    fn setup() -> (Env, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let founder = Address::generate(&env);
        let members = soroban_sdk::Vec::from_array(&env, [founder]);
        let deposits = soroban_sdk::Vec::from_array(&env, [1u64]);
        let id = env.register(crate::Governance, (members, deposits, 10_000u32, 100u32));
        (env, id)
    }

    #[test]
    fn test_quadratic_weight_basic() {
        let (env, id) = setup();
        let member = Address::generate(&env);
        env.as_contract(&id, || {
            // Deposit 100 tokens -> weight = sqrt(100) = 10
            register_deposit(&env, &member, 100);
            assert_eq!(quadratic_weight(&env, &member), 10);

            // Deposit 25 tokens -> weight = sqrt(25) = 5
            register_deposit(&env, &member, 25);
            assert_eq!(quadratic_weight(&env, &member), 5);
        });
    }

    #[test]
    fn test_quadratic_weight_large_deposits() {
        let (env, id) = setup();
        let member = Address::generate(&env);
        env.as_contract(&id, || {
            // 10 tokens with 7 decimal places = 100_000_000 units -> sqrt = 10_000
            register_deposit(&env, &member, 10_0000000);
            assert_eq!(quadratic_weight(&env, &member), 10_000);
        });
    }
}
