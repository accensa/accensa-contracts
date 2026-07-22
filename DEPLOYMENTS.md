# Deployments

Every Accensa contract deployment is recorded here with its contract ID and the
transaction that created it, so anyone can verify the deployment independently
without trusting this repository.

Machine-readable values live in [`deployments/testnet.env`](deployments/testnet.env)
and are produced by [`deploy.sh`](deploy.sh).

## Testnet

Deployed 2026-07-22 with `soroban-sdk` 27.0.0, built for `wasm32v1-none`.

| Contract | Contract ID | Explorer |
|---|---|---|
| `ReceiptAnchor` | `CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV` | [stellar.expert](https://stellar.expert/explorer/testnet/contract/CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV) |
| `RefundVault` | `CCMBM44EJUGD52G4LSMGHSXMAH2KSAQZX7VOYY4TTBF5BK4D7M4IHRQA` | [stellar.expert](https://stellar.expert/explorer/testnet/contract/CCMBM44EJUGD52G4LSMGHSXMAH2KSAQZX7VOYY4TTBF5BK4D7M4IHRQA) |

- **Merchant / admin:** `GCALKSGAZRJLSUEJT3M5W6LN4R7XQOLIRCOS6ZA6EDZVTZDBIIPPFKJ6`
- **Refund token:** native XLM SAC `CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC`
- **Refund window:** 17,280 ledgers (~24h)
- **`ReceiptAnchor` wasm hash:** `f5dc42e6c2821607de6e35ed6e37d49623415e7221a77a290e853970f1a6c7b7`

### Transactions

| Step | Transaction |
|---|---|
| Upload `ReceiptAnchor` wasm | [`137b484d…`](https://stellar.expert/explorer/testnet/tx/137b484dd907c29b61a15ee2c84e8b6209de214f06f60b86c52dc749e4322f1b) |
| Deploy `ReceiptAnchor` | [`da9f4b4a…`](https://stellar.expert/explorer/testnet/tx/da9f4b4afbde8030750b1bf5bc7b9518a8ffc6ce654867aa282374d1360d38db) |
| Deploy `RefundVault` | [`b61ad2a9…`](https://stellar.expert/explorer/testnet/tx/b61ad2a9849b7ad9773bef3b204380b19b6a1a579d0c49898105d05bf2afcf1d) |
| Initialize `ReceiptAnchor` | [`852d7fe1…`](https://stellar.expert/explorer/testnet/tx/852d7fe12dff0117a1e171bc9e3b57f491b4d802eedf2c8cf7d5ba73897cc49d) |
| Initialize `RefundVault` | [`5c77fc34…`](https://stellar.expert/explorer/testnet/tx/5c77fc346943f56e10fc3666f4640211d721c1754886f107aac9fa696897662e) |
| Anchor batch #1 | [`99d0481b…`](https://stellar.expert/explorer/testnet/tx/99d0481bf2b4a00b51f1ca7c3e633d8675dc84ede8eefc6804a00686ff7b8c9a) |

## Verifying the live deployment yourself

Batch #1 is anchored on-chain over four demo receipts. Its Merkle root was computed
off-chain by the TypeScript SDK (`packages/sdk` in
[`accensa-app`](https://github.com/accensa/accensa-app)) and verified on-chain by
`ReceiptAnchor.verify_receipt` — the two implementations agree on the same
sorted-pair SHA-256 convention.

Read the anchored batch:

```bash
stellar contract invoke \
  --id CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV \
  --network testnet --source <your-identity> \
  -- get_batch --batch_id 1
```

Verify a receipt that is in the batch — returns `true`:

```bash
stellar contract invoke \
  --id CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV \
  --network testnet --source <your-identity> \
  -- verify_receipt --batch_id 1 \
  --leaf c476fc0553303ec4275bd4cb50ab7fa8182e343dbc4c721d7e2076fd77a5b56c \
  --proof '["7ca64ee60e2b975f59f2a1f1cc1526d5b001a5c29f70291f316ba1c012a01bd1","1733fad16ada0c23d8cdaff52bea66bea308dddddcb79348842acef0065c9615"]'
```

Verify a forged receipt against the same proof — returns `false`:

```bash
stellar contract invoke \
  --id CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV \
  --network testnet --source <your-identity> \
  -- verify_receipt --batch_id 1 \
  --leaf 16b138aabc889c21114436424e13132bd8928d2c21b4ac5a9ac5198104efb42c \
  --proof '["7ca64ee60e2b975f59f2a1f1cc1526d5b001a5c29f70291f316ba1c012a01bd1","1733fad16ada0c23d8cdaff52bea66bea308dddddcb79348842acef0065c9615"]'
```

Both are read-only simulations and cost nothing to run.

## Redeploying

```bash
./deploy.sh                      # testnet, identity "deployer"
NETWORK=futurenet ./deploy.sh    # another network
TOKEN=<usdc-sac-id> ./deploy.sh  # settle refunds in USDC instead of XLM
```

The script writes `deployments/<network>.env`. Commit that file so the record
stays reproducible.
