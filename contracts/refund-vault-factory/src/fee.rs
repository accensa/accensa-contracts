use crate::Error;
use soroban_sdk::Env;

pub const MAX_FEE_BPS: u32 = 500; // 5% hardcap

pub fn validate_fee_cap(_env: &Env, fee_bps: u32) -> Result<(), Error> {
    if fee_bps > MAX_FEE_BPS {
        return Err(Error::FeeExceedsHardcap);
    }
    Ok(())
}

#[allow(dead_code)]
pub fn extract_fee(_env: &Env, amount: i128, tier: u32) -> i128 {
    let fee_bps = match tier {
        1 => 300,         // 3%
        2 => 200,         // 2%
        3 => 100,         // 1%
        _ => MAX_FEE_BPS, // 5% default
    };
    (amount * (fee_bps as i128)) / 10000
}
