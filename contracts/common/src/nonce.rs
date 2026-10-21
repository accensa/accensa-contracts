use soroban_sdk::{contracttype, BytesN};

/// Cryptographic Nonce Revocation Bitmap
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonceBitmap {
    pub bitmap: BytesN<32>,
}

impl NonceBitmap {
    pub fn is_revoked(&self, nonce: u64) -> bool {
        let bit_index = nonce % 256;
        let byte_index = bit_index / 8;
        let bit_in_byte = bit_index % 8;

        let byte = self.bitmap.get(byte_index as u32).unwrap_or(0);
        (byte & (1 << bit_in_byte)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    /// Wraps a 32-byte image in a `NonceBitmap`.
    fn bitmap_from(env: &Env, bytes: [u8; 32]) -> NonceBitmap {
        NonceBitmap {
            bitmap: BytesN::from_array(env, &bytes),
        }
    }

    /// (a) LSB-first ordering across every one of the 32 bytes: bit `i` of
    /// the 256-bit image is nonce `i` — byte `i / 8`, bit `i % 8` counting
    /// from the *least* significant bit. For each single-bit image only the
    /// nonce that maps onto that bit reads as revoked, for every bit
    /// position 0..=255.
    #[test]
    fn is_revoked_reads_every_bit_lsb_first() {
        let env = Env::default();
        for i in 0..256u64 {
            let mut bytes = [0u8; 32];
            bytes[(i / 8) as usize] = 1u8 << (i % 8);
            let bitmap = bitmap_from(&env, bytes);
            for j in 0..256u64 {
                assert_eq!(
                    bitmap.is_revoked(j),
                    i == j,
                    "bit {i} must revoke nonce {j} only when j == i (LSB-first)"
                );
            }
        }
    }

    /// (a, explicit endianness) Byte `0x01` revokes nonce 0 — the least
    /// significant bit — and not nonce 7; byte `0x80` revokes nonce 7 and
    /// not nonce 0.
    #[test]
    fn is_revoked_bit_zero_is_the_least_significant_bit() {
        let env = Env::default();

        let mut low = [0u8; 32];
        low[0] = 0x01;
        let low = bitmap_from(&env, low);
        assert!(low.is_revoked(0));
        assert!(!low.is_revoked(7));

        let mut high = [0u8; 32];
        high[0] = 0x80;
        let high = bitmap_from(&env, high);
        assert!(high.is_revoked(7));
        assert!(!high.is_revoked(0));
    }

    /// (b) 256-slot aliasing: `nonce % 256` means bit 0 serves nonces 0,
    /// 256, 512, … and bit 255 serves 255, 511, 767, …. Setting one bit
    /// revokes exactly its residue class mod 256 and nothing else; the wrap
    /// is recorded here rather than discovered later.
    #[test]
    fn is_revoked_aliases_every_256_nonces() {
        let env = Env::default();

        let mut bytes = [0u8; 32];
        bytes[0] = 0x01; // nonce 0 → aliases 256, 512, 768, …
        bytes[31] = 0x80; // nonce 255 → aliases 511, 767, …
        let bitmap = bitmap_from(&env, bytes);

        assert!(bitmap.is_revoked(0));
        assert!(bitmap.is_revoked(256));
        assert!(bitmap.is_revoked(512));
        for k in 0..8u64 {
            assert!(
                bitmap.is_revoked(256 * k),
                "bit 0 must alias to nonce {}",
                256 * k
            );
            assert!(
                bitmap.is_revoked(255 + 256 * k),
                "bit 255 must alias to nonce {}",
                255 + 256 * k
            );
            assert!(
                !bitmap.is_revoked(1 + 256 * k),
                "nonce {} must stay unrevoked",
                1 + 256 * k
            );
            assert!(
                !bitmap.is_revoked(254 + 256 * k),
                "nonce {} must stay unrevoked",
                254 + 256 * k
            );
        }

        // The answer for any nonce depends only on `nonce % 256`.
        for nonce in [2u64, 42, 250] {
            for k in 0..4u64 {
                assert_eq!(
                    bitmap.is_revoked(nonce + 256 * k),
                    bitmap.is_revoked(nonce),
                    "nonce {} must alias to nonce {nonce}",
                    nonce + 256 * k
                );
            }
        }
    }

    /// (c) Out-of-range default: `is_revoked` reads through
    /// `bitmap.get(byte_index).unwrap_or(0)`, so a byte index past the end
    /// of the image reads as `0` — not revoked — instead of panicking. With
    /// `BytesN<32>` and `nonce % 256`, `byte_index` is mathematically
    /// confined to `0..=31`, so for a well-formed bitmap the fallback arm
    /// cannot fire; what this pins is that no nonce — including the `u64`
    /// extremes, which alias onto the very last byte and bit — can select
    /// an index past the end, panic, or misread, and that the very next
    /// index is exactly what the default would see.
    #[test]
    fn is_revoked_never_reads_past_the_end_and_defaults_to_not_revoked() {
        let env = Env::default();

        let empty = bitmap_from(&env, [0u8; 32]);
        for nonce in [0u64, 255, 256, 65_535, u64::MAX - 1, u64::MAX] {
            assert!(
                !empty.is_revoked(nonce),
                "a fresh bitmap must read not-revoked for nonce {nonce}"
            );
        }

        // `u64::MAX % 256 == 255`: the extreme nonce aliases onto bit 255
        // (byte 31) — the last bit of the image.
        let mut bytes = [0u8; 32];
        bytes[31] = 0x80;
        let top = bitmap_from(&env, bytes);
        assert!(top.is_revoked(u64::MAX), "u64::MAX must alias onto bit 255");
        assert!(!top.is_revoked(u64::MAX - 1));

        // One past the end — the exact index the `unwrap_or(0)` default
        // exists to absorb.
        assert_eq!(top.bitmap.get(32), None);
    }

    /// (d) Consistency with `state-channel`'s `NonceWindow` (issue #570):
    /// `contracts/state-channel/src/nonce.rs` documents its bitmap as "the
    /// same bit order as `accensa_common::nonce::NonceBitmap`", and
    /// `NonceWindow::is_consumed` reads `bytes[offset / 8] &
    /// (1u8 << (offset % 8))` with `offset = nonce - base`. At `base = 0`
    /// that formula is restated verbatim below as the reference. A direct
    /// type-level cross-check is not possible from this crate:
    /// `state-channel` depends on `accensa-common`, not the other way
    /// around, and this issue rules out new dependencies — so the layout
    /// agreement is pinned by re-implementing the window's read and
    /// checking it against `is_revoked` for every bit position under
    /// several bitmap images.
    fn nonce_window_read_at_base_zero(bytes: &[u8; 32], nonce: u64) -> bool {
        let byte = bytes[(nonce / 8) as usize];
        (byte & (1u8 << (nonce % 8))) != 0
    }

    #[test]
    fn bitmap_matches_state_channel_nonce_window_bit_layout() {
        let env = Env::default();

        let mut gradient = [0u8; 32];
        for (i, byte) in gradient.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(37) ^ 0x5A;
        }
        let images: [[u8; 32]; 3] = [[0u8; 32], [0xAA; 32], gradient];

        for bytes in images {
            let bitmap = bitmap_from(&env, bytes);
            for nonce in 0..256u64 {
                assert_eq!(
                    bitmap.is_revoked(nonce),
                    nonce_window_read_at_base_zero(&bytes, nonce),
                    "is_revoked must agree with NonceWindow's bit layout at nonce {nonce}"
                );
            }
        }
    }
}
