use soroban_sdk::{contracttype, Address};

/// Basis-point denominator: 10_000 bps = 100%.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Largest accepted slippage tolerance (100%): a settlement may never exceed
/// twice the signed cap.
pub const MAX_SLIPPAGE_BPS: u32 = BPS_DENOMINATOR;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRecord {
    pub from: Address,
    pub to: Address,
    pub cap: i128,
    pub expiry: u32,
    pub consumed: bool,
    /// Slippage the buyer tolerates above `cap`, in basis points. `0` keeps
    /// the strict `actual <= cap` rule.
    pub max_slippage_bps: u32,
}

impl AuthorizationRecord {
    /// The most `settle` may charge against this authorization. Always
    /// `Some` for a stored record: `authorize` rejects inputs that overflow.
    pub fn max_settleable(&self) -> Option<i128> {
        max_settleable(self.cap, self.max_slippage_bps)
    }
}

/// `cap + floor(cap * bps / 10_000)`, or `None` if `cap` is negative, `bps`
/// exceeds [`MAX_SLIPPAGE_BPS`], or the sum overflows `i128`.
///
/// `cap * bps` is never formed directly — it overflows for caps above
/// `i128::MAX / 10_000`. Splitting `cap = q * 10_000 + r` gives
/// `cap * bps / 10_000 = q * bps + r * bps / 10_000`, which is exact (the
/// floor falls only on the remainder term) and whose terms are each bounded
/// by `cap` because `bps <= 10_000`. Rounding down favours the buyer.
pub fn max_settleable(cap: i128, bps: u32) -> Option<i128> {
    if cap < 0 || bps > MAX_SLIPPAGE_BPS {
        return None;
    }
    let denom = BPS_DENOMINATOR as i128;
    let bps = bps as i128;
    let tolerance = (cap / denom) * bps + (cap % denom) * bps / denom;
    cap.checked_add(tolerance)
}
