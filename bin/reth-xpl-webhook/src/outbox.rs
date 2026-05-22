use crate::payload::Transfer;
use alloy_primitives::B256;
use reth_libmdbx::{DatabaseFlags, Environment, EnvironmentKind, Geometry, PageSize, WriteFlags};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
use tokio::sync::Notify;

const PENDING_TABLE: &str = "pending_batches";
const CHECKPOINT_TABLE: &str = "delivery_checkpoint";
const CHECKPOINT_KEY: &[u8] = b"checkpoint";

/// Default MDBX geometry: grow up to 16 GiB, start at 64 MiB.
const DB_INITIAL_BYTES: usize = 64 * 1024 * 1024;
const DB_MAX_BYTES: usize = 16 * 1024 * 1024 * 1024;
const DB_GROWTH_STEP: isize = 64 * 1024 * 1024;

/// One persisted batch — emitted rows for a single block plus delivery
/// bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingBatchRecord {
    pub block_number: u64,
    pub block_hash: B256,
    pub rows: Vec<Transfer>,
    pub delivered: bool,
    pub attempts: u32,
    /// Earliest unix-millis at which the dispatcher should retry this batch.
    pub next_attempt_at: u64,
}

/// Durable webhook outbox backed by a dedicated MDBX environment under the
/// node's datadir.
#[derive(Clone)]
pub struct Outbox {
    inner: Arc<OutboxInner>,
}

struct OutboxInner {
    env: Environment,
    pending_dbi: reth_libmdbx::ffi::MDBX_dbi,
    checkpoint_dbi: reth_libmdbx::ffi::MDBX_dbi,
    wakeup: Arc<Notify>,
}

impl Outbox {
    /// Open or create the outbox env at `path`.
    pub fn open(path: &Path) -> eyre::Result<Self> {
        std::fs::create_dir_all(path)?;

        let env = Environment::builder()
            .set_kind(EnvironmentKind::Default)
            .set_max_dbs(8)
            .set_geometry(Geometry {
                size: Some(DB_INITIAL_BYTES..DB_MAX_BYTES),
                growth_step: Some(DB_GROWTH_STEP),
                shrink_threshold: None,
                page_size: Some(PageSize::MinimalAcceptable),
            })
            .open(path)?;

        let (pending_dbi, checkpoint_dbi) = {
            let tx = env.begin_rw_txn()?;
            let pending = tx.create_db(Some(PENDING_TABLE), DatabaseFlags::default())?;
            let checkpoint = tx.create_db(Some(CHECKPOINT_TABLE), DatabaseFlags::default())?;
            let p = pending.dbi();
            let c = checkpoint.dbi();
            tx.commit()?;
            (p, c)
        };

        Ok(Self {
            inner: Arc::new(OutboxInner {
                env,
                pending_dbi,
                checkpoint_dbi,
                wakeup: Arc::new(Notify::new()),
            }),
        })
    }

    /// Returns a handle the dispatcher can `notified()` on to learn that new
    /// rows are pending.
    pub fn wakeup_handle(&self) -> Arc<Notify> {
        self.inner.wakeup.clone()
    }

    /// Persist a non-empty batch of rows for `block_hash` and wake any waiter.
    pub fn append_block_batch(
        &self,
        block_hash: B256,
        block_number: u64,
        rows: Vec<Transfer>,
    ) -> eyre::Result<()> {
        let record = PendingBatchRecord {
            block_number,
            block_hash,
            rows,
            delivered: false,
            attempts: 0,
            next_attempt_at: 0,
        };
        self.put_record(&record)?;
        self.inner.wakeup.notify_waiters();
        Ok(())
    }

