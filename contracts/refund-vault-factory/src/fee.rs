use soroban_sdk::Env;

pub fn extract_fee(_env: &Env, amount: i128, tier: u32) -> i128 {
    let fee_bps = match tier {
        1 => 300, // 3%
        2 => 200, // 2%
        3 => 100, // 1%
        _ => 500, // 5% default
    };
    (amount * fee_bps) / 10000
}
