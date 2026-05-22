use crate::{metrics as m, outbox::Outbox, replay::replay_block};
use alloy_consensus::BlockHeader;
use alloy_primitives::Address;
use futures::TryStreamExt;
use reth_ethereum::{
    exex::{ExExContext, ExExEvent, ExExNotification},
    node::api::FullNodeComponents,
};
use tracing::{debug, error, info, warn};

/// The ExEx future. Loops over canonical chain notifications, replays each
/// committed block, persists rows to the outbox, and signals
/// [`ExExEvent::FinishedHeight`] only after the rows are durable.
pub async fn xpl_webhook_exex<Node: FullNodeComponents>(
    mut ctx: ExExContext<Node>,
    outbox: Outbox,
    native_address: Address,
) -> eyre::Result<()> {
    info!(target: "xpl-webhook", "ExEx started");

    while let Some(notification) = ctx.notifications.try_next().await? {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                for block in new.blocks_iter() {
                    let block_number = block.number();
                    match replay_block(&ctx, block, native_address) {
                        Ok(rows) => {
                            metrics::counter!(m::BLOCKS_PROCESSED).increment(1);
                            if rows.is_empty() {
                                continue;
                            }
                            metrics::counter!(m::ROWS_EMITTED).increment(rows.len() as u64);
                            debug!(
                                target: "xpl-webhook",
                                block_number,
                                rows = rows.len(),
                                "persisting batch",
                            );
                            if let Err(err) =
                                outbox.append_block_batch(block.hash(), block_number, rows)
                            {
                                error!(target: "xpl-webhook", %err, "outbox write failed");
                            }
                        }
                        Err(err) => {
                            metrics::counter!(m::REPLAY_ERRORS).increment(1);
                            error!(target: "xpl-webhook", block_number, %err, "replay failed");
                        }
                    }
                }
            }
            ExExNotification::ChainReorged { old, new } => {
                metrics::counter!(m::REORGS).increment(1);
                warn!(
                    target: "xpl-webhook",
                    old = ?old.range(),
                    new = ?new.range(),
                    "reorg: discarding pending rows for old chain segment",
                );
                if let Err(err) = outbox.discard_blocks(old.blocks_iter().map(|b| b.hash())) {
                    error!(target: "xpl-webhook", %err, "failed to discard reorged blocks");
                }
                for block in new.blocks_iter() {
                    let block_number = block.number();
                    if let Ok(rows) = replay_block(&ctx, block, native_address) &&
                        !rows.is_empty()
                    {
                        let _ = outbox.append_block_batch(block.hash(), block_number, rows);
                    }
                }
            }
            ExExNotification::ChainReverted { old } => {
                metrics::counter!(m::REVERTS).increment(1);
                warn!(
                    target: "xpl-webhook",
                    old = ?old.range(),
                    "revert: discarding pending rows",
                );
                if let Err(err) = outbox.discard_blocks(old.blocks_iter().map(|b| b.hash())) {
                    error!(target: "xpl-webhook", %err, "failed to discard reverted blocks");
                }
            }
        }

        if let Some(committed) = notification.committed_chain() {
            ctx.events.send(ExExEvent::FinishedHeight(committed.tip().num_hash()))?;
        }
    }

    info!(target: "xpl-webhook", "ExEx exiting");
    Ok(())
}
