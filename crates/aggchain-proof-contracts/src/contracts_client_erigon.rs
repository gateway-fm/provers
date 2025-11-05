use std::panic::AssertUnwindSafe;
use std::str::FromStr;
use std::sync::Arc;
use agglayer_interop::types::Digest;
use agglayer_primitives::Address;
use jsonrpsee::{core::client::ClientT, http_client::HttpClient, rpc_params};
use url::Url;
use crate::contracts::{L1OpSuccinctConfigFetcher, L2LocalExitRootFetcher, L2OutputAtBlockFetcher, L2EvmStateSketchFetcher, GetTrustedSequencerAddress, L2OutputAtBlock, OpSuccinctConfig, GlobalExitRootManagerL2SovereignChain, GlobalExitRootManagerL2SovereignChainRpcClient, ZkevmBridgeRpcClient, PolygonZkevmBridgeV2};
pub use crate::error::Error;
use alloy::{
    primitives::B256,
};
use eyre::Context as _;
use sp1_cc_client_executor::io::{EvmSketchInput};
use sp1_cc_client_executor::{Genesis};
use sp1_cc_host_executor::EvmSketch;
use aggchain_proof_core::bridge::BridgeL2SovereignChain;
use aggchain_proof_core::bridge::static_call::{HashChainType, StaticCallStage};
use prover_alloy::{build_alloy_fill_provider, AlloyFillProvider};
use prover_executor::sp1_async;
use crate::config::AggchainProofContractsConfig;
use crate::contracts::GlobalExitRootManagerL2SovereignChain::GlobalExitRootManagerL2SovereignChainCalls;
use crate::{config, host_execute};

// a decorator for the usual contracts client that handles certain calls differently where the network
// doesn't have an optimism namespace in the RPC and we need to handle the roots differently
#[derive(Clone)]
pub struct ErigonContractsClient<T> {
    inner: Arc<T>,
    l2_client: Arc<HttpClient>,
    static_call_caller_address: Address,
    global_exit_root_manager_l2: GlobalExitRootManagerL2SovereignChainRpcClient<AlloyFillProvider>,
    polygon_zkevm_bridge_v2: ZkevmBridgeRpcClient<AlloyFillProvider>,
    sketch_genesis: Genesis,
}

impl<T> ErigonContractsClient<T>
where T: Send + Sync + 'static {
    pub async fn new(inner: Arc<T>, l2endpoint: Url, config: AggchainProofContractsConfig) -> Result<Self, Error> {
        let l2_client = Arc::new(
            HttpClient::builder()
                .build(l2endpoint)
                .map_err(Error::RollupNodeInitError)?,
        );

        let l2_el_client = build_alloy_fill_provider(
            &config.l2_execution_layer_rpc_endpoint,
            prover_alloy::DEFAULT_HTTP_RPC_NODE_INITIAL_BACKOFF_MS,
            prover_alloy::DEFAULT_HTTP_RPC_NODE_BACKOFF_MAX_RETRIES,
        )
        .map_err(Error::ProviderInitializationError)?;

        let global_exit_root_manager_l2 = GlobalExitRootManagerL2SovereignChain::new(
            config.global_exit_root_manager_v2_sovereign_chain.into(),
            l2_el_client.clone(),
        );

        let polygon_zkevm_bridge_address = global_exit_root_manager_l2
            .bridgeAddress()
            .call()
            .await
            .map_err(Error::BridgeAddressError)?;

        // Create client for Polygon zkevm bridge v2 smart contract.
        let polygon_zkevm_bridge_v2 =
            PolygonZkevmBridgeV2::new(polygon_zkevm_bridge_address, l2_el_client.clone());

        Ok(Self {
            inner,
            l2_client,
            static_call_caller_address: config.static_call_caller_address,
            global_exit_root_manager_l2,
            polygon_zkevm_bridge_v2,
            sketch_genesis: config::parse_evm_sketch_genesis(&config.evm_sketch_genesis)?,
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
        sp1_async(AssertUnwindSafe(async move {
            let sketch = EvmSketch::builder()
                .at_block(block_number)
                .with_genesis(self.sketch_genesis.clone())
                .el_rpc_url(Url::parse("http://127.0.0.1:8123").unwrap())
                .build()
                .await
                .map_err(Error::HostExecutorNewBlockInitialization)?;

            let caller_address = *self.static_call_caller_address.as_alloy();
            let ger_address = *self.global_exit_root_manager_l2.address();
            let bridge_address = *self.polygon_zkevm_bridge_v2.address();

            {
                host_execute(
                    caller_address,
                    ger_address,
                    &sketch,
                    GlobalExitRootManagerL2SovereignChain::insertedGERHashChainCall {},
                    StaticCallStage::PrevHashChain(HashChainType::InsertedGER),
                )
                .await?;

                host_execute(
                    caller_address,
                    ger_address,
                    &sketch,
                    GlobalExitRootManagerL2SovereignChain::removedGERHashChainCall {},
                    StaticCallStage::NewHashChain(HashChainType::RemovedGER),
                )
                .await?;

                host_execute(
                    caller_address,
                    bridge_address,
                    &sketch,
                    BridgeL2SovereignChain::claimedGlobalIndexHashChainCall {},
                    StaticCallStage::NewHashChain(HashChainType::ClaimedGlobalIndex),
                )
                .await?;

                host_execute(
                    caller_address,
                    bridge_address,
                    &sketch,
                    BridgeL2SovereignChain::unsetGlobalIndexHashChainCall {},
                    StaticCallStage::NewHashChain(HashChainType::UnsetGlobalIndex),
                )
                .await?
            }

            let prev_block_sketch = sketch
                .finalize()
                .await
                .map_err(Error::InvalidPreBlockSketchFinalization)?;

            Ok(prev_block_sketch)
        }))
        .await
        .context("Failed getting previous L2 block sketch")
        .map_err(Error::Other)?
    }

    async fn get_new_l2_block_sketch(&self, block_number: alloy::eips::BlockNumberOrTag) -> Result<EvmSketchInput, Error> {
        // Create an empty EvmSketchInput with default values
        let sketch = EvmSketch::builder()
            // .optimism()
            .at_block(block_number)
            .with_genesis(Genesis::Mainnet)
            .el_rpc_url(Url::parse(Url::parse("http://localhost:8123").unwrap().as_str()).unwrap())
            .build()
            .await
            .map_err(Error::HostExecutorNewBlockInitialization)?;

        let result = sketch.finalize()
            .await
            .map_err(Error::InvalidPreBlockSketchFinalization)?;

        Ok(result)
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
