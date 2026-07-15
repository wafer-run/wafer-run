//! Single-model embedding batcher (PERF-05).
//!
//! The fastembed `TextEmbedding::embed` forward pass takes `&mut self`, so
//! one model instance can only run one forward pass at a time. Previously the
//! service shared the model behind a `Mutex` and every concurrent `embed`
//! call fully serialized on it — N callers took N sequential forward passes.
//!
//! A pool of model instances would remove the serialization but multiplies
//! model memory (each `TextEmbedding` owns a full ONNX session — hundreds of
//! MB for the catalog models), which is disproportionate for this service.
//! Instead, a single dedicated worker thread owns the model and callers queue
//! requests to it: while a forward pass runs, newly arriving requests
//! accumulate in the channel, and the worker drains them into ONE batched
//! forward pass. ONNX batch inference amortizes per-pass overhead across
//! texts, so N queued callers cost ~1 batched pass instead of N sequential
//! passes, without any additional model memory.
//!
//! The worker is a plain blocking thread (the same role `spawn_blocking`
//! played before — CPU-heavy sync work stays off the async workers); callers
//! only ever await a `oneshot`, so no async task blocks.

use tokio::sync::oneshot;

/// Upper bound on texts coalesced into one forward pass. Bounds per-batch
/// latency and peak activation memory; `TextEmbedding::embed` additionally
/// chunks internally by its own batch size. A single request larger than
/// this is still processed whole (requests are never split).
const MAX_BATCH_TEXTS: usize = 256;

/// Error message returned when the worker thread is gone (its request
/// channel is closed or it dropped a pending response).
const WORKER_GONE: &str = "fastembed embedding worker thread has terminated";

/// Result of one embedding request: one vector per input text, in order, or
/// a service-error message.
pub(crate) type EmbedResult = Result<Vec<Vec<f32>>, String>;

/// One queued embedding request: input texts and the channel that receives
/// the matching output vectors (one per text, in order).
struct BatchRequest {
    texts: Vec<String>,
    respond: oneshot::Sender<EmbedResult>,
}

/// Handle to the dedicated embedding worker thread.
///
/// Dropping the batcher closes the request channel, which terminates the
/// worker thread after it finishes any in-flight batch.
pub(crate) struct EmbedBatcher {
    tx: std::sync::mpsc::Sender<BatchRequest>,
}

/// A request accepted by [`EmbedBatcher::enqueue`], awaiting its result.
pub(crate) struct PendingEmbed {
    /// `Err` when the request could not be queued (worker gone); otherwise
    /// the receiver for the worker's response.
    rx: Result<oneshot::Receiver<EmbedResult>, String>,
}

impl PendingEmbed {
    /// Await the embeddings for the enqueued texts (one vector per text, in
    /// request order).
    pub(crate) async fn wait(self) -> EmbedResult {
        match self.rx {
            Err(e) => Err(e),
            // A dropped sender means the worker died (e.g. panicked in the
            // embedding library) between accepting and answering.
            Ok(rx) => rx.await.unwrap_or_else(|_| Err(WORKER_GONE.to_string())),
        }
    }
}

impl EmbedBatcher {
    /// Spawn the worker thread owning `embed_fn` (the model forward pass:
    /// texts in, one embedding per text out, in order).
    pub(crate) fn spawn<F>(embed_fn: F) -> Self
    where
        F: FnMut(Vec<String>) -> EmbedResult + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel::<BatchRequest>();
        std::thread::Builder::new()
            .name("fastembed-embed".to_string())
            .spawn(move || worker_loop(&rx, embed_fn))
            .expect("failed to spawn fastembed embedding worker thread");
        Self { tx }
    }

    /// Queue `texts` for embedding.
    ///
    /// The request is enqueued synchronously — before the returned
    /// [`PendingEmbed`] is awaited — so requests reach the worker in call
    /// order and tests can stage the queue deterministically.
    pub(crate) fn enqueue(&self, texts: Vec<String>) -> PendingEmbed {
        let (respond, rx) = oneshot::channel();
        let rx = match self.tx.send(BatchRequest { texts, respond }) {
            Ok(()) => Ok(rx),
            Err(_) => Err(WORKER_GONE.to_string()),
        };
        PendingEmbed { rx }
    }
}

