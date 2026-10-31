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

/// `init_state` reads each state without running or waiting for init: an
/// attempt in flight is `Initializing`, and the read returns while it runs.
#[tokio::test]
async fn init_state_reports_each_state_without_waiting_for_an_attempt() {
    use wafer_run::BlockInitState;

    let slot = Arc::new(BlockSlot::new());
    assert_eq!(slot.init_state(), BlockInitState::NotInitialized);

    // A transient failure: retrying, with the backoff left.
    let _ = slot
        .get_or_init(|| async { Err::<InitializedState, _>(InitError::Transient("down".into())) })
        .await;
    match slot.init_state() {
        BlockInitState::Retrying { reason, retry_in } => {
            assert_eq!(reason, "down");
            // At most the first backoff; it may already have elapsed on a
            // slow runner (then zero), so no lower bound is asserted.
            assert!(retry_in <= TRANSIENT_RETRY_BASE, "{retry_in:?}");
        }
        other => panic!("expected Retrying, got {other:?}"),
    }
    tokio::time::sleep(TRANSIENT_RETRY_BASE + TRANSIENT_RETRY_BASE / 2).await;
    assert_eq!(
        slot.init_state(),
        BlockInitState::Retrying {
            reason: "down".into(),
            retry_in: std::time::Duration::ZERO,
        },
        "past the backoff the last failure is still reported, due now"
    );

    // An attempt in flight.
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let attempt = {
        let (slot, entered, release) = (slot.clone(), entered.clone(), release.clone());
        tokio::spawn(async move {
            slot.get_or_init(|| async move {
                entered.notify_one();
                release.notified().await;
                Ok::<_, InitError>(InitializedState::new())
            })
            .await
        })
    };
    entered.notified().await;
    assert_eq!(slot.init_state(), BlockInitState::Initializing);
    assert!(
        slot.try_cached().is_none(),
        "an attempt in flight is not settled"
    );
    release.notify_one();
    attempt.await.expect("join").expect("init succeeds");
    assert_eq!(slot.init_state(), BlockInitState::Ready);

    // A permanent failure on a fresh slot.
    let failed = BlockSlot::new();
    let _ = failed
        .get_or_init(|| async { Err::<InitializedState, _>(InitError::Permanent("bad".into())) })
        .await;
    assert_eq!(
        failed.init_state(),
        BlockInitState::Failed {
            reason: "bad".into()
        }
    );
}

/// Readers on OS threads, `READS` each.
const READERS: usize = 8;
const READS: usize = 100_000;

/// Run `read` on [`READERS`] OS threads, [`READS`] times each, and return
/// how many calls it answered `false`.
fn concurrent_misreads(read: impl Fn() -> bool + Send + Sync + 'static) -> usize {
    let read = Arc::new(read);
    let start = Arc::new(std::sync::Barrier::new(READERS));
    // Every reader is spawned before any is joined: the barrier releases
    // them only once all `READERS` wait on it.
    let mut threads = Vec::with_capacity(READERS);
    for _ in 0..READERS {
        let (read, start) = (read.clone(), start.clone());
        threads.push(std::thread::spawn(move || {
            start.wait();
            (0..READS).filter(|_| !read()).count()
        }));
    }
    threads
        .into_iter()
        .map(|t| t.join().expect("reader thread"))
        .sum()
}

/// Concurrent readers of an idle slot see it idle: one reader's look never
/// makes another report an attempt in flight (a reader that probed the
/// attempt lock did — about a quarter of reads answered `Initializing`).
#[tokio::test]
async fn concurrent_readers_never_see_an_idle_slot_in_flight() {
    use wafer_run::BlockInitState;

    let never = Arc::new(BlockSlot::new());
    let misreads = {
        let slot = never.clone();
        concurrent_misreads(move || slot.init_state() == BlockInitState::NotInitialized)
    };
    assert_eq!(
        misreads, 0,
        "a never-initialized slot read as something else"
    );

    // A transient failure whose backoff has elapsed keeps its class.
    let elapsed = Arc::new(BlockSlot::new());
    let _ = elapsed
        .get_or_init(|| async { Err::<InitializedState, _>(InitError::Transient("down".into())) })
        .await;
    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
    let misreads = {
        let slot = elapsed.clone();
        concurrent_misreads(move || {
            matches!(
                slot.init_state(),
                BlockInitState::Retrying { retry_in, .. } if retry_in.is_zero()
            )
        })
    };
    assert_eq!(
        misreads, 0,
        "an elapsed transient slot read as something else"
    );
}

