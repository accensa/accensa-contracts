use soroban_sdk::{contracttype, Address};

/// Basis-point denominator: 10_000 bps = 100%.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Largest accepted slippage tolerance (100%): a settlement may never exceed
/// twice the signed cap.
pub const MAX_SLIPPAGE_BPS: u32 = BPS_DENOMINATOR;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRecord {
    /// The buyer whose tokens the authorization locks: the SEP-41 allowance
    /// is granted from and spent by this address, whose `require_auth` gates
    /// both the authorization and its settlement. Written by
    /// `authorize_with_slippage`; read by `settle` (auth check and
    /// `transfer_from` source) and `inactivity::cancel_inactive_escrow`
    /// (buyer auth for the release).
    pub from: Address,
    /// The recipient paid on settlement, fixed at authorization time.
    /// Written by `authorize_with_slippage`; read by `settle` as the
    /// `transfer_from` destination — the only code path that spends the
    /// authorization pays this address.
    pub to: Address,
    /// Payment cap, as an `i128` token amount in the amount unit of the
    /// token bound by `initialize` (the same unit passed to SEP-41
    /// `approve`/`transfer_from`; the contract names no subtype). Must be
    /// positive — `authorize_with_slippage` rejects `cap <= 0`. Written
    /// there; read by `settle` via `max_settleable` (bound on `actual`) and
    /// returned as the released amount by `inactivity::cancel_inactive_escrow`.
    pub cap: i128,
    /// Ledger sequence number (`env.ledger().sequence()` domain) until which
    /// the authorization stays settleable. Written by
    /// `authorize_with_slippage` from the caller-supplied argument, which is
    /// also passed as the allowance lifetime to the token's `approve`; read
    /// by `settle` (`Error::Expired` once `current_ledger > expiry`) and by
    /// the re-authorization check in `authorize_with_slippage`.
    pub expiry: u32,
    /// One-shot flag: `false` while the cap is still spendable, `true` once
    /// `settle` has transferred the actual amount. Written `false` by
    /// `authorize_with_slippage` and flipped to `true` by `settle`; read by
    /// `settle`, `authorize_with_slippage` and
    /// `inactivity::cancel_inactive_escrow` to refuse a second spend.
    pub consumed: bool,
    /// Slippage the buyer tolerates above `cap`, in basis points. `0` keeps
    /// the strict `actual <= cap` rule.
    pub max_slippage_bps: u32,
    /// Ledger at which the authorization was created (issue #435). Anchors
    /// the dormancy window that `cancel_inactive_escrow` measures.
    pub created_ledger: u32,
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
