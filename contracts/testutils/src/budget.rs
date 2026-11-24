#![cfg(not(target_arch = "wasm32"))]

#[allow(unused_imports)]
use soroban_sdk::{testutils::*, Bytes, Env};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BudgetMetrics {
    pub cpu_instructions: u64,
    pub memory_bytes: u64,
}

/// A generic measurement utility for tracking resource consumption of a specific closure.
///
/// Measures the resource consumption of `f` by computing the non-negative delta
/// (`after.saturating_sub(before)`) between cumulative host budget counters before and after
/// executing the closure.
///
/// Because it computes a relative delta over the cumulative counters rather than resetting them,
/// `f` is measured in isolation without requiring `reset_unlimited()` to precede the call
/// (though callers may still call `reset_unlimited()` if they wish to avoid hitting default budget limits).
/// An empty closure that performs no Soroban host operations measures zero CPU instructions and zero memory bytes.
pub fn measure_resources<F, R>(env: &Env, mut f: F) -> (R, BudgetMetrics)
where
    F: FnMut() -> R,
{
    let cpu_before = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_before = env.cost_estimate().budget().memory_bytes_cost();

    let result = f();

    let cpu_after = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_after = env.cost_estimate().budget().memory_bytes_cost();

    let metrics = BudgetMetrics {
        cpu_instructions: cpu_after.saturating_sub(cpu_before),
        memory_bytes: mem_after.saturating_sub(mem_before),
    };

    (result, metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_closure_measures_zero_resources() {
        let env = Env::default();
        let ((), metrics) = measure_resources(&env, || ());

        assert_eq!(metrics.cpu_instructions, 0);
        assert_eq!(metrics.memory_bytes, 0);
    }

    #[test]
    fn non_empty_closure_records_non_negative_deltas() {
        let env = Env::default();
        // An empty closure returns zero
        let ((), baseline) = measure_resources(&env, || ());
        assert_eq!(baseline.cpu_instructions, 0);
        assert_eq!(baseline.memory_bytes, 0);

        // A closure performing contract environment operations records non-negative deltas
        let (val, metrics) = measure_resources(&env, || {
            let data = Bytes::from_slice(&env, &[1u8, 2, 3, 4]);
            env.crypto().sha256(&data)
        });

        assert_eq!(val.len(), 32);
        assert!(metrics.cpu_instructions > 0);
        assert!(metrics.memory_bytes > 0);
    }

    #[test]
    fn operates_on_deltas_without_requiring_reset() {
        let env = Env::default();

        // Perform some setup work before measurement
        let setup_data = Bytes::from_slice(&env, &[9u8; 64]);
        let _ = env.crypto().sha256(&setup_data);

        // measure_resources measures only the delta of the closure, not the setup work
        let ((), empty_metrics) = measure_resources(&env, || ());
        assert_eq!(empty_metrics.cpu_instructions, 0);
        assert_eq!(empty_metrics.memory_bytes, 0);

        let (result, op_metrics) = measure_resources(&env, || {
            let inner_data = Bytes::from_slice(&env, &[5u8; 16]);
            env.crypto().sha256(&inner_data)
        });
        assert_eq!(result.len(), 32);
        assert!(op_metrics.cpu_instructions > 0);
        assert!(op_metrics.memory_bytes > 0);
    }
}
