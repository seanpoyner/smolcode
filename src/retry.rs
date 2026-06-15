//! Retry/backoff policy helpers for transient provider errors.
//!
//! smolcode sends chat-completion requests to an OpenAI-compatible endpoint;
//! these can fail transiently (connection reset, 429, 5xx, timeouts). This
//! module provides a transient-error classifier ([`is_transient`]), an
//! exponential backoff schedule ([`Policy`]), and a generic async retry
//! wrapper ([`retry_async`]). Backoff is deterministic (no jitter) so it is
//! testable. Nothing here panics.

use std::time::Duration;

/// Retry/backoff configuration.
#[derive(Clone, Copy)]
pub struct Policy {
    /// Total attempts (including the first try). `1` means no retries.
    pub max_attempts: usize,
    /// Base delay in milliseconds for the first retry.
    pub base_ms: u64,
    /// Upper bound on any single backoff delay, in milliseconds.
    pub max_ms: u64,
}

impl Policy {
    /// Sensible default: 4 attempts, 400ms base, capped at 8s.
    pub fn default_policy() -> Self {
        Policy {
            max_attempts: 4,
            base_ms: 400,
            max_ms: 8000,
        }
    }

    /// Backoff delay (exponential, capped) before attempt index `attempt`
    /// (0-based: attempt 0 = first retry). Deterministic (no jitter):
    /// `base_ms * 2^attempt`, capped at `max_ms`. Overflow-safe.
    pub fn delay_ms(&self, attempt: usize) -> u64 {
        // Cap the exponent so 2^attempt never overflows u64.
        let exp = attempt.min(16) as u32;
        let factor = 2u64.saturating_pow(exp);
        self.base_ms.saturating_mul(factor).min(self.max_ms)
    }
}

/// True if the error text looks like a transient/retryable failure.
///
/// Case-insensitive substring checks for common transient signatures
/// (timeout, connection reset/refused, 429, 5xx, "temporarily",
/// "rate limit", "overloaded"). Returns false otherwise so non-transient
/// errors fail fast.
pub fn is_transient(err: &str) -> bool {
    let e = err.to_lowercase();
    const NEEDLES: &[&str] = &[
        "timeout",
        "timed out",
        "connection reset",
        "connection refused",
        "connection closed",
        "reset by peer",
        "429",
        "500",
        "502",
        "503",
        "504",
        "temporarily",
        "rate limit",
        "overloaded",
    ];
    NEEDLES.iter().any(|n| e.contains(n))
}

/// Retry an async operation per `policy`, but only while the error is
/// transient (per [`is_transient`] applied to the error's `Display`).
///
/// Returns `Ok` on the first success, or the last error if every attempt
/// fails or a non-transient error is encountered. `op` is called up to
/// `policy.max_attempts` times; between attempts it sleeps for the
/// corresponding backoff delay.
pub async fn retry_async<T, E, F, Fut>(policy: Policy, mut op: F) -> Result<T, E>
where
    E: std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let attempts = policy.max_attempts.max(1);
    for attempt in 0..attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let last = attempt + 1 >= attempts;
                if last || !is_transient(&e.to_string()) {
                    return Err(e);
                }
                // `attempt` is 0-based: 0 = delay before the 2nd try.
                let delay = policy.delay_ms(attempt);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }
    }
    // Unreachable: the loop always returns on the final attempt, but keep
    // a defensive fallback by running one last try without retrying.
    op().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn classifies_transient_errors() {
        assert!(is_transient("connection reset by peer"));
        assert!(is_transient("HTTP 503"));
        assert!(is_transient("429 Too Many Requests"));
        assert!(is_transient("request timed out"));
        assert!(is_transient("server overloaded"));
    }

    #[test]
    fn classifies_non_transient_errors() {
        assert!(!is_transient("invalid api key"));
        assert!(!is_transient("model not found"));
        assert!(!is_transient("bad request"));
    }

    #[test]
    fn backoff_schedule_is_exponential_and_capped() {
        let p = Policy {
            max_attempts: 4,
            base_ms: 400,
            max_ms: 8000,
        };
        assert_eq!(p.delay_ms(0), 400);
        assert_eq!(p.delay_ms(1), 800);
        assert_eq!(p.delay_ms(2), 1600);
        assert_eq!(p.delay_ms(3), 3200);
        assert_eq!(p.delay_ms(10), 8000); // capped
    }

    fn tiny_policy() -> Policy {
        Policy {
            max_attempts: 4,
            base_ms: 1,
            max_ms: 2,
        }
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        let calls = AtomicUsize::new(0);
        let result: Result<&str, String> = retry_async(tiny_policy(), || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err("connection reset by peer".to_string())
                } else {
                    Ok("ok")
                }
            }
        })
        .await;
        assert_eq!(result, Ok("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_transient_fails_fast() {
        let calls = AtomicUsize::new(0);
        let result: Result<(), String> = retry_async(tiny_policy(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err("invalid api key".to_string()) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exhausts_attempts_on_persistent_transient() {
        let calls = AtomicUsize::new(0);
        let result: Result<(), String> = retry_async(tiny_policy(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err("HTTP 503".to_string()) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
