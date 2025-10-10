use std::str::FromStr;
use std::sync::Arc;
use agglayer_interop::types::Digest;
use agglayer_primitives::Address;
use jsonrpsee::{core::client::ClientT, http_client::HttpClient, rpc_params};
use url::Url;
use crate::contracts::{
    L1OpSuccinctConfigFetcher,
    L2LocalExitRootFetcher,
    L2OutputAtBlockFetcher,
    L2EvmStateSketchFetcher,
    GetTrustedSequencerAddress,
    L2OutputAtBlock,
    OpSuccinctConfig,
};
pub use crate::error::Error;
use alloy::{
    primitives::B256,
};
use sp1_cc_client_executor::io::EvmSketchInput;

// a decorator for the usual contracts client that handles certain calls differently where the network
// doesn't have an optimism namespace in the RPC and we need to handle the roots differently
#[derive(Clone)]
pub struct ErigonContractsClient<T> {
    inner: Arc<T>,
    l2_client: Arc<HttpClient>,
}

impl<T> ErigonContractsClient<T>
where T: Send + Sync + 'static {
    pub fn new(inner: Arc<T>, l2endpoint: Url) -> Result<Self, Error> {
        let l2_client = Arc::new(
            HttpClient::builder()
                .build(l2endpoint)
                .map_err(Error::RollupNodeInitError)?,
        );

        Ok(Self {
            inner,
            l2_client,
        })
    }
}

#[async_trait::async_trait]
impl<T> GetTrustedSequencerAddress for ErigonContractsClient<T>
where
    T: GetTrustedSequencerAddress + Send + Sync,
{
    async fn get_trusted_sequencer_address(&self) -> Result<Address, Error> {
        self.inner.clone().get_trusted_sequencer_address().await
    }
}


#[async_trait::async_trait]
impl<T> L1OpSuccinctConfigFetcher for ErigonContractsClient<T>
where
    T: L1OpSuccinctConfigFetcher + Send + Sync,
{
    async fn get_op_succinct_config(&self) -> Result<OpSuccinctConfig, Error> {
        self.inner.get_op_succinct_config().await
    }
}

#[async_trait::async_trait]
impl<T> L2EvmStateSketchFetcher for ErigonContractsClient<T>
where
    T: L2EvmStateSketchFetcher + Send + Sync,
{
    async fn get_prev_l2_block_sketch(&self, block_number: alloy::eips::BlockNumberOrTag) -> Result<EvmSketchInput, Error> {
        self.inner.clone().get_prev_l2_block_sketch(block_number).await
    }

    async fn get_new_l2_block_sketch(&self, block_number: alloy::eips::BlockNumberOrTag) -> Result<EvmSketchInput, Error> {
        self.inner.clone().get_new_l2_block_sketch(block_number).await
    }
}

#[async_trait::async_trait]
impl<T> L2OutputAtBlockFetcher for ErigonContractsClient<T>
where
    T: Send + Sync,
{
    async fn get_l2_output_at_block(&self, block_number: u64) -> Result<L2OutputAtBlock, Error> {
        fn parse_hash(json: &serde_json::Value, field: &str) -> Result<Digest, Error> {
            let value_str = json
                .get(field)
                .ok_or(Error::L2OutputAtBlockValueMissing(field.to_string()))?
                .as_str()
                .ok_or(Error::L2OutputAtBlockValueMissing(field.to_string()))?;

            B256::from_str(value_str)
                .map(|bytes| bytes.0.into())
                .map_err(|e| Error::L2OutputAtBlockInvalidValue(field.to_string(), e))
        }

        let params = rpc_params![format!("0x{block_number:x}")];

        let json: serde_json::Value = self
            .l2_client
            .request("eth_getBlockByNumber", params)
            .await
            .map_err(Error::L2OutputAtBlockRetrievalError)?;

        Ok(L2OutputAtBlock {
            version: Digest::default(), // 0x0 on katana network
            state_root: parse_hash(&json, "stateRoot")?,
            withdrawal_storage_root: parse_hash(&json, "withdrawalsRoot")?,
            latest_block_hash: parse_hash(&json, "hash")?,
            output_root: parse_hash(&json, "stateRoot")?,
        })
    }
}

#[async_trait::async_trait]
impl<T> L2LocalExitRootFetcher for ErigonContractsClient<T>
where
    T: L2LocalExitRootFetcher + Send + Sync,
{
    async fn get_l2_local_exit_root(&self, block_number: u64) -> Result<Digest, Error> {
        self.inner.get_l2_local_exit_root(block_number).await
    }
}

// Implement the AggchainContractsClient marker trait
impl<T> crate::AggchainContractsClient for ErigonContractsClient<T>
where
    T: L2LocalExitRootFetcher + L2OutputAtBlockFetcher + L1OpSuccinctConfigFetcher + L2EvmStateSketchFetcher + Send + Sync,
{}
