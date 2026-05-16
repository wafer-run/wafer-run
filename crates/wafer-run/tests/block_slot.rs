//! BlockSlot: the per-block init state container.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use wafer_run::runtime::slot::{BlockSlot, InitError, InitializedState};

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

    // Second call: succeeds
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
        "transient must not be cached; second call retries"
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
