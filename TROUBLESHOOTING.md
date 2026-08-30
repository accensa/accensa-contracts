# Deployment Troubleshooting

When deploying the Accensa smart contracts to the Stellar testnet or mainnet, you might encounter some common errors. This guide outlines the most frequent deployment issues and their solutions.

### 1. `tx_bad_seq` (Transaction Bad Sequence)

**Symptom:**
```text
error: transaction failed: tx_bad_seq
```

**Cause:**
The sequence number of your transaction is stale. This typically happens when you submit multiple transactions in rapid succession, and the network hasn't processed the previous one yet, or your local sequence number is out of sync with the network.

**Solution:**
- Wait a few seconds for the previous transaction to clear the ledger.
- If you are deploying via a script or CLI, ensure it fetches the latest sequence number for your identity before submitting.

### 2. `insufficient fees` or `tx_insufficient_fee`

**Symptom:**
```text
error: transaction failed: tx_insufficient_fee
```
*(or similar errors indicating the provided fee is too low for the current network surge)*

**Cause:**
The Stellar network is experiencing higher than normal activity, driving up the inclusion fee. The default or specified fee in your deployment command is not high enough to get the transaction included in the current ledger.

**Solution:**
- Increase the fee parameter in your deployment command (e.g., passing `--fee 100000` to the Soroban CLI).
- Ensure your deployment account has enough XLM to cover both the fees and the base reserve requirements.

### 3. `op_bad_auth` (Bad Authorization)

**Symptom:**
```text
error: transaction failed: op_bad_auth
```

**Cause:**
The transaction is missing a required signature, or the signature provided does not match the source account attempting to execute the operation. This often happens if the identity executing the `initialize` function is not the one configured as the admin.

**Solution:**
- Verify that the `--source` identity you are using in the CLI matches the account expected by the contract.
- If you are trying to call an admin-only function (like `deposit` or `propose_policy`), ensure you are signing with the correct merchant identity.

### 4. `HostError` during Refund or Withdraw

**Symptom:**
```text
error: transaction failed: HostError
```
*(or similar unhandled errors when calling `refund` or `withdraw`)*

**Cause:**
The recipient address (in a `refund`) or the destination address (in a `withdraw`) does not have a trustline established for the configured Stellar Asset Contract token (e.g., USDC). The underlying token contract panics and reverts the transaction when attempting to transfer to an account without a trustline.

**Solution:**
- Ensure that the recipient account has a valid trustline for the asset configured in the vault.
- For merchants calling `withdraw`, verify that the destination wallet has established the required trustline before initiating the withdrawal.
- `RefundVault` intentionally bubbles up this token-level panic rather than pre-checking trustlines, as a pre-check would consume extra computation budget on every successful refund.
