# Anonymous Governance Voting

Governance accepts Ristretto255 LSAG signatures through
`vote_anonymous(proposal_id, support, ring, signature)`. Members first register
one public voting key with `register_voting_key(member, public_key)`, which
requires the member's normal Soroban authorization. The corresponding secret
key is held off-chain.

## Signing payload

Clients should obtain the bytes from
`get_anonymous_vote_message(proposal_id, support, ring)`. The canonical
preimage is:

```text
"ACCENSA_GOVERNANCE_LSAG_V1"
|| network_id[32]
|| governance_contract_address_xdr
|| proposal_id_be_u64
|| support_u8                 // 0 = no, 1 = yes
|| ring_length_be_u32
|| compressed_ring_key[0]
|| ...
|| compressed_ring_key[n-1]
```

The ordered ring is part of the signed message. Public keys are canonical
32-byte compressed Ristretto points. Rings contain 2 to 32 unique registered
keys, and all keys in a ring must map to members with the same current
quadratic weight. The contract therefore applies the correct existing weight
without learning which ring member signed; the selected weight class is
observable.

## LSAG encoding and hashing

`RingSignature` contains:

- `key_image`: a compressed Ristretto point (32 bytes)
- `initial_challenge`: a canonical scalar (32 bytes)
- `responses`: one canonical scalar (32 bytes) per ring key, in ring order

The signer constructs the key image as `I = x * Hp(context, P)`, where `x` is
the signer's secret scalar and `P = x * G`. The image context is:

```text
"ACCENSA_GOVERNANCE_LSAG_KEY_IMAGE_V1"
|| network_id[32]
|| governance_contract_address_xdr
|| proposal_id_be_u64
|| compressed_public_key[32]
```

`Hp` hashes that context with a trailing `0x00` and `0x01`, concatenates the
two SHA-256 digests into 64 bytes, and maps them with
`RistrettoPoint::from_uniform_bytes`. Including the proposal but excluding the
vote choice makes the image stable for duplicate detection within one
proposal, while avoiding cross-proposal image correlation.

The challenge cycle hashes:

```text
"ACCENSA_GOVERNANCE_LSAG_CHALLENGE_V1"
|| message_length_be_u32
|| message
|| compress(s_i * G + c_i * P_i)
|| compress(s_i * Hp(context, P_i) + c_i * I)
```

The SHA-256 digest is reduced modulo the Ristretto scalar order. Verification
recomputes the cycle for every ring member and accepts only if the final
challenge equals `initial_challenge`. Curve and scalar encodings must be
canonical, and the key image and public keys may not be the identity point.

## Replay and privacy behavior

After all membership, weight, proposal, signature, and duplicate checks pass,
the contract stores `KeyImage(proposal_id, key_image)` in temporary storage
through the proposal deadline, updates its existing yes/no tally, and emits
`AnonymousVoteCast`. No signer address or key image is included in that vote
event or in the per-vote storage key. See [`EVENTS.md`](EVENTS.md#governance-events)
for the event schema.

After the first anonymous vote, the proposal rejects further transparent
votes. Transparent votes submitted earlier remain in the tally, but an
anonymous ring containing any of those addresses is rejected. This prevents a
member from voting through both paths. Anonymous dissent does not create the
address-keyed marker required by the existing ragequit payout flow. A voter
who needs transaction-source privacy must also submit through a relayer: LSAG
hides the signing ring member, not the source account of a public ledger
transaction.

This scheme provides signer ambiguity among the supplied ring keys. It does
not hide vote choice, tally weight, proposal, ring, or transaction timing.
Anonymity also depends on more than the cryptographic proof: a small ring,
unique weight class, off-chain correlation, or direct submission can reduce
practical privacy.
