use soroban_sdk::Env;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BudgetMetrics {
    pub cpu_instructions: u64,
    pub memory_bytes: u64,
}

/// A generic measurement utility for tracking resource consumption of a specific closure.
/// This allows tests to isolate the CPU and memory cost of individual contract invocations,
/// separating setup costs from actual execution costs.
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
