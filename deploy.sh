#!/bin/bash
set -e

echo "🚀 Building contracts..."
stellar contract build

echo "🔑 Setting up deployer identity..."
stellar keys generate deployer --network testnet 2>/dev/null || true
DEPLOYER=$(stellar keys address deployer)
echo "Deployer address: $DEPLOYER"

echo "💎 Funding deployer..."
stellar keys fund deployer --network testnet || true

echo "🚢 Deploying ReceiptAnchor..."
ANCHOR_ID=$(stellar contract deploy --wasm target/wasm32v1-none/release/receipt_anchor.wasm --source deployer --network testnet)
echo "ReceiptAnchor ID: $ANCHOR_ID"

echo "🚢 Deploying RefundVault..."
VAULT_ID=$(stellar contract deploy --wasm target/wasm32v1-none/release/refund_vault.wasm --source deployer --network testnet)
echo "RefundVault ID: $VAULT_ID"

echo "⚙️ Initializing ReceiptAnchor..."
stellar contract invoke --id $ANCHOR_ID --source deployer --network testnet -- initialize --merchant $DEPLOYER
echo "✅ ReceiptAnchor initialized."

echo "⚙️ Initializing RefundVault..."
# Using Testnet Native XLM as the default testing token (can be changed to USDC later)
NATIVE_ASSET="CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC"
# 17280 ledgers is approximately 1 day (5 seconds per ledger)
stellar contract invoke --id $VAULT_ID --source deployer --network testnet -- initialize --merchant $DEPLOYER --token $NATIVE_ASSET --refund_window_ledgers 17280
echo "✅ RefundVault initialized."

echo ""
echo "==========================================================="
echo "🎉 DEPLOYMENT COMPLETE 🎉"
echo "==========================================================="
echo "NEXT_PUBLIC_RECEIPT_ANCHOR_ID=$ANCHOR_ID"
echo "NEXT_PUBLIC_REFUND_VAULT_ID=$VAULT_ID"
echo "MERCHANT_ADDRESS=$DEPLOYER"
echo "==========================================================="
echo "Copy these values into your accensa-app/apps/web/.env.local and your Go Indexer environment!"
