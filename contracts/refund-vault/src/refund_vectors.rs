// GENERATED FILE — do not edit by hand.
//
// Emitted by packages/sdk/scripts/generate-refund-vectors.mjs in the accensa-app repo,
// from the same source of truth as packages/sdk/refund-vectors.json. The
// TypeScript SDK and this contract are tested against byte-identical vectors,
// so any divergence between the two implementations fails one of the suites.
//
// To regenerate:
//   node packages/sdk/scripts/generate-refund-vectors.mjs   # in accensa-app
//   cp packages/sdk/refund_vectors.rs \
//      ../accensa-contracts/contracts/refund-vault/src/refund_vectors.rs

#[allow(dead_code)]
pub struct RefundVector {
    pub name: &'static str,
    pub payment_id: &'static str,
    pub payment_ref: [u8; 32],
    pub amount: i128,
    pub paid_at_ledger: u32,
    pub tx_hash: Option<&'static str>,
    pub expected_success: bool,
}

#[rustfmt::skip]
pub const VECTORS: &[RefundVector] = &[
    RefundVector {
        // Live testnet initialization/deployment refund transaction
        // Contract ID: CCMBM44EJUGD52G4LSMGHSXMAH2KSAQZX7VOYY4TTBF5BK4D7M4IHRQA
        name: "live testnet refund — valid refund reference",
        payment_id: "pay_testnet_live_001",
        payment_ref: [
            0x5c, 0x77, 0xfc, 0x34, 0x69, 0x43, 0xf5, 0x6e,
            0x10, 0xfc, 0x36, 0x66, 0xf4, 0x64, 0x02, 0x11,
            0xd7, 0x21, 0xc1, 0x75, 0x48, 0x86, 0xf1, 0x07,
            0xaa, 0xc9, 0xfa, 0x69, 0x68, 0x97, 0x66, 0x2e,
        ],
        amount: 100_000,
        paid_at_ledger: 100,
        tx_hash: Some("5c77fc346943f56e10fc3666f4640211d721c1754886f107aac9fa696897662e"),
        expected_success: true,
    },
    RefundVector {
        name: "standard refund — valid payment reference",
        payment_id: "pay_standard_002",
        payment_ref: [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ],
        amount: 50_000,
        paid_at_ledger: 105,
        tx_hash: None,
        expected_success: true,
    },
    RefundVector {
        name: "invalid refund — zero amount rejected",
        payment_id: "pay_invalid_zero_003",
        payment_ref: [
            0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11,
            0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
            0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11,
            0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
        ],
        amount: 0,
        paid_at_ledger: 105,
        tx_hash: None,
        expected_success: false,
    },
];
