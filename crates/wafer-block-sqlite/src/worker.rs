//! Dedicated connection-worker thread (PERF-02).
//!
//! `rusqlite::Connection` is synchronous, so every operation previously ran
//! inline on a Tokio executor thread while holding a `Mutex<Connection>` —
//! blocking the async runtime and serializing all database access on one
//! lock. Instead, a dedicated worker thread now owns each connection and
//! callers queue closures to it: the async side only ever awaits a bounded
//! channel send and a `oneshot` reply, so no executor thread blocks on
//! SQLite I/O (the same role the batching worker plays for fastembed).
//!
//! One worker still processes its jobs strictly in order, which preserves
//! the previous mutex semantics exactly: anything that needed to happen
//! under one continuous lock hold (multi-statement DDL, transactions,
//! `INSERT` + `last_insert_rowid()`) is expressed as ONE job that owns
//! `&mut Connection` for its whole duration. Cross-job atomicity never
//! existed under the mutex either — each call took its own lock — so
//! nothing is lost by queueing.

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

/// Upper bound on queued jobs per worker. A full queue applies backpressure
/// through the async `send` (callers suspend, they never block a thread) —
/// bounding the queue keeps an overload from turning into unbounded memory
/// growth behind a slow disk.
const QUEUE_CAP: usize = 256;

/// Error message used when the worker thread is gone (channel closed or a
/// pending reply was dropped, e.g. the job panicked the thread).
pub(crate) const WORKER_GONE: &str = "sqlite connection worker thread has terminated";

/// One queued unit of work, executed with exclusive access to the
/// connection. The closure is responsible for sending its own reply.
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Handle to a dedicated thread owning one `rusqlite::Connection`.
///
/// Dropping every handle closes the channel; the worker finishes queued
/// jobs, then exits and drops the connection.
#[derive(Clone)]
pub(crate) struct ConnWorker {
    tx: mpsc::Sender<Job>,
}

impl ConnWorker {
    /// Spawn a worker thread owning `conn`. `name` is the OS thread name
    /// (shows up in stack dumps / `ps -T`).
    pub(crate) fn spawn(conn: Connection, name: &str) -> Self {
        let (tx, mut rx) = mpsc::channel::<Job>(QUEUE_CAP);
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let mut conn = conn;
                while let Some(job) = rx.blocking_recv() {
                    job(&mut conn);
                }
            })
            .expect("failed to spawn sqlite connection worker thread");
        Self { tx }
    }

    /// Run `f` on the worker thread and await its result.
    ///
    /// `Err(())` means the worker is gone — the channel is closed or the
    /// reply sender was dropped (a panicking job kills its worker; queued
    /// and later jobs then fail here instead of hanging). Callers map this
    /// to their own error type via [`WORKER_GONE`].
    pub(crate) async fn run<T, F>(&self, f: F) -> Result<T, ()>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> T + Send + 'static,
    {
        let (respond, rx) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            // The receiver may have been dropped (caller cancelled); the
            // job still ran to completion, matching mutex-era semantics
            // where a cancelled future could not undo an executed call.
            let _ = respond.send(f(conn));
        });
        self.tx.send(job).await.map_err(|_| ())?;
        rx.await.map_err(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Generous CI-safe bound: operations complete immediately when correct;
    /// a hit means a deadlock/lost-reply regression, not slowness.
    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    async fn run_with_timeout<T, F>(worker: &ConnWorker, f: F) -> Result<T, ()>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> T + Send + 'static,
    {
        tokio::time::timeout(TEST_TIMEOUT, worker.run(f))
            .await
            .expect("ConnWorker::run timed out — worker deadlocked or lost the reply")
    }

    fn mem_worker() -> ConnWorker {
        ConnWorker::spawn(
            Connection::open_in_memory().expect("open in-memory"),
            "sqlite-test",
        )
    }

    #[tokio::test]
    async fn runs_jobs_in_order_on_one_connection() {
        let worker = mem_worker();
        run_with_timeout(&worker, |conn| {
            conn.execute_batch("CREATE TABLE t (v INTEGER)").unwrap();
        })
        .await
        .expect("ddl job");
        run_with_timeout(&worker, |conn| {
            conn.execute("INSERT INTO t (v) VALUES (7)", []).unwrap();
        })
        .await
        .expect("insert job");
        let v: i64 = run_with_timeout(&worker, |conn| {
            conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap()
        })
        .await
        .expect("select job");
        assert_eq!(v, 7, "jobs must observe earlier jobs' effects in order");
    }

    /// A panicking job kills its worker; the in-flight caller and all later
    /// callers get `Err(())`, never a hang.
    #[tokio::test]
    async fn dead_worker_errors_instead_of_hanging() {
        let worker = mem_worker();
        let res = tokio::time::timeout(
            TEST_TIMEOUT,
            worker.run(|_conn| -> () { panic!("job dies") }),
        )
        .await
        .expect("must not hang");
        assert!(res.is_err(), "in-flight job on dying worker must error");

        let res = tokio::time::timeout(TEST_TIMEOUT, worker.run(|_conn| ()))
            .await
            .expect("must not hang");
        assert!(res.is_err(), "later jobs on a dead worker must error");
    }

    /// Dropping the handle after enqueueing still answers the queued job —
    /// the worker drains the channel before exiting (no lost work).
    #[tokio::test]
    async fn drop_after_enqueue_still_answers_queued_job() {
        let worker = mem_worker();
        let pending = {
            let w = worker.clone();
            tokio::spawn(async move { w.run(|_conn| 42_i32).await })
        };
        drop(worker);
        let out = tokio::time::timeout(TEST_TIMEOUT, pending)
            .await
            .expect("must not hang")
            .expect("task join");
        assert_eq!(out, Ok(42));
    }

    /// Concurrent callers all get their own results back (reply routing).
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_jobs_route_replies_to_their_callers() {
        let worker = mem_worker();
        let mut handles = Vec::new();
        for i in 0..32_i64 {
            let w = worker.clone();
            handles.push(tokio::spawn(async move { w.run(move |_conn| i).await }));
        }
        for (i, h) in handles.into_iter().enumerate() {
            let got = tokio::time::timeout(TEST_TIMEOUT, h)
                .await
                .expect("must not hang")
                .expect("task join");
            assert_eq!(got, Ok(i as i64));
        }
    }
}