    /// Remove pending (not-yet-delivered) batches for the given block hashes.
    /// Used to drop reverted blocks on reorg.
    pub fn discard_blocks(&self, hashes: impl IntoIterator<Item = B256>) -> eyre::Result<()> {
        let tx = self.inner.env.begin_rw_txn()?;
        for hash in hashes {
            let cursor = tx.cursor_with_dbi(self.inner.pending_dbi)?;
            if let Some((key, value)) = find_by_hash(cursor, hash)? {
                let record: PendingBatchRecord = serde_json::from_slice(&value)?;
                if !record.delivered {
                    tx.del(self.inner.pending_dbi, &key, None)?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Return the oldest pending batch whose `next_attempt_at <= now_ms`, or
    /// `None` if the outbox is empty or only contains future-scheduled retries.
    pub fn next_pending(&self, now_ms: u64) -> eyre::Result<Option<PendingBatchRecord>> {
        let tx = self.inner.env.begin_ro_txn()?;
        let mut cursor = tx.cursor_with_dbi(self.inner.pending_dbi)?;
        let mut entry: Option<(Vec<u8>, Vec<u8>)> = cursor.first()?;
        while let Some((_, value)) = entry.as_ref() {
            let record: PendingBatchRecord = serde_json::from_slice(value)?;
            if !record.delivered && record.next_attempt_at <= now_ms {
                return Ok(Some(record));
            }
            entry = cursor.next()?;
        }
        Ok(None)
    }

    /// Mark a batch as delivered and advance the checkpoint to its height.
    pub fn mark_delivered(&self, block_hash: B256) -> eyre::Result<()> {
        let tx = self.inner.env.begin_rw_txn()?;
        let cursor = tx.cursor_with_dbi(self.inner.pending_dbi)?;
        if let Some((key, value)) = find_by_hash(cursor, block_hash)? {
            let record: PendingBatchRecord = serde_json::from_slice(&value)?;
            let checkpoint = serde_json::to_vec(&record.block_number)?;
            tx.put(self.inner.checkpoint_dbi, CHECKPOINT_KEY, &checkpoint, WriteFlags::UPSERT)?;
            // Once delivered, drop the row to keep the outbox bounded.
            tx.del(self.inner.pending_dbi, &key, None)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Bump the attempt counter and reschedule a failed batch.
    pub fn record_attempt(&self, block_hash: B256, next_attempt_at: u64) -> eyre::Result<()> {
        let tx = self.inner.env.begin_rw_txn()?;
        let cursor = tx.cursor_with_dbi(self.inner.pending_dbi)?;
        if let Some((key, value)) = find_by_hash(cursor, block_hash)? {
            let mut record: PendingBatchRecord = serde_json::from_slice(&value)?;
            record.attempts = record.attempts.saturating_add(1);
            record.next_attempt_at = next_attempt_at;
            let encoded = serde_json::to_vec(&record)?;
            tx.put(self.inner.pending_dbi, key, &encoded, WriteFlags::UPSERT)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn put_record(&self, record: &PendingBatchRecord) -> eyre::Result<()> {
        let key = batch_key(record.block_number, record.block_hash);
        let encoded = serde_json::to_vec(record)?;
        let tx = self.inner.env.begin_rw_txn()?;
        tx.put(self.inner.pending_dbi, key, &encoded, WriteFlags::UPSERT)?;
        tx.commit()?;
        Ok(())
    }
}

fn batch_key(block_number: u64, block_hash: B256) -> [u8; 40] {
    let mut key = [0_u8; 40];
    key[..8].copy_from_slice(&block_number.to_be_bytes());
    key[8..].copy_from_slice(block_hash.as_slice());
    key
}

fn find_by_hash<K: reth_libmdbx::TransactionKind>(
    mut cursor: reth_libmdbx::Cursor<K>,
    target: B256,
) -> eyre::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut entry: Option<(Vec<u8>, Vec<u8>)> = cursor.first()?;
    while let Some((key, _)) = entry.as_ref() {
        if key.len() == 40 && key[8..] == target.as_slice()[..] {
            return Ok(entry);
        }
        entry = cursor.next()?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{HexQuantity, TransferName};
    use alloy_primitives::{b256, Address, U256};
    use tempfile::tempdir;

    fn sample_batch(block: u64) -> (B256, u64, Vec<Transfer>) {
        let hash = B256::with_last_byte(block as u8);
        let row = Transfer {
            address: Address::ZERO,
            block_hash: hash,
            block_number: HexQuantity(block),
            from: Address::ZERO,
            log_index: HexQuantity(0),
            name: TransferName::Xpl,
            to: Address::ZERO,
            transaction_hash: b256!(
                "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
            ),
            value: U256::from(1).into(),
        };
        (hash, block, vec![row])
    }

    #[test]
    fn round_trip_append_and_next_pending() {
        let dir = tempdir().unwrap();
        let outbox = Outbox::open(dir.path()).unwrap();
        let (hash, num, rows) = sample_batch(7);
        outbox.append_block_batch(hash, num, rows).unwrap();
        let pending = outbox.next_pending(0).unwrap().expect("pending exists");
        assert_eq!(pending.block_number, 7);
        assert_eq!(pending.block_hash, hash);
        assert_eq!(pending.rows.len(), 1);
        assert!(!pending.delivered);
    }

    #[test]
    fn mark_delivered_removes_from_pending() {
        let dir = tempdir().unwrap();
        let outbox = Outbox::open(dir.path()).unwrap();
        let (hash, num, rows) = sample_batch(3);
        outbox.append_block_batch(hash, num, rows).unwrap();
        outbox.mark_delivered(hash).unwrap();
        assert!(outbox.next_pending(0).unwrap().is_none());
    }

    #[test]
    fn discard_blocks_drops_undelivered() {
        let dir = tempdir().unwrap();
        let outbox = Outbox::open(dir.path()).unwrap();
        let (hash, num, rows) = sample_batch(1);
        outbox.append_block_batch(hash, num, rows).unwrap();
        outbox.discard_blocks([hash]).unwrap();
        assert!(outbox.next_pending(0).unwrap().is_none());
    }

    #[test]
    fn next_pending_respects_next_attempt_at() {
        let dir = tempdir().unwrap();
        let outbox = Outbox::open(dir.path()).unwrap();
        let (hash, num, rows) = sample_batch(2);
        outbox.append_block_batch(hash, num, rows).unwrap();
        outbox.record_attempt(hash, 1_000_000).unwrap();
        assert!(outbox.next_pending(0).unwrap().is_none());
        let r = outbox.next_pending(1_000_000).unwrap().expect("eligible after deadline");
        assert_eq!(r.attempts, 1);
    }

    #[test]
    fn restart_preserves_pending() {
        let dir = tempdir().unwrap();
        let (hash, num, rows) = sample_batch(11);
        {
            let outbox = Outbox::open(dir.path()).unwrap();
            outbox.append_block_batch(hash, num, rows).unwrap();
        }
        let reopened = Outbox::open(dir.path()).unwrap();
        let pending = reopened.next_pending(0).unwrap().expect("survives reopen");
        assert_eq!(pending.block_number, 11);
    }
}
