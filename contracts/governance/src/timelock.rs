//! 48-hour timelock for protocol upgrades (issue #447).
//!
//! Every protocol upgrade routed through [`Governance`] — a target contract's
//! Wasm swap or a sensitive parameter change — has to be queued here before it
//! may run. The queue maps the upgrade's **call hash** to the ledger at which
//! it first becomes executable, [`TIMELOCK_DELAY`] ledgers later (48 hours at
//! the network's ~5 s ledger close). Until then
//! [`execute_queued_transaction`] rejects it with [`Error::TimelockNotExpired`],
//! which is what gives users a full two days to read the pending change and
//! exit before it takes effect.
//!
//! - [`queue_transaction`] records the upgrade and returns its call hash.
//! - [`execute_queued_transaction`] runs it — and only once the delay has
//!   fully elapsed. Execution is permissionless, mirroring
//!   [`Governance::execute`](crate::Governance::execute): a queued call
//!   carries no authority beyond what this contract already holds, and a
//!   target that gates itself with `require_auth()` authorizes it because
//!   this contract is the caller.
//! - [`get_queued_transaction`] is the read-only view of a queue entry.
//!
//! Queueing is restricted to registered members — the governance body *is* the
//! admin this delay protects against — and one call can only ever sit in the
//! queue once, so nobody can keep pushing an already-queued upgrade's clock.

use soroban_sdk::{
    contractevent, contracttype, xdr::ToXdr, Address, Bytes, BytesN, Env, Symbol, Val, Vec,
};

use crate::{DataKey, Error};

/// Timelock every protocol upgrade waits out before it may execute, in ledger
/// sequences: **48 hours** at the network's ~5 second ledger close
/// (`48 * 60 * 60 / 5 = 34_560`).
pub const TIMELOCK_DELAY: u32 = 34_560;

/// Slack added on top of [`TIMELOCK_DELAY`] when a queue entry's persistent
/// TTL is set, so an entry whose delay has *just* elapsed is still live when
/// someone executes it (the same grace `PROPOSAL_TTL_GRACE` gives proposals).
const QUEUE_TTL_GRACE: u32 = 100;

/// Domain-separation prefix for the queue key's hash, so a call hash computed
/// here can never collide with one computed for any other purpose.
const HASH_DOMAIN: &[u8] = b"accensa:gov:upgrade:v1";

/// Storage key for one queued upgrade. The queue is the issue's
/// `Hash -> ExecutionTime` mapping: the hash is the key, the record it points
/// at carries the ledger the upgrade becomes executable on.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimelockKey {
    /// Persistent: the upgrade whose call hash is `.0` is waiting out its
    /// 48-hour timelock.
    Queued(BytesN<32>),
}

/// A protocol upgrade waiting out the 48-hour timelock.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedTransaction {
    /// SHA-256 of the domain-separated `(target, function, args)` call — the
    /// key this entry is stored under.
    pub call_hash: BytesN<32>,
    /// First ledger at which the call may execute
    /// (`queued_at_ledger + TIMELOCK_DELAY`).
    pub execution_ledger: u32,
    /// Member that queued it.
    pub queued_by: Address,
    pub target: Address,
    pub function: Symbol,
    pub args: Vec<Val>,
}

/// Emitted when an upgrade enters the timelock.
///
/// Topics: `("upgrade_queued_event", call_hash)`. The data map carries the
/// queuer, the call target and function, and the ledger the upgrade first
/// becomes executable on — enough for an indexer to track every pending
/// upgrade from the event log alone.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeQueuedEvent {
    #[topic]
    pub call_hash: BytesN<32>,
    pub queued_by: Address,
    pub target: Address,
    pub function: Symbol,
    pub execution_ledger: u32,
}

/// Emitted when a queued upgrade runs.
///
/// Topics: `("upgrade_executed_event", call_hash)`. The data map carries the
/// call target and function and the ledger it executed at.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeExecutedEvent {
    #[topic]
    pub call_hash: BytesN<32>,
    pub target: Address,
    pub function: Symbol,
    pub executed_at_ledger: u32,
}

