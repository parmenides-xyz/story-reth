//! Story EVM factory with IPGraph stateful precompile.

mod ipgraph;

use alloy_evm::{
    eth::EthEvmContext,
    precompiles::PrecompilesMap,
    revm::handler::EthPrecompiles,
    EvmFactory,
};
use crate::chainspec::StoryChainSpec;
use reth_ethereum::{
    evm::{
        primitives::{Database, EvmEnv},
        revm::{
            context::{BlockEnv, Context, TxEnv},
            context_interface::result::{EVMError, HaltReason},
            inspector::{Inspector, NoOpInspector},
            interpreter::interpreter::EthInterpreter,
            primitives::hardfork::SpecId,
            MainBuilder, MainContext,
        },
    },
    node::{
        api::{FullNodeTypes, NodeTypes},
        builder::{components::ExecutorBuilder, BuilderContext},
        evm::EthEvm,
        EthEvmConfig,
    },
    EthPrimitives,
};

/// Story EVM factory
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct StoryEvmFactory;

impl EvmFactory for StoryEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>, EthInterpreter>> =
        EthEvm<DB, I, PrecompilesMap>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let mut precompiles =
            PrecompilesMap::from_static(EthPrecompiles::default().precompiles);
        precompiles.extend_precompiles([
            (ipgraph::IPGRAPH_ADDRESS, ipgraph::ipgraph_precompile()),
        ]);

        let evm = Context::mainnet()
            .with_db(db)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env)
            .build_mainnet_with_inspector(NoOpInspector {})
            .with_precompiles(precompiles);

        EthEvm::new(evm, false)
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        EthEvm::new(self.create_evm(db, input).into_inner().with_inspector(inspector), true)
    }
}

/// Builds an Ethereum block executor that uses the Story EVM factory.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct StoryExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for StoryExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = StoryChainSpec, Primitives = EthPrimitives>>,
{
    type EVM = EthEvmConfig<StoryChainSpec, StoryEvmFactory>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        Ok(EthEvmConfig::new_with_evm_factory(ctx.chain_spec(), StoryEvmFactory))
    }
}