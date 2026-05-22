use crate::{args::XplWebhookArgs, metrics as m, outbox::Outbox, payload::WebhookBody};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Notify, task::JoinHandle};
use tracing::{debug, error, warn};

type HmacSha256 = Hmac<Sha256>;

/// Background HTTP worker draining the [`Outbox`] in block-number order.
pub struct WebhookDispatcher {
    shutdown: Arc<Notify>,
    handle: JoinHandle<()>,
}

impl WebhookDispatcher {
    pub fn spawn(args: XplWebhookArgs, outbox: Outbox) -> Self {
        let shutdown = Arc::new(Notify::new());
        let notify_clone = shutdown.clone();
        let wakeup = outbox.wakeup_handle();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(args.http_timeout_ms))
            .build()
            .expect("reqwest client builds");

        let handle = tokio::spawn(async move {
            run_dispatcher(args, outbox, client, wakeup, notify_clone).await;
        });

        Self { shutdown, handle }
    }

    pub async fn shutdown(self) {
        self.shutdown.notify_waiters();
        let _ = self.handle.await;
    }
}

async fn run_dispatcher(
    args: XplWebhookArgs,
    outbox: Outbox,
    client: reqwest::Client,
    wakeup: Arc<Notify>,
    shutdown: Arc<Notify>,
) {
    loop {
        tokio::select! {
            biased;
            _ = shutdown.notified() => return,
            _ = drain_loop(&args, &outbox, &client, &wakeup) => {}
        }
    }
}

async fn drain_loop(
    args: &XplWebhookArgs,
    outbox: &Outbox,
    client: &reqwest::Client,
    wakeup: &Arc<Notify>,
) {
    let now_ms = unix_ms();
    let batch = match outbox.next_pending(now_ms) {
        Ok(Some(batch)) => batch,
        Ok(None) => {
            // nothing to send: wait for the producer to nudge us
            wakeup.notified().await;
            return;
        }
        Err(err) => {
            error!(target: "xpl-webhook", %err, "outbox read failed");
            tokio::time::sleep(Duration::from_secs(1)).await;
            return;
        }
    };

    let body = WebhookBody { result: &batch.rows };
    let raw_body = match serde_json::to_vec(&body) {
        Ok(bytes) => bytes,
        Err(err) => {
            error!(target: "xpl-webhook", %err, "payload serialization failed; dropping batch");
            let _ = outbox.mark_delivered(batch.block_hash);
            return;
        }
    };

    let nonce = random_nonce_hex();
    let timestamp = unix_ms().to_string();
    let signature = sign_payload(&args.token, &nonce, &timestamp, &raw_body);

    debug!(
        target: "xpl-webhook",
        block_number = batch.block_number,
        rows = batch.rows.len(),
        attempt = batch.attempts + 1,
        "posting batch",
    );

    let result = client
        .post(&args.url)
        .header("content-type", "application/json")
        .header("x-qn-signature", signature)
        .header("x-qn-nonce", nonce)
        .header("x-qn-timestamp", timestamp)
        .body(raw_body)
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            ::metrics::counter!(m::HTTP_SUCCESS).increment(1);
            ::metrics::counter!(m::ROWS_DELIVERED).increment(batch.rows.len() as u64);
            if let Err(err) = outbox.mark_delivered(batch.block_hash) {
                error!(target: "xpl-webhook", %err, "failed to mark batch delivered");
            }
        }
        Ok(resp) => {
            warn!(
                target: "xpl-webhook",
                status = %resp.status(),
                block_number = batch.block_number,
                "webhook non-2xx",
            );
            handle_failure(args, outbox, &batch);
        }
        Err(err) => {
            warn!(
                target: "xpl-webhook",
                %err,
                block_number = batch.block_number,
                "webhook request failed",
            );
            handle_failure(args, outbox, &batch);
        }
    }
}

fn handle_failure(
    args: &XplWebhookArgs,
    outbox: &Outbox,
    batch: &crate::outbox::PendingBatchRecord,
) {
    ::metrics::counter!(m::HTTP_FAILURES).increment(1);
    let next_attempt_at = unix_ms().saturating_add(backoff_ms(batch.attempts));
    if let Err(err) = outbox.record_attempt(batch.block_hash, next_attempt_at) {
        error!(target: "xpl-webhook", %err, "failed to record attempt");
    }
    if batch.attempts + 1 >= args.max_retries {
        ::metrics::counter!(m::DLQ).increment(1);
        warn!(
            target: "xpl-webhook",
            block_number = batch.block_number,
            attempts = batch.attempts + 1,
            "batch reached max retries; remains pending for operator intervention",
        );
    }
}

/// Capped exponential backoff in milliseconds.
const BACKOFF_LADDER_MS: &[u64] = &[500, 1_000, 2_000, 5_000, 15_000, 30_000, 60_000, 120_000];

fn backoff_ms(attempts: u32) -> u64 {
    let idx = (attempts as usize).min(BACKOFF_LADDER_MS.len() - 1);
    BACKOFF_LADDER_MS[idx]
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn random_nonce_hex() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// HMAC-SHA256 of `nonce || timestamp || raw_body` keyed with `token`,
/// returned as lowercase hex. Matches the controller's verifier in
/// `apps/tasks`.
pub fn sign_payload(token: &str, nonce: &str, timestamp: &str, raw_body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(token.as_bytes()).expect("HMAC accepts any key size");
    mac.update(nonce.as_bytes());
    mac.update(timestamp.as_bytes());
    mac.update(raw_body);
    let digest = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::sign_payload;

    #[test]
    fn hmac_signature_matches_precomputed_digest() {
        // Precomputed with:
        //   echo -n "nonce0123timestamp9999BODY" | openssl dgst -sha256 -hmac "secrettoken"
        let sig = sign_payload("secrettoken", "nonce0123", "timestamp9999", b"BODY");
        assert_eq!(sig, "b0f45fe2ba1e62484e1ab5ac9082b205694fff2c8b5352ab0dda5dd3ea47e609");
    }

    #[test]
    fn hmac_signature_includes_body_bytes() {
        let a = sign_payload("k", "n", "t", b"{\"result\":[]}");
        let b = sign_payload("k", "n", "t", b"{\"result\":[{}]}");
        assert_ne!(a, b);
    }
}
