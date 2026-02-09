use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;

use polymarket_client_sdk::ctf::types::{
    MergePositionsRequest, RedeemPositionsRequest, SplitPositionRequest,
};

/// USDC contract address on Polygon mainnet.
pub const USDC_ADDRESS: Address = alloy::primitives::address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

/// On-chain CTF executor for split/merge/redeem operations.
///
/// Uses `polymarket_client_sdk::ctf::Client` for contract interactions
/// on the Gnosis Conditional Token Framework.
pub struct CtfExecutor<P: Provider + Clone> {
    client: polymarket_client_sdk::ctf::Client<P>,
    collateral: Address,
}

impl<P: Provider + Clone> CtfExecutor<P> {
    /// Create a new CtfExecutor for standard (non-NegRisk) markets.
    ///
    /// # Arguments
    /// * `provider` - An alloy provider connected to Polygon
    /// * `chain_id` - Chain ID (137 for mainnet, 80002 for Amoy testnet)
    pub fn new(provider: P, chain_id: u64) -> anyhow::Result<Self> {
        let client = polymarket_client_sdk::ctf::Client::new(provider, chain_id)?;
        Ok(Self {
            client,
            collateral: USDC_ADDRESS,
        })
    }

    /// Create a new CtfExecutor with NegRisk adapter support.
    pub fn with_neg_risk(provider: P, chain_id: u64) -> anyhow::Result<Self> {
        let client = polymarket_client_sdk::ctf::Client::with_neg_risk(provider, chain_id)?;
        Ok(Self {
            client,
            collateral: USDC_ADDRESS,
        })
    }

    /// Override the collateral token address (default: USDC on Polygon).
    pub fn with_collateral(mut self, collateral: Address) -> Self {
        self.collateral = collateral;
        self
    }

    /// Merge YES + NO tokens back into USDC collateral.
    ///
    /// Calls `ConditionalTokens.mergePositions()` on-chain.
    /// Requires prior ERC-1155 `setApprovalForAll()` to CTFExchange.
    ///
    /// # Arguments
    /// * `condition_id` - The market's condition ID
    /// * `amount` - Amount to merge (in USDC decimals, i.e. 1_000_000 = 1 USDC)
    pub async fn merge(
        &self,
        condition_id: B256,
        amount: U256,
    ) -> anyhow::Result<TxResult> {
        tracing::info!(
            condition_id = %condition_id,
            amount = %amount,
            "Executing on-chain merge"
        );

        let request = MergePositionsRequest::for_binary_market(
            self.collateral,
            condition_id,
            amount,
        );

        let response = self.client.merge_positions(&request).await?;

        tracing::info!(
            tx_hash = %response.transaction_hash,
            block = response.block_number,
            "Merge transaction confirmed"
        );

        Ok(TxResult {
            tx_hash: response.transaction_hash,
            block_number: response.block_number,
        })
    }

    /// Split USDC collateral into YES + NO tokens.
    ///
    /// Calls `ConditionalTokens.splitPosition()` on-chain.
    /// Requires prior USDC `approve()` to ConditionalTokens contract.
    ///
    /// # Arguments
    /// * `condition_id` - The market's condition ID
    /// * `amount` - Amount of USDC to split (in USDC decimals)
    pub async fn split(
        &self,
        condition_id: B256,
        amount: U256,
    ) -> anyhow::Result<TxResult> {
        tracing::info!(
            condition_id = %condition_id,
            amount = %amount,
            "Executing on-chain split"
        );

        let request = SplitPositionRequest::for_binary_market(
            self.collateral,
            condition_id,
            amount,
        );

        let response = self.client.split_position(&request).await?;

        tracing::info!(
            tx_hash = %response.transaction_hash,
            block = response.block_number,
            "Split transaction confirmed"
        );

        Ok(TxResult {
            tx_hash: response.transaction_hash,
            block_number: response.block_number,
        })
    }

    /// Redeem winning tokens after market resolution.
    ///
    /// Calls `ConditionalTokens.redeemPositions()` on-chain.
    ///
    /// # Arguments
    /// * `condition_id` - The resolved market's condition ID
    pub async fn redeem(
        &self,
        condition_id: B256,
    ) -> anyhow::Result<TxResult> {
        tracing::info!(condition_id = %condition_id, "Executing on-chain redeem");

        let request = RedeemPositionsRequest::for_binary_market(
            self.collateral,
            condition_id,
        );

        let response = self.client.redeem_positions(&request).await?;

        tracing::info!(
            tx_hash = %response.transaction_hash,
            block = response.block_number,
            "Redeem transaction confirmed"
        );

        Ok(TxResult {
            tx_hash: response.transaction_hash,
            block_number: response.block_number,
        })
    }

    /// Redeem from a NegRisk market using the NegRiskAdapter.
    ///
    /// # Arguments
    /// * `condition_id` - The NegRisk market's condition ID
    /// * `amounts` - Amounts per outcome to redeem [yes_amount, no_amount]
    pub async fn redeem_neg_risk(
        &self,
        condition_id: B256,
        amounts: Vec<U256>,
    ) -> anyhow::Result<TxResult> {
        tracing::info!(
            condition_id = %condition_id,
            amounts_len = amounts.len(),
            "Executing NegRisk redeem"
        );

        let request = polymarket_client_sdk::ctf::types::RedeemNegRiskRequest::builder()
            .condition_id(condition_id)
            .amounts(amounts)
            .build();

        let response = self.client.redeem_neg_risk(&request).await?;

        tracing::info!(
            tx_hash = %response.transaction_hash,
            block = response.block_number,
            "NegRisk redeem transaction confirmed"
        );

        Ok(TxResult {
            tx_hash: response.transaction_hash,
            block_number: response.block_number,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TxResult {
    pub tx_hash: B256,
    pub block_number: u64,
}
