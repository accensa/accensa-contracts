//! Standardized read-only telemetry view for frontend dashboards.
//!
//! Every Accensa contract that escrows funds or collects fees exposes the same
//! aggregated snapshot through [`TelemetryProvider::get_telemetry`], so a
//! dashboard (or indexer) can render protocol health — active escrows, channel
//! counts, fees collected — from one canonical response shape instead of
//! per-contract queries.
//!
//! # Optimization contract
//!
//! `get_telemetry` is a **read-only** entry point. Implementations must:
//!
//! - perform **no storage writes** and never extend storage TTL;
//! - require **no authorization**;
//! - read only the specific storage keys backing each field (targeted
//!   `instance().get` calls) — never iterate records or load full channel or
//!   refund structs — so CPU cost is O(1) in the number of channels/refunds;
//! - be **infallible**: absent keys read as `0`, never as an error.
//!
//! The shared helpers [`read_counter`] and [`read_amount`] implement the
//! targeted-read pattern; [`Telemetry::empty`] stamps the snapshot metadata.

use soroban_sdk::{contractclient, contracttype, Env, IntoVal, Val};

/// Aggregated protocol telemetry returned by `get_telemetry`.
///
/// All counters are cumulative unless noted; amounts are in token base units.
/// The struct is `Copy` so the read path never clones it.
#[contracttype]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Telemetry {
    /// Total number of channels ever opened (monotonic counter).
    pub total_channels: u64,
    /// Channels currently open: escrow locked, states still being signed.
    pub open_channels: u64,
    /// Channels cooperatively closed; the challenge window is still running.
    pub closed_channels: u64,
    /// Channels with an active dispute.
    pub disputed_channels: u64,
    /// Channels finalized: escrow paid out (or reclaimed) and the record
    /// closed.
    pub finalized_channels: u64,
    /// Sum of the escrowed `amount` across every channel in an active phase
    /// (open or disputed), in token base units.
    pub active_escrows: i128,
    /// Cumulative fees collected by the protocol, in token base units.
    pub fees_collected: i128,
    /// Ledger sequence at which this snapshot was taken.
    pub ledger_sequence: u32,
    /// Wall-clock time (Unix seconds) at which this snapshot was taken.
    pub timestamp: u64,
}

impl Telemetry {
    /// A snapshot with every counter zeroed and only the ledger metadata
    /// (`ledger_sequence` / `timestamp`) stamped from `env`.
    ///
    /// Implementations seed their counters on top of this so a fresh contract
    /// reports a well-formed, all-zero snapshot instead of trapping.
    pub fn empty(env: &Env) -> Self {
        Self {
            ledger_sequence: env.ledger().sequence(),
            timestamp: env.ledger().timestamp(),
            ..Self::default()
        }
    }
}

/// Read a `u64` counter from instance storage, returning `0` when the key is
/// absent (e.g. a contract that has never incremented it).
///
/// This is the targeted-read pattern the telemetry view is built on: one
/// `instance().get` per field, no iteration, no full-record deserialization.
#[inline]
pub fn read_counter(env: &Env, key: &impl IntoVal<Env, Val>) -> u64 {
    env.storage().instance().get(key).unwrap_or(0)
}

/// Read an `i128` amount from instance storage, returning `0` when the key is
/// absent. See [`read_counter`] for the targeted-read pattern.
#[inline]
pub fn read_amount(env: &Env, key: &impl IntoVal<Env, Val>) -> i128 {
    env.storage().instance().get(key).unwrap_or(0)
}

