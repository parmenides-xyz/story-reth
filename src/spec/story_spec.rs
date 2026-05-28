use std::{fmt::Display, sync::Arc, time::SystemTime};

use alloy_eips::eip7840::BlobParams;
use alloy_genesis::Genesis;
use alloy_primitives::{Address, B256, U256};
use derive_more::{Deref, Into};
use reth_chainspec::{
    BaseFeeParams, BaseFeeParamsKind, ChainSpec, DepositContract, EthChainSpec, EthereumHardfork,
    EthereumHardforks, ForkCondition, ForkFilter, ForkHash, ForkId, Hardfork, Hardforks, Head,
};
use reth_cli::chainspec::{parse_genesis, ChainSpecParser};
use reth_evm::eth::spec::EthExecutorSpec;
use reth_network_peers::{parse_nodes, NodeRecord};
use tracing::debug;

#[derive(Debug, PartialEq, Eq)]
enum Chain {
    Story,
    Aeneid,
}

impl Chain {
    fn from_chain_id(chain_id: u64) -> Option<Self> {
        match chain_id {
            1514 => Some(Chain::Story),
            1315 => Some(Chain::Aeneid),
            _ => None,
        }
    }
}

use super::chains::{AENEID_GENESIS, STORY_GENESIS};

const STORY_NODES: &[&str] = &[
    "enode://f42110982b6ddaa4de8031f9fecb619d181902db5529a43bc9b1187debbc67771bf937b2210cbfd33babd2acbe138506596e23d0d1792ab3cb5229c5bb051544@b1.storyrpc.io:30303",
    "enode://2ae459a7cc28b59822377deec266e24e5ed00374d7a83e2e8d0d67dd89dc2b80366c1353c7909fe81b840f6081188850677fa20dd5d262c9e3f67eb23d0be0b5@b2.storyrpc.io:30303",
];

const AENEID_NODES: &[&str] = &[
    "enode://a7e893eb4b07bd9b0c0659730c066564dff0f5fa98c08a7df9f380b84e64fbea16165ee5cce6c3414d64bea8cacc1ac200540c50607a7bf170b9d5504f81bbf8@b1-b.odyssey-devnet.storyrpc.io:30303",
];

/// Story chain specification.
#[derive(Debug, Clone, Default, Deref, Into, PartialEq, Eq)]
pub struct StoryChainSpec {
    #[deref]
    pub inner: ChainSpec,
}

impl EthChainSpec for StoryChainSpec {
    type Header = alloy_consensus::Header;

    fn chain(&self) -> reth_chainspec::Chain {
        self.inner.chain()
    }

    fn base_fee_params_at_timestamp(&self, timestamp: u64) -> BaseFeeParams {
        self.inner.base_fee_params_at_timestamp(timestamp)
    }

    fn blob_params_at_timestamp(&self, timestamp: u64) -> Option<BlobParams> {
        match Chain::from_chain_id(self.chain_id()) {
            Some(Chain::Story) | Some(Chain::Aeneid) => None,
            None => self.inner.blob_params_at_timestamp(timestamp),
        }
    }

    fn deposit_contract(&self) -> Option<&DepositContract> {
        self.inner.deposit_contract()
    }

    fn genesis_hash(&self) -> B256 {
        self.inner.genesis_hash()
    }

    fn prune_delete_limit(&self) -> usize {
        self.inner.prune_delete_limit()
    }

    fn display_hardforks(&self) -> Box<dyn Display> {
        Box::new(ChainSpec::display_hardforks(self))
    }

    fn genesis_header(&self) -> &Self::Header {
        self.inner.genesis_header()
    }

    fn genesis(&self) -> &Genesis {
        self.inner.genesis()
    }

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        if let Some(chain) = Chain::from_chain_id(self.chain_id()) {
            match chain {
                Chain::Story => Some(parse_nodes(STORY_NODES)),
                Chain::Aeneid => Some(parse_nodes(AENEID_NODES)),
            }
        } else {
            None
        }
    }

    fn final_paris_total_difficulty(&self) -> Option<U256> {
        self.inner.final_paris_total_difficulty()
    }
}

impl Hardforks for StoryChainSpec {
    fn fork<H: Hardfork>(&self, fork: H) -> ForkCondition {
        self.inner.fork(fork)
    }

    fn forks_iter(
        &self,
    ) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)> {
        self.inner.forks_iter()
    }

    fn fork_id(&self, head: &Head) -> ForkId {
        self.inner.fork_id(head)
    }

    fn latest_fork_id(&self) -> ForkId {
        self.inner.latest_fork_id()
    }

    fn fork_filter(&self, head: Head) -> ForkFilter {
        self.inner.fork_filter(head)
    }
}

impl EthereumHardforks for StoryChainSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        self.inner.ethereum_fork_activation(fork)
    }
}

