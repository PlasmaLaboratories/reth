//! Per-transaction replay using a [`TracingInspector`] to recover internal
//! native value transfers that are invisible in receipts.

use crate::{
    extractor::{extract_value_transfers, push_fee_row, LogIndexCounter, TxRowContext},
    payload::Transfer,
};
use alloy_consensus::{transaction::TxHashRef, BlockHeader, Transaction};
use alloy_evm::{block::BlockExecutor, evm::EvmFactoryExt};
use alloy_primitives::Address;
use eyre::Context;
use reth_ethereum::{
    evm::primitives::ConfigureEvm,
    node::api::{FullNodeComponents, FullNodeTypes, NodePrimitives, NodeTypes},
    primitives::RecoveredBlock,
};
use reth_revm::{database::StateProviderDatabase, db::State, revm::context::Block as RevmBlock};
use reth_storage_api::StateProviderFactory;
use revm_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

type BlockTy<Node> =
    <<<Node as FullNodeTypes>::Types as NodeTypes>::Primitives as NodePrimitives>::Block;

/// Replay every transaction in `block` against its parent state with a parity
/// tracing inspector. Returns the flat list of native-transfer + fee rows in
/// block order.
pub fn replay_block<Node>(
    ctx: &reth_exex::ExExContext<Node>,
    block: &RecoveredBlock<BlockTy<Node>>,
    native_address: Address,
) -> eyre::Result<Vec<Transfer>>
where
    Node: FullNodeComponents,
{
    let parent_state = ctx
        .provider()
        .history_by_block_hash(block.parent_hash())
        .wrap_err("provider missing parent state for committed block")?;

    let mut db = State::builder()
        .with_bundle_update()
        .with_database(StateProviderDatabase::new(parent_state))
        .build();

    let evm_env = ctx
        .evm_config()
        .evm_env(block.sealed_block().sealed_header())
        .map_err(|e| eyre::eyre!("evm_env failed: {e:?}"))?;
    let base_fee = RevmBlock::basefee(&evm_env.block_env);

    // Apply pre-block state transitions (beacon root contract, withdrawals queue, etc.).
    ctx.evm_config()
        .executor_for_block(&mut db, block.sealed_block())
        .map_err(|e| eyre::eyre!("executor_for_block failed: {e:?}"))?
        .apply_pre_execution_changes()
        .map_err(|e| eyre::eyre!("apply_pre_execution_changes failed: {e:?}"))?;

    let block_hash = block.hash();
    let block_number = block.number();
    let mut rows = Vec::new();
    let mut counter = LogIndexCounter::default();

    let mut tracer = ctx.evm_config().evm_factory().create_tracer(
        &mut db,
        evm_env,
        TracingInspector::new(TracingInspectorConfig::default_parity()),
    );

    for result in
        tracer.try_trace_many(block.transactions_recovered(), |mut tctx| -> eyre::Result<()> {
            let tx = tctx.tx;
            let tx_hash = *tx.tx_hash();
            let signer = tx.signer();
            let gas_used = tctx.result.gas_used();
            let effective_gas_price = tx.effective_gas_price(Some(base_fee));

            let row_ctx = TxRowContext {
                native_address,
                block_hash,
                block_number,
                transaction_hash: tx_hash,
                tx_signer: signer,
            };

            let inspector = tctx.take_inspector();
            let arena = inspector.into_traces();
            extract_value_transfers(&arena, &row_ctx, &mut counter, &mut rows);
            push_fee_row(&row_ctx, gas_used, effective_gas_price, &mut counter, &mut rows);
            Ok(())
        })
    {
        result?;
    }

    Ok(rows)
}
