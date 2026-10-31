//! BlockSlot: the per-block init state container.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use wafer_run::runtime::slot::{BlockSlot, InitError, InitializedState, TRANSIENT_RETRY_BASE};

#[tokio::test]
async fn get_or_init_runs_init_once() {
    let slot = BlockSlot::new();
    let calls = Arc::new(AtomicUsize::new(0));

    for _ in 0..10 {
        let calls = calls.clone();
        let _ = slot
            .get_or_init(|| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, InitError>(InitializedState::new())
            })
            .await
            .expect("init must succeed");
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "init must run exactly once"
    );
}

#[tokio::test]
async fn permanent_init_error_is_cached() {
    let slot = BlockSlot::new();
    let calls = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let calls = calls.clone();
        let result = slot
            .get_or_init(|| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<InitializedState, _>(InitError::Permanent(
                    "missing required config".to_string(),
                ))
            })
            .await;

        assert!(matches!(result, Err(InitError::Permanent(_))));
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "permanent error must be cached, not retried"
    );
}

#[tokio::test]
async fn transient_init_error_is_not_cached() {
    let slot = BlockSlot::new();
    let calls = Arc::new(AtomicUsize::new(0));

    // First call: transient error
    {
        let calls = calls.clone();
        let result = slot
            .get_or_init(|| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<InitializedState, _>(InitError::Transient("d1 timeout".to_string()))
            })
            .await;
        assert!(matches!(result, Err(InitError::Transient(_))));
    }

    // Inside the retry backoff: the failure comes back, init does not run.
    {
        let calls = calls.clone();
        let result = slot
            .get_or_init(|| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, InitError>(InitializedState::new())
            })
            .await;
        assert!(
            matches!(result, Err(InitError::Transient(_))),
            "inside the backoff the transient failure is returned: {result:?}"
        );
        assert!(
            matches!(slot.try_cached(), Some(Err(InitError::Transient(_)))),
            "the fast path also answers inside the backoff"
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no retry inside the backoff"
    );

    tokio::time::sleep(TRANSIENT_RETRY_BASE + TRANSIENT_RETRY_BASE / 2).await;

    // After the backoff: init runs again and succeeds
    {
        let calls = calls.clone();
        let result = slot
            .get_or_init(|| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, InitError>(InitializedState::new())
            })
            .await;
        assert!(result.is_ok(), "second call must retry and succeed");
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "transient must not be cached; the call after the backoff retries"
    );

    // Third call: succeeds without re-running (cached now that init succeeded)
    {
        let calls = calls.clone();
        let _ = slot
            .get_or_init(|| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, InitError>(InitializedState::new())
            })
            .await;
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "successful init must be cached after retry"
    );
}

#[tokio::test]
async fn pending_retry_in_names_the_backoff_only_while_it_lasts() {
    let slot = BlockSlot::new();
    assert_eq!(
        slot.pending_retry_in(),
        None,
        "an empty slot has no backoff"
    );

    let result = slot
        .get_or_init(|| async { Err::<InitializedState, _>(InitError::Transient("down".into())) })
        .await;
    assert!(matches!(result, Err(InitError::Transient(_))));
    let pending = slot
        .pending_retry_in()
        .expect("a transient failure starts a backoff");
    assert!(
        pending > std::time::Duration::ZERO && pending <= TRANSIENT_RETRY_BASE,
        "the first backoff is at most the base: {pending:?}"
    );

    tokio::time::sleep(TRANSIENT_RETRY_BASE + TRANSIENT_RETRY_BASE / 2).await;
    assert_eq!(
        slot.pending_retry_in(),
        None,
        "an elapsed backoff names no delay"
    );

    let result = slot
        .get_or_init(|| async { Ok::<_, InitError>(InitializedState::new()) })
        .await;
    assert!(result.is_ok());
    assert_eq!(slot.pending_retry_in(), None, "a settled Ok has no backoff");
}
