//! Dispute fallback oracle (issue #469).
//! ... (rest of file unchanged)
#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{testutils::Address as _, xdr::FromXdr, BytesN, Env};

    fn test_env() -> Env {
        let env = Env::default();
        env.mock_all_auths();
        env
    }

    fn payment_ref(env: &Env, slot: u8) -> BytesN<32> {
        BytesN::from_array(env, &[slot; 32])
    }

    #[test]
    fn dispute_can_be_escalated_and_queried() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 1),
            &recipient,
            &100,
        );
        assert_eq!(dispute_id, 1);

        let dispute = client.get_fallback_dispute(&dispute_id);
        assert!(dispute.open);
        assert_eq!(dispute.merchant, merchant);
        assert_eq!(dispute.oracle, oracle);
        assert_eq!(dispute.recipient, recipient);
        assert_eq!(dispute.amount, 100);
    }

    #[test]
    fn oracle_escalation_assigns_monotonic_ids() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let a = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 1),
            &recipient,
            &10,
        );
        let b = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 2),
            &recipient,
            &10,
        );
        assert_ne!(a, b);
        assert_eq!(a + 1, b);
    }

    #[test]
    fn oracle_request_payload_decodes_to_the_dispute() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 3),
            &recipient,
            &77,
        );
        let payload = client.build_fallback_oracle_request(&dispute_id);
        let decoded = FallbackDispute::from_xdr(&env, &payload).unwrap();
        assert_eq!(decoded.recipient, recipient);
        assert_eq!(decoded.amount, 77);

        assert_eq!(
            client.try_get_fallback_dispute(&99),
            Err(Ok(accensa_common::Error::DisputeNotFound))
        );
    }

    #[test]
    fn oracle_settles_a_dispute_exactly_once() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 4),
            &recipient,
            &50,
        );

        // Ruling: refund.
        client.settle_fallback_dispute(&dispute_id, &true);
        assert!(!client.get_fallback_dispute(&dispute_id).open);

        // A dispute settles exactly once.
        assert_eq!(
            client.try_settle_fallback_dispute(&dispute_id, &false),
            Err(Ok(accensa_common::Error::DisputeClosed))
        );
    }

    // ─── New tests below ────────────────────────────────────────────────

    #[test]
    fn fallback_dispute_requested_event_has_exact_topic_and_payload() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 1),
            &recipient,
            &100,
        );

        // Read events published by the contract.
        let events = env.events().all().filter_by_contract(&client.address);

        // The requested event must be present with the exact topic tuple:
        // ("fallback_dispute_requested_event", dispute_id)
        let requested_events: Vec<_> = events
            .events()
            .iter()
            .filter(|e| e.name() == "fallback_dispute_requested_event")
            .collect();
        assert_eq!(requested_events.len(), 1, "Expected exactly one FallbackDisputeRequestedEvent");

        let event = &requested_events[0];

        // Compare by XDR as the hashlock test does: serialize and compare.
        let expected = FallbackDisputeRequestedEvent {
            dispute_id,
            oracle: oracle.clone(),
            payment_ref: payment_ref(&env, 1),
            merchant: merchant.clone(),
            recipient: recipient.clone(),
            amount: 100,
        };
        assert_eq!(&event.to_xdr(&env, &client.address), &expected.to_xdr(&env, &client.address));

        // Verify each topic/payload field explicitly.
        // Topics are emitted as the first N fields; for this event the topic is
        // ("fallback_dispute_requested_event", dispute_id).
        assert_eq!(event.name(), "fallback_dispute_requested_event");
        // The topic's second element is the dispute_id; pull it from the event's
        // indexed data via the XDR representation or the event directly.
        // Since dispute_id is a #[topic] field, it appears as the first topic.
        // We assert the full payload matches the struct.
        assert_eq!(
            (event.oracle(), event.payment_ref(), event.merchant(), event.recipient(), event.amount()),
            (oracle, payment_ref(&env, 1), merchant, recipient, 100)
        );
    }

    #[test]
    fn fallback_dispute_settled_event_distinguishes_refund_branches() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 2),
            &recipient,
            &75,
        );

        // Settle with refund = true (refund owed).
        client.settle_fallback_dispute(&dispute_id, &true);

        let events = env.events().all().filter_by_contract(&client.address);
        let settled_events: Vec<_> = events
            .events()
            .iter()
            .filter(|e| e.name() == "fallback_dispute_settled_event")
            .collect();
        assert_eq!(settled_events.len(), 1, "Expected exactly one FallbackDisputeSettledEvent");

        let event_true = &settled_events[0];

        // Verify event name and topic.
        assert_eq!(event_true.name(), "fallback_dispute_settled_event");

        // Settle a second dispute with refund = false (denied).
        let dispute_id2 = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 3),
            &recipient,
            &30,
        );
        client.settle_fallback_dispute(&dispute_id2, &false);

        let events2 = env.events().all().filter_by_contract(&client.address);
        let settled_events2: Vec<_> = events2
            .events()
            .iter()
            .filter(|e| e.name() == "fallback_dispute_settled_event")
            .collect();
        assert_eq!(settled_events2.len(), 2, "Expected two FallbackDisputeSettledEvents");

        let event_false = &settled_events2[1];

        // Both events share the same topic tuple structure
        // ("fallback_dispute_settled_event", dispute_id), but differ in refund.
        // Assert the full XDR payloads differ in the refund field.
        let expected_true = FallbackDisputeSettledEvent {
            dispute_id: dispute_id,
            refund: true,
            oracle: oracle.clone(),
            payment_ref: payment_ref(&env, 2),
        };
        let expected_false = FallbackDisputeSettledEvent {
            dispute_id: dispute_id2,
            refund: false,
            oracle: oracle,
            payment_ref: payment_ref(&env, 3),
        };

        assert_eq!(&event_true.to_xdr(&env, &client.address), &expected_true.to_xdr(&env, &client.address));
        assert_eq!(&event_false.to_xdr(&env, &client.address), &expected_false.to_xdr(&env, &client.address));

        // Explicitly assert the refund values are distinguishable.
        assert_eq!(event_true.refund(), true);
        assert_eq!(event_false.refund(), false);
    }

    #[test]
    fn fallback_dispute_settle_exactly_once_no_second_event() {
        let env = test_env();
        let id = env.register(crate::VdfPolicy, ());
        let client = crate::VdfPolicyClient::new(&env, &id);

        let merchant = Address::generate(&env);
        let oracle = Address::generate(&env);
        let recipient = Address::generate(&env);

        let dispute_id = client.request_fallback_dispute(
            &merchant,
            &oracle,
            &payment_ref(&env, 4),
            &recipient,
            &50,
        );

        // First settlement publishes an event.
        client.settle_fallback_dispute(&dispute_id, &true);

        let events_before = env.events().all().filter_by_contract(&client.address);
        let settled_before: Vec<_> = events_before
            .events()
            .iter()
            .filter(|e| e.name() == "fallback_dispute_settled_event")
            .collect();
        assert_eq!(settled_before.len(), 1, "Expected one event after first settlement");

        // Second settlement of the same dispute should return DisputeClosed
        // and NOT publish a new event.
        let result = client.try_settle_fallback_dispute(&dispute_id, &false);
        assert_eq!(
            result,
            Err(Ok(accensa_common::Error::DisputeClosed)),
            "Second settlement should fail with DisputeClosed"
        );

        let events_after = env.events().all().filter_by_contract(&client.address);
        let settled_after: Vec<_> = events_after
            .events()
            .iter()
            .filter(|e| e.name() == "fallback_dispute_settled_event")
            .collect();
        assert_eq!(
            settled_after.len(), 1,
            "Second settlement must not publish a new event; expected exactly 1, got {}",
            settled_after.len()
        );

        // Verify the existing event is still the same one.
        let events_all = env.events().all().filter_by_contract(&client.address);
        assert_eq!(
            events_all.events().len(),
            1,
            "Total event count should remain 1"
        );
    }
}