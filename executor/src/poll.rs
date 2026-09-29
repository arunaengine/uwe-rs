//! Backoff, cancellation and grace periods shared by remote executors.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::future::Future;
use std::time::{Duration, Instant};

use crate::CancellationToken;

pub const CANCEL_GRACE: Duration = Duration::from_secs(60);
pub const REQUEST_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum WaitError {
    Cancelled,
    Expired,
}

pub struct Poll {
    pause: Duration,
    maximum: Duration,
    trouble: Option<Instant>,
    deadline: Option<tokio::time::Instant>,
}

impl Poll {
    pub fn new(maximum: Duration) -> Self {
        Self {
            pause: Duration::from_millis(250),
            maximum,
            trouble: None,
            deadline: None,
        }
    }

    pub async fn read<T>(
        &mut self,
        cancel: &CancellationToken,
        read: impl Future<Output = T>,
    ) -> Result<T, WaitError> {
        let expiry = async {
            match self.deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let answer = tokio::select! {
            answer = async { tokio::time::sleep(self.pause).await; read.await } => Ok(answer),
            () = cancel.cancelled() => Err(WaitError::Cancelled),
            () = expiry => Err(WaitError::Expired),
        };
        self.pause = (self.pause * 2).min(self.maximum);
        answer
    }

    pub fn running_for(&mut self, seconds: Option<u64>) {
        if self.deadline.is_none() {
            self.deadline =
                seconds.map(|seconds| tokio::time::Instant::now() + Duration::from_secs(seconds));
        }
    }

    pub fn troubled(&mut self, problem: bool, grace: Duration) -> bool {
        if !problem {
            self.trouble = None;
            return false;
        }
        self.trouble.get_or_insert_with(Instant::now).elapsed() >= grace
    }
}

/// Repeats termination checks under one total deadline, retaining backend identity checks.
pub async fn until_stopped<T, F, Fut>(
    grace: Duration,
    pause: Duration,
    mut check: F,
) -> Result<T, tokio::time::error::Elapsed>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    tokio::time::timeout(grace, async {
        loop {
            if let Some(answer) = check().await {
                return answer;
            }
            tracing::debug!("remote termination is not yet confirmed; retrying");
            tokio::time::sleep(pause).await;
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn interrupts_cancelled_polling() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            Poll::new(Duration::from_secs(5))
                .read(&cancel, std::future::pending::<()>())
                .await,
            Err(WaitError::Cancelled)
        );
        let mut poll = Poll::new(Duration::from_secs(5));
        poll.running_for(Some(0));
        assert_eq!(
            poll.read(&CancellationToken::new(), std::future::pending::<()>())
                .await,
            Err(WaitError::Expired)
        );
    }

    #[test]
    fn resets_trouble_grace() {
        let mut poll = Poll::new(Duration::from_secs(5));
        assert!(poll.troubled(true, Duration::ZERO));
        assert!(!poll.troubled(false, Duration::ZERO));
        assert!(!poll.troubled(true, Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn bounds_termination_retries() {
        let mut calls = 0;
        let answer = until_stopped(Duration::from_secs(1), Duration::from_millis(1), || {
            calls += 1;
            std::future::ready((calls == 2).then_some("stopped"))
        })
        .await
        .unwrap();
        assert_eq!(answer, "stopped");
        assert_eq!(calls, 2);
        assert!(
            until_stopped(
                Duration::from_millis(10),
                Duration::from_millis(1),
                std::future::pending::<Option<()>>
            )
            .await
            .is_err()
        );
    }
}
