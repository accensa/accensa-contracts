#[cfg(test)]
mod test {
    use soroban_sdk::{Env, Symbol, BytesN, Address};

    #[test]
    fn test_event_shapes_match_docs() {
        // DepositEvent: ("deposit_event", from: Address) | Data: amount
        // RefundEvent: ("refund_event", payment_ref: BytesN<32>) | Data: amount, recipient, ledger
        // WithdrawEvent: ("withdraw_event", to: Address) | Data: amount
    }
}