/// Queue a protocol upgrade for execution after [`TIMELOCK_DELAY`] ledgers.
///
/// The caller must be a registered member. Returns the call hash to hand to
/// [`execute_queued_transaction`] once the delay has elapsed.
///
/// # Errors
/// - `NotAMember`: `caller` has no registered deposit.
/// - `AlreadyQueued`: the identical call is already in the queue — refusing
///   (rather than overwriting) is what stops the delay being renewed forever
///   by re-queueing the same call.
pub fn queue_transaction(
    env: &Env,
    caller: &Address,
    target: Address,
    function: Symbol,
    args: Vec<Val>,
) -> Result<BytesN<32>, Error> {
    caller.require_auth();
    if !env
        .storage()
        .persistent()
        .has(&DataKey::MemberDeposit(caller.clone()))
    {
        return Err(Error::NotAMember);
    }

    let call_hash = call_hash(env, &target, &function, &args);
    let key = TimelockKey::Queued(call_hash.clone());
    if env.storage().persistent().has(&key) {
        return Err(Error::AlreadyQueued);
    }

    let execution_ledger = env.ledger().sequence().saturating_add(TIMELOCK_DELAY);
    let queued = QueuedTransaction {
        call_hash: call_hash.clone(),
        execution_ledger,
        queued_by: caller.clone(),
        target: target.clone(),
        function: function.clone(),
        args,
    };
    env.storage().persistent().set(&key, &queued);
    // Cover the whole delay plus a grace, so a fresh entry cannot be archived
    // between "the delay just elapsed" and somebody executing it.
    let ttl = TIMELOCK_DELAY.saturating_add(QUEUE_TTL_GRACE);
    env.storage().persistent().extend_ttl(&key, ttl, ttl);

    UpgradeQueuedEvent {
        call_hash: call_hash.clone(),
        queued_by: caller.clone(),
        target,
        function,
        execution_ledger,
    }
    .publish(env);

    Ok(call_hash)
}

/// Execute a queued upgrade once its 48-hour timelock has elapsed.
///
/// Callable by anyone: the queued call carries no authority beyond what this
/// contract already holds, so the only thing a caller chooses is *when* — and
/// before the delay that choice is refused outright.
///
/// # Errors
/// - `NoQueuedTransaction`: nothing is queued under `call_hash` (never was,
///   or it has already been executed).
/// - `TimelockNotExpired`: the 48 hours have not fully elapsed yet.
pub fn execute_queued_transaction(env: &Env, call_hash: BytesN<32>) -> Result<(), Error> {
    let key = TimelockKey::Queued(call_hash.clone());
    let queued: QueuedTransaction = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::NoQueuedTransaction)?;

    if env.ledger().sequence() < queued.execution_ledger {
        return Err(Error::TimelockNotExpired);
    }

    // Effects before interaction: the entry is consumed before the external
    // call, so a reentrant `execute_queued_transaction` fired from inside the
    // target finds nothing queued instead of running the upgrade twice.
    env.storage().persistent().remove(&key);

    let _: Val = env.invoke_contract(&queued.target, &queued.function, queued.args.clone());

    UpgradeExecutedEvent {
        call_hash,
        target: queued.target,
        function: queued.function,
        executed_at_ledger: env.ledger().sequence(),
    }
    .publish(env);

    Ok(())
}

/// Read-only: the upgrade queued under `call_hash`, or `NoQueuedTransaction`
/// if there is none.
pub fn get_queued_transaction(
    env: &Env,
    call_hash: BytesN<32>,
) -> Result<QueuedTransaction, Error> {
    env.storage()
        .persistent()
        .get(&TimelockKey::Queued(call_hash))
        .ok_or(Error::NoQueuedTransaction)
}

