//! Shared metric names so callers don't drift on string constants.

pub const BLOCKS_PROCESSED: &str = "xpl_webhook_blocks_processed";
pub const ROWS_EMITTED: &str = "xpl_webhook_rows_emitted";
pub const ROWS_DELIVERED: &str = "xpl_webhook_rows_delivered";
pub const HTTP_FAILURES: &str = "xpl_webhook_http_failures";
pub const HTTP_SUCCESS: &str = "xpl_webhook_http_success";
pub const DLQ: &str = "xpl_webhook_dlq";
pub const REORGS: &str = "xpl_webhook_reorgs";
pub const REVERTS: &str = "xpl_webhook_reverts";
pub const REPLAY_ERRORS: &str = "xpl_webhook_replay_errors";
