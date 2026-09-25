//! Integer square root implementation for quadratic voting.
//!
//! Uses Newton-Raphson iteration to compute `isqrt(n)` without floating
//! point operations, ensuring precision for token balances with 7 decimal
//! places (Stellar native standard).

/// Compute the integer square root of `n` using Newton-Raphson iteration.
///
/// Returns `floor(sqrt(n))`. Handles overflow protection for `u64` inputs.
///
/// # Newton-Raphson Method
///
/// The iteration `x_{k+1} = (x_k + n / x_k) / 2` converges quadratically
/// to `sqrt(n)`. We start with an initial estimate and iterate until
/// convergence.
///
/// # Examples
///
/// ```ignore
/// assert_eq!(isqrt(0), 0);
/// assert_eq!(isqrt(1), 1);
/// assert_eq!(isqrt(4), 2);
/// assert_eq!(isqrt(9), 3);
/// assert_eq!(isqrt(100), 10);
/// ```
pub fn isqrt(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    if n < 4 {
        return 1;
    }

    // Initial estimate: use bit length to get a reasonable starting point
    let leading_zeros = n.leading_zeros();
    let bit_len = 64 - leading_zeros;
    let mut x: u64 = if bit_len <= 2 {
        1
    } else {
        1u64 << ((bit_len + 1) / 2)
    };

    // Newton-Raphson iteration
    loop {
        let next_x = x.saturating_add(n / x) / 2;
        if next_x >= x {
            break x;
        }
        x = next_x;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isqrt_basic() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(2), 1);
        assert_eq!(isqrt(3), 1);
        assert_eq!(isqrt(4), 2);
        assert_eq!(isqrt(8), 2);
        assert_eq!(isqrt(9), 3);
        assert_eq!(isqrt(15), 3);
        assert_eq!(isqrt(16), 4);
        assert_eq!(isqrt(100), 10);
    }

    #[test]
    fn test_isqrt_perfect_squares() {
        for i in 0..=1_000_000u64 {
            let sqrt_i = isqrt(i);
            assert!(
                sqrt_i * sqrt_i <= i,
                "isqrt({}) = {} but {}^2 = {} > {}",
                i,
                sqrt_i,
                sqrt_i,
                sqrt_i * sqrt_i,
                i
            );
            let next = sqrt_i + 1;
            assert!(
                next * next > i,
                "isqrt({}) = {} but {}^2 = {} <= {}",
                i,
                sqrt_i,
                next,
                next * next,
                i
            );
        }
    }

    #[test]
    fn test_isqrt_large_values() {
        assert_eq!(isqrt(u64::MAX), 4294967295);
        assert_eq!(isqrt(1_000_000_000_000), 1000000);
        assert_eq!(isqrt(1_000_000_000_000_000), 31622776);
    }

    #[test]
    fn test_isqrt_with_decimal_precision() {
        // Token balances with 7 decimal places (Stellar native standard)
        // e.g., 1_0000000 units = 1 token
        assert_eq!(isqrt(1_0000000), 3162);
        assert_eq!(isqrt(100_000000), 10000);
        // 10 million tokens (with 7 decimals) -> sqrt ~ 3162277
        assert_eq!(isqrt(10_0000000), 100000);
    }
}