/// `sha256(HASH_DOMAIN ‖ target ‖ function ‖ args)` over the XDR encoding of
/// each part — the hash the queue is keyed by.
fn call_hash(env: &Env, target: &Address, function: &Symbol, args: &Vec<Val>) -> BytesN<32> {
    let mut buf = Bytes::from_slice(env, HASH_DOMAIN);
    buf.append(&target.clone().to_xdr(env));
    buf.append(&function.clone().to_xdr(env));
    buf.append(&args.clone().to_xdr(env));
    env.crypto().sha256(&buf).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Governance, GovernanceClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        IntoVal,
    };

    /// `(env, governance id, registered member)`: a one-member body, which is
    /// the smallest construction `__constructor` accepts.
    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();

        let member = Address::generate(&env);
        let members = Vec::from_array(&env, [member.clone()]);
        let deposits = Vec::from_array(&env, [1u64]);
        let gov_id = env.register(Governance, (members, deposits, 6000u32, 100u32));
        (env, gov_id, member)
    }

    /// A parameter change for the queued upgrade to perform — a call to this
    /// contract's own `set_treasury_token`. Returns
    /// `(target, function, args, token)` so a test can assert the effect.
    fn treasury_call(env: &Env, gov: &Address) -> (Address, Symbol, Vec<Val>, Address) {
        let token = Address::generate(env);
        let args = Vec::from_array(env, [token.clone().into_val(env)]);
        (
            gov.clone(),
            Symbol::new(env, "set_treasury_token"),
            args,
            token,
        )
    }

    #[test]
    fn delay_is_forty_eight_hours_in_ledgers() {
        // 48 hours * 3600 s/h, one ledger every ~5 s.
        assert_eq!(TIMELOCK_DELAY, 34_560);
    }

    #[test]
    fn queue_records_the_call_hash_and_its_execution_ledger() {
        let (env, gov_id, member) = setup();
        let gov = GovernanceClient::new(&env, &gov_id);
        let (target, function, args, _token) = treasury_call(&env, &gov_id);

        let queued_at = env.ledger().sequence();
        let call_hash = gov.queue_upgrade(&member, &target, &function, &args);

        let queued = gov.get_queued_transaction(&call_hash);
        assert_eq!(queued.call_hash, call_hash);
        assert_eq!(queued.execution_ledger, queued_at + TIMELOCK_DELAY);
        assert_eq!(queued.queued_by, member);
        assert_eq!(queued.target, target);
        assert_eq!(queued.function, function);
        assert_eq!(queued.args, args);
    }

    #[test]
    fn executing_before_the_timelock_expires_reverts() {
        let (env, gov_id, member) = setup();
        let gov = GovernanceClient::new(&env, &gov_id);
        let (target, function, args, _token) = treasury_call(&env, &gov_id);
        let call_hash = gov.queue_upgrade(&member, &target, &function, &args);

        // Same ledger: the upgrade must not run.
        let res = gov.try_execute_queued_transaction(&call_hash);
        assert_eq!(res, Err(Ok(Error::TimelockNotExpired)));
        assert_eq!(gov.get_treasury_token(), None);

        // One ledger short of the full 48 hours — still refused.
        env.ledger()
            .with_mut(|l| l.sequence_number += TIMELOCK_DELAY - 1);
        let res = gov.try_execute_queued_transaction(&call_hash);
        assert_eq!(res, Err(Ok(Error::TimelockNotExpired)));
        assert_eq!(gov.get_treasury_token(), None);

        // And neither failed attempt consumed the queue entry.
        assert!(gov.get_queued_transaction(&call_hash).is_ok());
    }

    #[test]
    fn executing_after_the_timelock_expires_runs_the_upgrade() {
        let (env, gov_id, member) = setup();
        let gov = GovernanceClient::new(&env, &gov_id);
        let (target, function, args, token) = treasury_call(&env, &gov_id);
        let call_hash = gov.queue_upgrade(&member, &target, &function, &args);

        env.ledger()
            .with_mut(|l| l.sequence_number += TIMELOCK_DELAY);
        gov.execute_queued_transaction(&call_hash);

        // The queued call ran…
        assert_eq!(gov.get_treasury_token(), Some(token));
        // …and was consumed, so it cannot run a second time.
        let res = gov.try_get_queued_transaction(&call_hash);
        assert_eq!(res, Err(Ok(Error::NoQueuedTransaction)));
        let res = gov.try_execute_queued_transaction(&call_hash);
        assert_eq!(res, Err(Ok(Error::NoQueuedTransaction)));
    }

    #[test]
    fn unknown_call_hash_has_nothing_to_execute() {
        let (env, gov_id, _member) = setup();
        let gov = GovernanceClient::new(&env, &gov_id);

        let res = gov.try_execute_queued_transaction(&BytesN::from_array(&env, &[9u8; 32]));
        assert_eq!(res, Err(Ok(Error::NoQueuedTransaction)));
    }

    #[test]
    fn non_member_cannot_queue_an_upgrade() {
        let (env, gov_id, _member) = setup();
        let gov = GovernanceClient::new(&env, &gov_id);
        let (target, function, args, _token) = treasury_call(&env, &gov_id);
        let outsider = Address::generate(&env);

        let res = gov.try_queue_upgrade(&outsider, &target, &function, &args);
        assert_eq!(res, Err(Ok(Error::NotAMember)));
    }

    #[test]
    fn identical_upgrade_cannot_be_queued_twice() {
        let (env, gov_id, member) = setup();
        let gov = GovernanceClient::new(&env, &gov_id);
        let (target, function, args, _token) = treasury_call(&env, &gov_id);

        let first = gov.queue_upgrade(&member, &target, &function, &args);
        let res = gov.try_queue_upgrade(&member, &target, &function, &args);
        assert_eq!(res, Err(Ok(Error::AlreadyQueued)));

        // The rejected attempt did not push the original clock out.
        let queued = gov.get_queued_transaction(&first);
        assert_eq!(
            queued.execution_ledger,
            env.ledger().sequence() + TIMELOCK_DELAY
        );
    }
}
