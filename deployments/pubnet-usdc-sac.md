# Pubnet USDC SAC address (verified input, not a deployment outcome)

No pubnet deployment has been performed. This note pins the **input** to pass
as `TOKEN` when one is, so the deployer does not have to re-resolve it:

```bash
NETWORK=pubnet TOKEN=CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75 ./deploy.sh
```

**Mainnet USDC SAC address:** `CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75`

Verified 2026-11-23 against two independent sources, which agree on the
same ID:

- Chain data: Horizon's pubnet asset record for `USDC` issued by
  `GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN` publishes
  this `contract_id`:
  <https://horizon.stellar.org/assets?asset_code=USDC&asset_issuer=GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN>
- Issuer-published: Circle's stellar.toml (the `toml` link on that Horizon
  record) pins the same `USDC` asset pair, from which this SAC ID derives
  deterministically:
  <https://circle.com/.well-known/stellar.toml>
- Corroborated by Stellar's CCTP docs, whose USDC SAC table lists the same
  Mainnet address:
  <https://developers.stellar.org/docs/tokens/cross-chain-transfers>

The derivation was checked locally against the SAC algorithm
(`SHA256(XDR(HashIdPreimage::ContractId { network_id, preimage }))`,
pubnet passphrase): the Circle-published pair yields exactly the ID above,
and the same implementation reproduces the known testnet XLM and USDC SACs.

See `docs/MAINNET_DEPLOYMENT.md` (section 4) for the pre-deployment checklist.
This file is documentation; `deploy.sh` writes only `deployments/<network>.env`
and does not read this note.