/// Worker loop: block for one request, drain whatever else queued while the
/// previous forward pass ran (up to [`MAX_BATCH_TEXTS`] texts), run ONE
/// forward pass over the concatenation, and split the results back per
/// request. Exits when every [`EmbedBatcher`] handle is dropped.
fn worker_loop<F>(rx: &std::sync::mpsc::Receiver<BatchRequest>, mut embed_fn: F)
where
    F: FnMut(Vec<String>) -> EmbedResult,
{
    while let Ok(first) = rx.recv() {
        let mut requests = vec![first];
        let mut total = requests[0].texts.len();
        // The first request is always taken whole (even above the cap —
        // requests are never split); the cap only bounds further coalescing.
        while total < MAX_BATCH_TEXTS {
            match rx.try_recv() {
                Ok(req) => {
                    total += req.texts.len();
                    requests.push(req);
                }
                Err(_) => break,
            }
        }

        let lengths: Vec<usize> = requests.iter().map(|r| r.texts.len()).collect();
        let batch: Vec<String> = requests
            .iter_mut()
            .flat_map(|r| r.texts.drain(..))
            .collect();

        match embed_fn(batch) {
            Ok(all) if all.len() == total => {
                let mut results = all.into_iter();
                for (req, n) in requests.into_iter().zip(lengths) {
                    let out: Vec<Vec<f32>> = results.by_ref().take(n).collect();
                    let _ = req.respond.send(Ok(out));
                }
            }
            Ok(all) => {
                let msg = format!(
                    "fastembed returned {} embeddings for {} texts",
                    all.len(),
                    total
                );
                for req in requests {
                    let _ = req.respond.send(Err(msg.clone()));
                }
            }
            Err(e) => {
                for req in requests {
                    let _ = req.respond.send(Err(e.clone()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{mpsc, Arc, Mutex},
        time::Duration,
    };

    use super::*;

    /// Generous CI-safe bound for operations that complete immediately when
    /// correct; a hit means a deadlock/lost-response regression, not slowness.
    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    async fn wait_with_timeout(pending: PendingEmbed) -> EmbedResult {
        tokio::time::timeout(TEST_TIMEOUT, pending.wait())
            .await
            .expect("PendingEmbed::wait timed out — worker deadlocked or lost the request")
    }

    /// Fake forward pass mapping each text to `[text.len() as f32]` so tests
    /// can verify per-request result routing by content.
    fn len_embed(texts: &[String]) -> EmbedResult {
        Ok(texts.iter().map(|t| vec![t.len() as f32]).collect())
    }

    /// PERF-05 evidence: requests that queue while a forward pass is running
    /// coalesce into ONE subsequent forward pass, and every caller gets its
    /// own results back. Deterministic: `enqueue` sends synchronously, and a
    /// gate blocks the worker inside pass 1 until requests B/C/D are queued —
    /// no timing asserts.
    #[tokio::test]
    async fn queued_requests_coalesce_into_one_forward_pass() {
        // Worker signals entry into each forward pass, then blocks on the gate.
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let calls: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));

        let batcher = EmbedBatcher::spawn({
            let calls = calls.clone();
            move |texts| {
                calls.lock().expect("calls mutex").push(texts.len());
                entered_tx.send(()).expect("test dropped entered_rx");
                gate_rx.recv().expect("test dropped gate_tx");
                len_embed(&texts)
            }
        });

        // Pass 1: request A occupies the worker.
        let pending_a = batcher.enqueue(vec!["a".to_string()]);
        entered_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("worker never started forward pass 1");

        // While pass 1 is blocked, queue B (2 texts), C, D (1 text each).
        let pending_b = batcher.enqueue(vec!["bb".to_string(), "bbb".to_string()]);
        let pending_c = batcher.enqueue(vec!["cccc".to_string()]);
        let pending_d = batcher.enqueue(vec!["ddddd".to_string()]);

        // Release pass 1; the worker then drains B+C+D into pass 2.
        gate_tx.send(()).expect("worker dropped gate_rx");
        entered_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("worker never started forward pass 2");
        gate_tx.send(()).expect("worker dropped gate_rx");

        assert_eq!(wait_with_timeout(pending_a).await, Ok(vec![vec![1.0]]));
        assert_eq!(
            wait_with_timeout(pending_b).await,
            Ok(vec![vec![2.0], vec![3.0]]),
            "B gets exactly its own two embeddings"
        );
        assert_eq!(wait_with_timeout(pending_c).await, Ok(vec![vec![4.0]]));
        assert_eq!(wait_with_timeout(pending_d).await, Ok(vec![vec![5.0]]));

        assert_eq!(
            *calls.lock().expect("calls mutex"),
            vec![1, 4],
            "three queued requests must coalesce into one 4-text forward pass"
        );
    }

    /// A single oversized request (> MAX_BATCH_TEXTS) is processed whole in
    /// one forward pass — requests are never split.
    #[tokio::test]
    async fn oversized_request_is_processed_whole() {
        let calls: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let batcher = EmbedBatcher::spawn({
            let calls = calls.clone();
            move |texts| {
                calls.lock().expect("calls mutex").push(texts.len());
                len_embed(&texts)
            }
        });

        let n = MAX_BATCH_TEXTS + 44;
        let texts: Vec<String> = (0..n).map(|_| "x".to_string()).collect();
        let out = wait_with_timeout(batcher.enqueue(texts))
            .await
            .expect("embed ok");
        assert_eq!(out.len(), n);
        assert_eq!(*calls.lock().expect("calls mutex"), vec![n]);
    }

    /// A failed forward pass reports the error to EVERY caller batched into it.
    #[tokio::test]
    async fn embed_error_propagates_to_every_batched_caller() {
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();

        let batcher = EmbedBatcher::spawn(move |texts| {
            entered_tx.send(()).expect("test dropped entered_rx");
            gate_rx.recv().expect("test dropped gate_tx");
            if texts.first().is_some_and(|t| t == "poison") {
                Err("onnx exploded".to_string())
            } else {
                len_embed(&texts)
            }
        });

        // Occupy the worker so the two poisoned requests batch together.
        let pending_ok = batcher.enqueue(vec!["fine".to_string()]);
        entered_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("worker never started forward pass 1");
        let pending_p1 = batcher.enqueue(vec!["poison".to_string()]);
        let pending_p2 = batcher.enqueue(vec!["also-batched".to_string()]);
        gate_tx.send(()).expect("worker dropped gate_rx");
        entered_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("worker never started forward pass 2");
        gate_tx.send(()).expect("worker dropped gate_rx");

        assert!(wait_with_timeout(pending_ok).await.is_ok());
        let e1 = wait_with_timeout(pending_p1)
            .await
            .expect_err("poisoned batch must error");
        let e2 = wait_with_timeout(pending_p2)
            .await
            .expect_err("poisoned batch must error");
        assert_eq!(e1, "onnx exploded");
        assert_eq!(
            e2, "onnx exploded",
            "error reaches every caller in the batch"
        );
    }

    /// A worker that dies (embed_fn panics) yields errors, not hangs: the
    /// in-flight caller gets WORKER_GONE, and later enqueues error either at
    /// send time (channel closed) or at wait time.
    #[tokio::test]
    async fn dead_worker_errors_instead_of_hanging() {
        let batcher = EmbedBatcher::spawn(|_texts| panic!("worker dies"));

        let err = wait_with_timeout(batcher.enqueue(vec!["x".to_string()]))
            .await
            .expect_err("request to a dying worker must error");
        assert_eq!(err, WORKER_GONE);

        // The thread is now dead; subsequent requests must also error.
        let err = wait_with_timeout(batcher.enqueue(vec!["y".to_string()]))
            .await
            .expect_err("request to a dead worker must error");
        assert_eq!(err, WORKER_GONE);
    }

    /// A mismatched forward pass (wrong embedding count) errors every caller
    /// instead of silently misrouting vectors across requests.
    #[tokio::test]
    async fn embedding_count_mismatch_errors_all_callers() {
        let batcher = EmbedBatcher::spawn(|_texts| Ok(vec![vec![1.0]; 999]));
        let err = wait_with_timeout(batcher.enqueue(vec!["x".to_string()]))
            .await
            .expect_err("mismatched embedding count must error");
        assert!(
            err.contains("999") && err.contains("1"),
            "error should state got-vs-expected counts: {err}"
        );
    }

    /// Dropping the batcher closes the channel and the worker exits — checked
    /// indirectly: the worker drains an already-queued request first (no lost
    /// work), then terminates without wedging the test binary.
    #[tokio::test]
    async fn drop_after_enqueue_still_answers_queued_request() {
        let batcher = EmbedBatcher::spawn(|texts| len_embed(&texts));
        let pending = batcher.enqueue(vec!["hello".to_string()]);
        drop(batcher);
        assert_eq!(wait_with_timeout(pending).await, Ok(vec![vec![5.0]]));
    }
}