impl EthExecutorSpec for StoryChainSpec {
    fn deposit_contract_address(&self) -> Option<Address> {
        self.inner.deposit_contract
            .map(|deposit_contract| deposit_contract.address)
    }
}

impl From<Genesis> for StoryChainSpec {
    fn from(genesis: Genesis) -> Self {
        let mut inner = ChainSpec::from(genesis);

        if Chain::from_chain_id(inner.chain_id()) == Some(Chain::Story) {
            inner.base_fee_params = BaseFeeParamsKind::Constant(BaseFeeParams::new(24, 2));
        }

        Self { inner }
    }
}

/// Parser for Story chain specifications.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct StoryChainSpecParser;

impl ChainSpecParser for StoryChainSpecParser {
    type ChainSpec = StoryChainSpec;

    const SUPPORTED_CHAINS: &'static [&'static str] = &["aeneid", "story"];

    fn default_value() -> Option<&'static str> {
        Some("story")
    }

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        chain_value_parser(s)
    }
}

/// Parses a built-in chain name, genesis JSON file path, or in-memory genesis JSON string.
pub fn chain_value_parser(s: &str) -> eyre::Result<Arc<StoryChainSpec>, eyre::Error> {
    Ok(match s {
        "aeneid" => Arc::new(StoryChainSpec::from(AENEID_GENESIS.clone())),
        "story" => Arc::new(StoryChainSpec::from(STORY_GENESIS.clone())),
        _ => Arc::new(parse_genesis(s)?.into()),
    })
}

impl StoryChainSpec {
    pub fn log_all_fork_ids(&self) {
        debug!(target: "reth::story", "=== Fork IDs for all hardforks ===");

        let genesis_hash = self.genesis_hash();
        let mut forkhash = ForkHash::from(genesis_hash);
        let mut current_applied = 0;

        debug!(target: "reth::story", %genesis_hash, ?forkhash, "Genesis info");

        debug!(target: "reth::story", "Block-based forks:");
        for (hardfork, cond) in self.hardforks.forks_iter() {
            match cond {
                ForkCondition::Block(block)
                | ForkCondition::TTD {
                    fork_block: Some(block),
                    ..
                } => {
                    if block != current_applied {
                        forkhash += block;
                        current_applied = block;
                    }
                    debug!(
                        target: "reth::story",
                        hardfork = %hardfork.name(),
                        block,
                        ?forkhash,
                        "Block fork"
                    );
                }
                _ => {}
            }
        }

        debug!(target: "reth::story", "Timestamp-based forks:");
        for (hardfork, cond) in self.hardforks.forks_iter() {
            if let ForkCondition::Timestamp(timestamp) = cond {
                if timestamp > self.genesis.timestamp && timestamp != current_applied {
                    forkhash += timestamp;
                    current_applied = timestamp;
                }
                debug!(
                    target: "reth::story",
                    hardfork = %hardfork.name(),
                    timestamp,
                    ?forkhash,
                    "Timestamp fork"
                );
            }
        }

        let current_timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        debug!(
            target: "reth::story",
            ?forkhash,
            current_timestamp,
            "Final fork hash"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_supports_story_and_aeneid() {
        assert_eq!(StoryChainSpecParser::SUPPORTED_CHAINS, ["aeneid", "story"]);
        assert_eq!(StoryChainSpecParser::default_value(), Some("story"));
    }

    #[test]
    fn parser_builtin_networks_parse() {
        let aeneid = StoryChainSpecParser::parse("aeneid").unwrap();
        let story = StoryChainSpecParser::parse("story").unwrap();

        assert_eq!(aeneid.chain_id(), 1315);
        assert_eq!(story.chain_id(), 1514);
    }

    #[test]
    fn story_base_fee_params_are_customized() {
        let story = StoryChainSpecParser::parse("story").unwrap();
        let params = story.base_fee_params_at_timestamp(0);

        assert_eq!(params.max_change_denominator, 24);
        assert_eq!(params.elasticity_multiplier, 2);
    }

    #[test]
    fn aeneid_base_fee_params_remain_ethereum_default() {
        let aeneid = StoryChainSpecParser::parse("aeneid").unwrap();
        let params = aeneid.base_fee_params_at_timestamp(0);

        assert_eq!(params.max_change_denominator, 8);
        assert_eq!(params.elasticity_multiplier, 2);
    }

    #[test]
    fn builtin_story_networks_disable_blob_params() {
        let aeneid = StoryChainSpecParser::parse("aeneid").unwrap();
        let story = StoryChainSpecParser::parse("story").unwrap();

        assert_eq!(aeneid.blob_params_at_timestamp(0), None);
        assert_eq!(story.blob_params_at_timestamp(0), None);
    }
}
