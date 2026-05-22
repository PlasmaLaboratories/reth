use alloy_primitives::Address;
use clap::Parser;
use std::path::PathBuf;

/// CLI extension flags appended to the standard `reth` argument set. Each
/// argument ID is namespaced with the `xpl_webhook_` prefix so it doesn't
/// collide with the built-in node flags (e.g. era `--url`).
#[derive(Debug, Clone, Parser)]
pub struct XplWebhookArgs {
    /// Webhook URL that receives `{ result: Transfer[] }` POSTs.
    #[arg(id = "xpl_webhook_url", long = "xpl-webhook-url", env = "XPL_WEBHOOK_URL")]
    pub url: String,

    /// HMAC-SHA256 token used to sign requests. Read from `XPL_WEBHOOK_TOKEN`
    /// when the flag is omitted.
    #[arg(id = "xpl_webhook_token", long = "xpl-webhook-token", env = "XPL_WEBHOOK_TOKEN")]
    pub token: String,

    /// Address emitted in the `address` field of every transfer row. Defaults
    /// to the zero address (native XPL marker).
    #[arg(
        id = "xpl_webhook_native_address",
        long = "xpl-webhook-native-address",
        default_value = "0x0000000000000000000000000000000000000000"
    )]
    pub native_address: Address,

    /// Path to the MDBX outbox directory. Defaults to `<datadir>/xpl-webhook`.
    #[arg(id = "xpl_webhook_outbox_path", long = "xpl-webhook-outbox-path")]
    pub outbox_path: Option<PathBuf>,

    /// Maximum number of HTTP delivery attempts per batch before backing off
    /// to the configured ceiling.
    #[arg(id = "xpl_webhook_max_retries", long = "xpl-webhook-max-retries", default_value_t = 8)]
    pub max_retries: u32,

    /// HTTP request timeout in milliseconds.
    #[arg(
        id = "xpl_webhook_http_timeout_ms",
        long = "xpl-webhook-http-timeout-ms",
        default_value_t = 5_000
    )]
    pub http_timeout_ms: u64,
}
