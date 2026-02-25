//! Redeems resolved Polymarket positions through a GnosisSafe proxy wallet.
//!
//! When `signature_type = 2` (GnosisSafe), tokens live in the Safe proxy wallet,
//! not the EOA. The `redeemPositions()` function redeems for `msg.sender`, so the
//! call must originate FROM the Safe. This module encodes CTF calldata and routes it
//! through `GnosisSafe.execTransaction()`.

use alloy::primitives::{Address, Bytes, B256, U256, address};
use alloy::providers::Provider;
use alloy::signers::Signer;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::SolCall;

use crate::ctf_executor::TxResult;

// Polygon mainnet contract addresses
const CONDITIONAL_TOKENS: Address = address!("0x4D97DCd97eC945f40cF65F87097ACe5EA0476045");
const NEG_RISK_ADAPTER: Address = address!("0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296");
const USDC: Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

sol! {
    #[sol(rpc)]
    interface IGnosisSafe {
        function execTransaction(
            address to,
            uint256 value,
            bytes calldata data,
            uint8 operation,
            uint256 safeTxGas,
            uint256 baseGas,
            uint256 gasPrice,
            address gasToken,
            address payable refundReceiver,
            bytes memory signatures
        ) external payable returns (bool success);

        function nonce() external view returns (uint256);

        function getTransactionHash(
            address to,
            uint256 value,
            bytes memory data,
            uint8 operation,
            uint256 safeTxGas,
            uint256 baseGas,
            uint256 gasPrice,
            address gasToken,
            address refundReceiver,
            uint256 _nonce
        ) external view returns (bytes32);

        function getOwners() external view returns (address[] memory);
        function getThreshold() external view returns (uint256);
    }
}

// Calldata encoders — only the function signatures, no contract instance needed.
sol! {
    function redeemPositionsCTF(
        address collateralToken,
        bytes32 parentCollectionId,
        bytes32 conditionId,
        uint256[] calldata indexSets
    ) external;

    function redeemPositionsNR(
        bytes32 conditionId,
        uint256[] calldata amounts
    ) external;
}

/// Redeems resolved positions by routing calls through a GnosisSafe proxy wallet.
///
/// For a 1-of-1 Safe where the EOA is the sole owner, we:
/// 1. Encode the CTF `redeemPositions` calldata
/// 2. Get the Safe's nonce and compute the transaction hash
/// 3. Sign the hash with the EOA private key
/// 4. Call `Safe.execTransaction()` — the Safe becomes `msg.sender` for the CTF call
pub struct SafeRedeemer<P: Provider + Clone> {
    safe: IGnosisSafe::IGnosisSafeInstance<P>,
    signer: PrivateKeySigner,
    safe_address: Address,
}

impl<P: Provider + Clone> SafeRedeemer<P> {
    pub fn new(provider: P, signer: PrivateKeySigner, safe_address: Address) -> Self {
        let safe = IGnosisSafe::new(safe_address, provider);
        Self {
            safe,
            signer,
            safe_address,
        }
    }

    /// Verify the EOA signer is an owner of the Safe. Call at startup for diagnostics.
    pub async fn verify_ownership(&self) -> anyhow::Result<bool> {
        let signer_addr = self.signer.address();
        let owners = self.safe.getOwners().call().await?;
        let threshold = self.safe.getThreshold().call().await?;

        tracing::info!(
            safe = %self.safe_address,
            signer = %signer_addr,
            owners = ?owners,
            threshold = %threshold,
            "GnosisSafe ownership check"
        );

        let is_owner = owners.iter().any(|o| *o == signer_addr);
        if !is_owner {
            tracing::error!(
                signer = %signer_addr,
                owners = ?owners,
                "EOA is NOT an owner of the Safe — execTransaction will fail with GS013"
            );
        }
        Ok(is_owner)
    }

    /// Redeem a standard (non-NegRisk) binary market position.
    pub async fn redeem(&self, condition_id: B256) -> anyhow::Result<TxResult> {
        tracing::info!(
            condition_id = %condition_id,
            safe = %self.safe_address,
            "Redeeming via GnosisSafe"
        );

        // Encode redeemPositions(collateral, parentCollectionId=0, conditionId, [1,2])
        let calldata = redeemPositionsCTFCall {
            collateralToken: USDC,
            parentCollectionId: B256::ZERO,
            conditionId: condition_id,
            indexSets: vec![U256::from(1), U256::from(2)],
        }
        .abi_encode();

        self.exec_through_safe(CONDITIONAL_TOKENS, Bytes::from(calldata)).await
    }

    /// Redeem a NegRisk market position.
    pub async fn redeem_neg_risk(
        &self,
        condition_id: B256,
        amounts: Vec<U256>,
    ) -> anyhow::Result<TxResult> {
        tracing::info!(
            condition_id = %condition_id,
            safe = %self.safe_address,
            amounts_len = amounts.len(),
            "Redeeming NegRisk via GnosisSafe"
        );

        let calldata = redeemPositionsNRCall {
            conditionId: condition_id,
            amounts,
        }
        .abi_encode();

        self.exec_through_safe(NEG_RISK_ADAPTER, Bytes::from(calldata)).await
    }

    /// Execute an arbitrary call through the GnosisSafe's `execTransaction`.
    async fn exec_through_safe(
        &self,
        to: Address,
        data: Bytes,
    ) -> anyhow::Result<TxResult> {
        let zero = U256::ZERO;
        let zero_addr = Address::ZERO;

        // 1. Get current Safe nonce
        let nonce = self.safe.nonce().call().await?;

        // 2. Compute Safe transaction hash
        let tx_hash = self.safe.getTransactionHash(
            to,
            zero,          // value
            data.clone(),
            0u8,           // operation: Call
            zero,          // safeTxGas
            zero,          // baseGas
            zero,          // gasPrice
            zero_addr,     // gasToken
            zero_addr,     // refundReceiver
            nonce,
        )
        .call()
        .await?;

        tracing::debug!(
            nonce = %nonce,
            tx_hash = ?tx_hash,
            signer = %self.signer.address(),
            "Safe transaction hash computed"
        );

        // 3. Sign the hash with the EOA private key (eth_sign mode: v > 30)
        // GnosisSafe supports two ECDSA modes:
        //   v=27/28: ecrecover(dataHash, v, r, s)
        //   v>30 (eth_sign): ecrecover(keccak256("\x19Ethereum Signed Message:\n32" + dataHash), v-4, r, s)
        // We use eth_sign mode because it's the standard used by MetaMask/browser wallets
        // and is more widely compatible with Safe implementations.
        let signature = self.signer.sign_message(tx_hash.as_slice()).await?;
        let sig_bytes = {
            let mut buf = [0u8; 65];
            buf[..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
            buf[32..64].copy_from_slice(&signature.s().to_be_bytes::<32>());
            // eth_sign: v = original_v + 4 (27→31, 28→32)
            buf[64] = if signature.v() { 32 } else { 31 };
            Bytes::from(buf.to_vec())
        };

        // 4. Send execTransaction through the Safe
        let receipt = self.safe.execTransaction(
            to,
            zero,
            data,
            0u8,
            zero,
            zero,
            zero,
            zero_addr,
            zero_addr,
            sig_bytes,
        )
        .send()
        .await?
        .get_receipt()
        .await?;

        tracing::info!(
            tx_hash = %receipt.transaction_hash,
            block = receipt.block_number.unwrap_or(0),
            "Safe execTransaction confirmed"
        );

        Ok(TxResult {
            tx_hash: receipt.transaction_hash,
            block_number: receipt.block_number.unwrap_or(0),
        })
    }
}