/// Concurrent readers of a settled slot see it settled: the outcome lives
/// under the state lock, so no reader misses it because another is looking
/// (with the outcome behind a `try_lock`, readers answered "unsettled").
#[tokio::test]
async fn concurrent_readers_see_a_settled_slot_settled() {
    use wafer_run::BlockInitState;

    let slot = Arc::new(BlockSlot::new());
    slot.get_or_init(|| async { Ok::<_, InitError>(InitializedState::new()) })
        .await
        .expect("init succeeds");
    let misreads = concurrent_misreads(move || {
        matches!(slot.try_cached(), Some(Ok(_))) && slot.init_state() == BlockInitState::Ready
    });
    assert_eq!(misreads, 0, "a Ready slot read as unsettled");
}

/// A reader never sees an attempt go backwards — `Initializing` or `Ready`
/// and then `NotInitialized` — while it runs. The in-flight flag is cleared
/// under the same lock that stores the outcome, so there is no moment with
/// neither (a reader that read the state, then probed the attempt lock,
/// could land in one).
#[tokio::test]
async fn init_state_never_goes_backwards_across_an_attempt() {
    use std::sync::atomic::AtomicBool;

    use wafer_run::BlockInitState;

    const ATTEMPTS: u64 = 30_000;

    fn rank(state: &BlockInitState) -> u8 {
        match state {
            BlockInitState::NotInitialized => 0,
            BlockInitState::Initializing => 1,
            _ => 2,
        }
    }

    let current = Arc::new(parking_lot::Mutex::new((0u64, Arc::new(BlockSlot::new()))));
    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let (current, done) = (current.clone(), done.clone());
        std::thread::spawn(move || {
            let (mut generation, mut highest, mut backwards) = (u64::MAX, 0u8, 0usize);
            while !done.load(Ordering::Relaxed) {
                let (seen, slot) = {
                    let guard = current.lock();
                    (guard.0, guard.1.clone())
                };
                let r = rank(&slot.init_state());
                if seen != generation {
                    (generation, highest) = (seen, r);
                } else if r < highest {
                    backwards += 1;
                } else {
                    highest = r;
                }
            }
            backwards
        })
    };

    for attempt in 1..=ATTEMPTS {
        let slot = Arc::new(BlockSlot::new());
        *current.lock() = (attempt, slot.clone());
        slot.get_or_init(|| async {
            tokio::task::yield_now().await;
            Ok::<_, InitError>(InitializedState::new())
        })
        .await
        .expect("init succeeds");
    }
    done.store(true, Ordering::Relaxed);
    let backwards = reader.join().expect("reader thread");
    assert_eq!(backwards, 0, "a reader saw an attempt's state go backwards");
}

/// A cancelled attempt leaves the slot as it found it: not in flight.
#[tokio::test]
async fn a_cancelled_attempt_is_not_left_in_flight() {
    use wafer_run::BlockInitState;

    let slot = BlockSlot::new();
    let parked = slot.get_or_init(|| async {
        std::future::pending::<()>().await;
        Ok::<_, InitError>(InitializedState::new())
    });
    let timed_out = tokio::time::timeout(std::time::Duration::from_millis(20), parked).await;
    assert!(timed_out.is_err(), "the attempt is parked");
    assert_eq!(slot.init_state(), BlockInitState::NotInitialized);
    assert!(slot.try_cached().is_none());
}
