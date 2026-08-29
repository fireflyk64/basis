//! Retrying transient faults.
//!
//! Only a [`FaultKind::Transient`](crate::FaultKind::Transient) error is retried; a permanent one
//! is returned at once, because repeating a request that a malformed config or a bad key will
//! always refuse only delays the operator finding out.

use std::time::Duration;

use crate::{BasisError, BasisResult, RetryPolicy};

fn seed_from_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
}

/// Decides what to do after a failed attempt.
fn next_step(policy: &RetryPolicy, attempt: u32, err: &BasisError, what: &str) -> Result<Duration, String> {
    if !err.is_transient() {
        return Err(format!("{what}: permanent fault on attempt {attempt}"));
    }
    if attempt >= policy.max_attempts {
        return Err(format!("{what}: gave up after {attempt} attempt(s)"));
    }
    Ok(policy.delay_for(attempt, seed_from_clock()))
}

/// Runs `op` until it succeeds, returns a permanent fault, or the policy is exhausted.
/// The closure receives the 1-based attempt number.
pub fn retry_blocking<T>(policy: RetryPolicy, what: &str, mut op: impl FnMut(u32) -> BasisResult<T>) -> BasisResult<T> {
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.saturating_add(1);
        match op(attempt) {
            Ok(value) => return Ok(value),
            Err(err) => match next_step(&policy, attempt, &err, what) {
                Ok(delay) => std::thread::sleep(delay),
                Err(note) => return Err(err.context(note)),
            },
        }
    }
}

/// Async form of [`retry_blocking`] on tokio's timer.
#[cfg(feature = "tokio")]
pub async fn retry_async<T, F, Fut>(policy: RetryPolicy, what: &str, mut op: F) -> BasisResult<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = BasisResult<T>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.saturating_add(1);
        match op(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => match next_step(&policy, attempt, &err, what) {
                Ok(delay) => tokio::time::sleep(delay).await,
                Err(note) => return Err(err.context(note)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn fast_policy(attempts: u32) -> RetryPolicy {
        RetryPolicy::new(attempts, Duration::from_millis(1), Duration::from_millis(2))
    }

    #[test]
    fn succeeds_after_transient_failures() {
        let result = retry_blocking(fast_policy(4), "dial", |attempt| {
            if attempt < 3 {
                Err(BasisError::transient(ErrorCode::Transport, "refused"))
            } else {
                Ok(attempt)
            }
        });
        assert_eq!(result.ok(), Some(3));
    }

    #[test]
    fn gives_up_when_exhausted_and_says_so() {
        let mut calls = 0;
        let result = retry_blocking(fast_policy(3), "dial", |_| {
            calls += 1;
            Err::<(), _>(BasisError::transient(ErrorCode::Transport, "refused"))
        });
        assert_eq!(calls, 3);
        let err = result.err();
        assert!(err.as_ref().is_some_and(|e| e.is_transient()));
        assert!(err.is_some_and(|e| e.frames().last().is_some_and(|f| f.note().contains("gave up after 3"))));
    }

    #[test]
    fn permanent_faults_are_not_retried() {
        let mut calls = 0;
        let result = retry_blocking(fast_policy(5), "load", |_| {
            calls += 1;
            Err::<(), _>(BasisError::permanent(ErrorCode::Config, "bad value"))
        });
        assert_eq!(calls, 1);
        assert!(result.err().is_some_and(|e| e.is_permanent() && e.to_string() == "bad value"));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn async_retry_succeeds_after_transient_failures() {
        let result = retry_async(fast_policy(4), "dial", |attempt| async move {
            if attempt < 2 {
                Err(BasisError::transient(ErrorCode::Transport, "refused"))
            } else {
                Ok(attempt)
            }
        })
        .await;
        assert_eq!(result.ok(), Some(2));
    }
}