/// Read-only telemetry entry point implemented by every Accensa contract that
/// escrows funds or collects fees.
///
/// The generated [`TelemetryClient`] lets dashboards and indexers pull the
/// snapshot cross-contract with the same typed shape.
#[contractclient(name = "TelemetryClient")]
pub trait TelemetryProvider {
    /// Return the aggregated telemetry snapshot.
    ///
    /// Read-only: performs no writes, requires no authorization, and never
    /// extends storage TTL. Implementations read only the specific storage
    /// keys backing each field, so the CPU cost is O(1) in the number of
    /// channels or refunds. Never fails: absent keys read as `0`.
    fn get_telemetry(env: Env) -> Telemetry;
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{storage::Instance as _, Ledger as _},
        Env,
    };

    /// Storage keys backing the probe contract's telemetry fields. The probe
    /// mirrors how a real contract (e.g. `StateChannel`) would store its
    /// counters: one instance key per aggregated field.
    #[contracttype]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ProbeKey {
        TotalChannels,
        OpenChannels,
        ClosedChannels,
        DisputedChannels,
        FinalizedChannels,
        ActiveEscrows,
        FeesCollected,
    }

    /// Minimal contract that stores telemetry counters in instance storage
    /// and serves them through the standardized view.
    #[contract]
    struct TelemetryProbe;

    #[contractimpl]
    impl TelemetryProbe {
        /// Seed a `u64` counter (test-only setter).
        pub fn set_counter(env: Env, key: ProbeKey, value: u64) {
            env.storage().instance().set(&key, &value);
        }

        /// Seed an `i128` amount (test-only setter).
        pub fn set_amount(env: Env, key: ProbeKey, value: i128) {
            env.storage().instance().set(&key, &value);
        }

        /// Standardized read-only telemetry entry point (delegates to the
        /// trait implementation below).
        pub fn get_telemetry(env: Env) -> Telemetry {
            <TelemetryProbe as TelemetryProvider>::get_telemetry(env)
        }
    }

    impl TelemetryProvider for TelemetryProbe {
        fn get_telemetry(env: Env) -> Telemetry {
            Telemetry {
                total_channels: read_counter(&env, &ProbeKey::TotalChannels),
                open_channels: read_counter(&env, &ProbeKey::OpenChannels),
                closed_channels: read_counter(&env, &ProbeKey::ClosedChannels),
                disputed_channels: read_counter(&env, &ProbeKey::DisputedChannels),
                finalized_channels: read_counter(&env, &ProbeKey::FinalizedChannels),
                active_escrows: read_amount(&env, &ProbeKey::ActiveEscrows),
                fees_collected: read_amount(&env, &ProbeKey::FeesCollected),
                ledger_sequence: env.ledger().sequence(),
                timestamp: env.ledger().timestamp(),
            }
        }
    }

    /// Register a fresh probe and return `(env, contract_id, client,
    /// probe_client)` where both clients target the same contract.
    fn setup() -> (
        Env,
        soroban_sdk::Address,
        TelemetryClient<'static>,
        TelemetryProbeClient<'static>,
    ) {
        let env = Env::default();
        let contract_id = env.register(TelemetryProbe, ());
        let client = TelemetryClient::new(&env, &contract_id);
        let probe = TelemetryProbeClient::new(&env, &contract_id);
        (env, contract_id, client, probe)
    }

    /// Query response completeness: every field of the response must reflect
    /// the stored state — no field left at its default.
    #[test]
    fn test_get_telemetry_response_completeness() {
        let (env, _id, client, probe) = setup();
        env.ledger().set_timestamp(1_700_000_000);

        probe.set_counter(&ProbeKey::TotalChannels, &42);
        probe.set_counter(&ProbeKey::OpenChannels, &7);
        probe.set_counter(&ProbeKey::ClosedChannels, &5);
        probe.set_counter(&ProbeKey::DisputedChannels, &3);
        probe.set_counter(&ProbeKey::FinalizedChannels, &27);
        probe.set_amount(&ProbeKey::ActiveEscrows, &1_234_567);
        probe.set_amount(&ProbeKey::FeesCollected, &8_910);

        let telemetry = client.get_telemetry();

        assert_eq!(telemetry.total_channels, 42);
        assert_eq!(telemetry.open_channels, 7);
        assert_eq!(telemetry.closed_channels, 5);
        assert_eq!(telemetry.disputed_channels, 3);
        assert_eq!(telemetry.finalized_channels, 27);
        assert_eq!(telemetry.active_escrows, 1_234_567);
        assert_eq!(telemetry.fees_collected, 8_910);
        assert_eq!(telemetry.ledger_sequence, env.ledger().sequence());
        assert_eq!(telemetry.timestamp, 1_700_000_000);
    }

    /// A fresh contract with no seeded state must report a well-formed,
    /// all-zero snapshot (never a trap) with the ledger metadata stamped.
    #[test]
    fn test_get_telemetry_empty_state_is_zeroed_but_timestamped() {
        let (env, _id, client, _probe) = setup();
        env.ledger().set_timestamp(1_700_000_123);

        let telemetry = client.get_telemetry();

        assert_eq!(telemetry.total_channels, 0);
        assert_eq!(telemetry.open_channels, 0);
        assert_eq!(telemetry.closed_channels, 0);
        assert_eq!(telemetry.disputed_channels, 0);
        assert_eq!(telemetry.finalized_channels, 0);
        assert_eq!(telemetry.active_escrows, 0);
        assert_eq!(telemetry.fees_collected, 0);
        assert_eq!(telemetry.ledger_sequence, env.ledger().sequence());
        assert_eq!(telemetry.timestamp, 1_700_000_123);
    }

    /// The query must be strictly read-only: the instance TTL is untouched
    /// and the telemetry keys the view reads are still absent afterwards (the
    /// call created no storage entries).
    #[test]
    fn test_get_telemetry_is_read_only() {
        let (env, id, client, _probe) = setup();

        let ttl_before = env.as_contract(&id, || env.storage().instance().get_ttl());

        client.get_telemetry();

        let ttl_after = env.as_contract(&id, || env.storage().instance().get_ttl());
        assert_eq!(
            ttl_after, ttl_before,
            "get_telemetry must not extend the instance TTL"
        );

        env.as_contract(&id, || {
            assert!(
                !env.storage().instance().has(&ProbeKey::TotalChannels),
                "get_telemetry must not write storage entries"
            );
            assert!(
                !env.storage().instance().has(&ProbeKey::ActiveEscrows),
                "get_telemetry must not write storage entries"
            );
        });
    }

    /// The view must track state changes between calls (no caching, no
    /// stale reads).
    #[test]
    fn test_get_telemetry_tracks_state_changes() {
        let (_env, _id, client, probe) = setup();

        assert_eq!(client.get_telemetry().open_channels, 0);

        probe.set_counter(&ProbeKey::OpenChannels, &3);
        assert_eq!(client.get_telemetry().open_channels, 3);

        probe.set_counter(&ProbeKey::OpenChannels, &1);
        probe.set_amount(&ProbeKey::FeesCollected, &500);
        let telemetry = client.get_telemetry();
        assert_eq!(telemetry.open_channels, 1);
        assert_eq!(telemetry.fees_collected, 500);
    }

    /// Property: for every reachable state, the per-phase channel counts must
    /// sum exactly to the total channel count. Seeded with a deterministic
    /// LCG so the test is reproducible without external dependencies.
    #[test]
    fn test_channel_phase_counts_sum_to_total() {
        let (_env, _id, client, probe) = setup();

        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next = || {
            // 64-bit LCG (Numerical Recipes constants).
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state >> 33
        };

        for _ in 0..64 {
            let total = next() % 10_000;
            let open = next() % (total + 1);
            let closed = next() % (total + 1 - open);
            let disputed = next() % (total + 1 - open - closed);
            let finalized = total - open - closed - disputed;

            probe.set_counter(&ProbeKey::TotalChannels, &total);
            probe.set_counter(&ProbeKey::OpenChannels, &open);
            probe.set_counter(&ProbeKey::ClosedChannels, &closed);
            probe.set_counter(&ProbeKey::DisputedChannels, &disputed);
            probe.set_counter(&ProbeKey::FinalizedChannels, &finalized);

            let telemetry = client.get_telemetry();
            assert_eq!(
                telemetry.open_channels
                    + telemetry.closed_channels
                    + telemetry.disputed_channels
                    + telemetry.finalized_channels,
                telemetry.total_channels,
                "phase counts must sum to total_channels"
            );
        }
    }

    /// The shared helpers must default to `0` for absent keys and round-trip
    /// stored values otherwise.
    #[test]
    fn test_read_helpers_default_and_roundtrip() {
        let env = Env::default();
        let id = env.register(TelemetryProbe, ());

        env.as_contract(&id, || {
            assert_eq!(read_counter(&env, &ProbeKey::OpenChannels), 0);
            assert_eq!(read_amount(&env, &ProbeKey::ActiveEscrows), 0);

            env.storage().instance().set(&ProbeKey::OpenChannels, &9u64);
            env.storage()
                .instance()
                .set(&ProbeKey::ActiveEscrows, &-42i128);

            assert_eq!(read_counter(&env, &ProbeKey::OpenChannels), 9);
            assert_eq!(read_amount(&env, &ProbeKey::ActiveEscrows), -42);
        });
    }
}
