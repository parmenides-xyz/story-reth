use alloy_eips::eip7840::BlobParams;
use alloy_genesis::Genesis;
use alloy_primitives::{B256, U256};
use reth_chainspec::{
    BaseFeeParams, BaseFeeParamsKind, Chain, ChainSpec, DepositContract, EthChainSpec,
    EthereumHardfork, EthereumHardforks, ForkCondition, Hardfork, Hardforks,
};
use reth_cli::chainspec::{parse_genesis, ChainSpecParser};
use reth_evm::eth::spec::EthExecutorSpec;
use reth_network_peers::NodeRecord;
use std::sync::Arc;

/// Story chain specification wrapping Reth's ChainSpec.
#[derive(Debug, Clone, Default)]
pub struct StoryChainSpec {
    pub inner: ChainSpec,
}

impl EthChainSpec for StoryChainSpec {
    type Header = alloy_consensus::Header;

    fn chain(&self) -> Chain {
        self.inner.chain()
    }

    fn base_fee_params_at_timestamp(&self, timestamp: u64) -> BaseFeeParams {
        self.inner.base_fee_params_at_timestamp(timestamp)
    }

    fn blob_params_at_timestamp(&self, _timestamp: u64) -> Option<BlobParams> {
        // Story disables EIP-4844 blob transactions
        None
    }

    fn deposit_contract(&self) -> Option<&DepositContract> {
        None
    }

    fn genesis_hash(&self) -> B256 {
        self.inner.genesis_hash()
    }

    fn prune_delete_limit(&self) -> usize {
        self.inner.prune_delete_limit()
    }

    fn display_hardforks(&self) -> Box<dyn core::fmt::Display> {
        Box::new(self.inner.display_hardforks())
    }

    fn genesis_header(&self) -> &Self::Header {
        self.inner.genesis_header()
    }

    fn genesis(&self) -> &Genesis {
        self.inner.genesis()
    }

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        None
    }

    fn final_paris_total_difficulty(&self) -> Option<U256> {
        self.inner.final_paris_total_difficulty()
    }
}

impl EthereumHardforks for StoryChainSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        self.inner.ethereum_fork_activation(fork)
    }
}

impl Hardforks for StoryChainSpec {
    fn fork<H: Hardfork>(&self, fork: H) -> ForkCondition {
        self.inner.fork(fork)
    }

    fn forks_iter(&self) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)> {
        self.inner.forks_iter()
    }

    fn fork_id(&self, head: &reth_chainspec::Head) -> reth_chainspec::ForkId {
        self.inner.fork_id(head)
    }

    fn latest_fork_id(&self) -> reth_chainspec::ForkId {
        self.inner.latest_fork_id()
    }

    fn fork_filter(&self, head: reth_chainspec::Head) -> reth_chainspec::ForkFilter {
        self.inner.fork_filter(head)
    }
}

impl EthExecutorSpec for StoryChainSpec {
    fn deposit_contract_address(&self) -> Option<alloy_primitives::Address> {
        None
    }
}

impl From<ChainSpec> for StoryChainSpec {
    fn from(value: ChainSpec) -> Self {
        Self { inner: value }
    }
}

impl From<StoryChainSpec> for ChainSpec {
    fn from(value: StoryChainSpec) -> Self {
        value.inner
    }
}

/// Parser for Story chain specifications.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct StoryChainSpecParser;

impl ChainSpecParser for StoryChainSpecParser {
    type ChainSpec = StoryChainSpec;

    const SUPPORTED_CHAINS: &'static [&'static str] = &["story", "aeneid"];

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        Ok(Arc::new(parse_genesis(s)?.into()))
    }
}

impl From<Genesis> for StoryChainSpec {
    fn from(genesis: Genesis) -> Self {
        let mut inner = ChainSpec::from(genesis);

        if inner.chain.id() == 1514 {
            inner.base_fee_params = BaseFeeParamsKind::Constant(BaseFeeParams::new(24, 2));
        }

        Self { inner }
    }
}
