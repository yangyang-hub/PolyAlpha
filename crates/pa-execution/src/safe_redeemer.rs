//! Redeems resolved Polymarket positions through a GnosisSafe proxy wallet.
//!
//! When `signature_type = 2` (GnosisSafe), tokens live in the Safe proxy wallet,
//! not the EOA. The `redeemPositions()` function redeems for `msg.sender`, so the
//! call must originate FROM the Safe. This module encodes CTF calldata and routes it
//! through `GnosisSafe.execTransaction()` using ETH_SIGN signatures (v > 30).

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
/// Uses ETH_SIGN signature mode (v > 30) — the same mode the Polymarket website uses.
/// For a 1-of-1 Safe where the EOA is the sole owner, we:
/// 1. Encode the CTF `redeemPositions` calldata
/// 2. Get the Safe's nonce and compute the EIP-712 transaction hash
/// 3. Sign the hash with EIP-191 prefix (eth_sign mode)
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
                "EOA is NOT an owner of the Safe — execTransaction will fail"
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
    ///
    /// `amounts[i]` is the number of outcome-i tokens to redeem.
    /// Pass the held amount for the outcome you own and 0 for the other.
    pub async fn redeem_neg_risk(
        &self,
        condition_id: B256,
        amounts: Vec<U256>,
    ) -> anyhow::Result<TxResult> {
        tracing::info!(
            condition_id = %condition_id,
            safe = %self.safe_address,
            amounts = ?amounts,
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
    ///
    /// Uses ETH_SIGN signature mode (v > 30):
    /// 1. Compute safeTxHash via `getTransactionHash()`
    /// 2. Sign with EIP-191: `sign_message(safeTxHash)` → adds "\x19Ethereum Signed Message:\n32" prefix
    /// 3. Set v = ecdsa_v + 4 (27→31, 28→32) to indicate eth_sign mode
    async fn exec_through_safe(
        &self,
        to: Address,
        data: Bytes,
    ) -> anyhow::Result<TxResult> {
        let zero = U256::ZERO;
        let zero_addr = Address::ZERO;

        // 1. Get current nonce and compute the EIP-712 Safe transaction hash
        let nonce = self.safe.nonce().call().await?;
        let safe_tx_hash = self.safe.getTransactionHash(
            to,
            zero,
            data.clone(),
            0u8,
            zero,
            zero,
            zero,
            zero_addr,
            zero_addr,
            nonce,
        ).call().await?;

        tracing::debug!(
            to = %to,
            safe = %self.safe_address,
            signer = %self.signer.address(),
            nonce = %nonce,
            safe_tx_hash = %safe_tx_hash,
            "Signing Safe transaction (ETH_SIGN mode)"
        );

        // 2. Sign with EIP-191 prefix (eth_sign mode)
        // sign_message internally computes: keccak256("\x19Ethereum Signed Message:\n32" + hash)
        // then signs with ECDSA. The Safe's checkNSignatures does the same prefix
        // when v > 30, so ecrecover will recover our address.
        let sig = self.signer.sign_message(safe_tx_hash.as_slice()).await?;
        let sig_raw = sig.as_bytes();

        // 3. Adjust v for eth_sign mode: v + 4
        // alloy's as_bytes() returns v as 27 or 28 (27 + y_parity), so: v_final = v + 4 → 31 or 32
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..64].copy_from_slice(&sig_raw[..64]); // r + s
        sig_bytes[64] = sig_raw[64] + 4; // 27→31 or 28→32 for eth_sign mode

        // Send execTransaction through the Safe
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
            Bytes::from(sig_bytes.to_vec()),
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
