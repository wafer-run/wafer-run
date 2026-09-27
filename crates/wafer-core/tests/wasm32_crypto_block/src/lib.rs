//! Runtime fixture: the crypto block awaits its `CryptoService` on wasm32.
//! See this crate's `Cargo.toml`; `run.mjs` drives it.
//!
//! Each export drives one call through the real `CryptoBlock` by hand: the
//! first poll must leave the call pending (the service has not answered),
//! releasing the service must wake the call, and the next poll must carry
//! the service's answer — its result, or for a service that could not reach
//! its backend, `ErrorCode::Unavailable`. Returns 0 when all of that holds,
//! otherwise the first step that failed (see `run.mjs` for the codes).

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "this fixture only exercises the crypto block on wasm32; \
     build it with --target wasm32-unknown-unknown (scripts/check.sh wasm does)"
);

use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::Future,
    pin::pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Poll, Wake, Waker},
    time::Duration,
};

use futures::channel::oneshot;
use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceType},
    wire::crypto as wire,
    Block, ErrorCode, Message, WaferError,
};
use wafer_core::{
    interfaces::crypto::service::{CryptoError, CryptoService},
    service_blocks::crypto::CryptoBlock,
};

/// A service whose password ops resolve only when the caller releases them,
/// with the answer the caller sends — on wasm32 there is no other thread, so
/// this is what a JS promise (a Durable Object stub fetch) looks like to the
/// block.
struct GatedCrypto {
    hash: RefCell<Option<oneshot::Receiver<Result<String, CryptoError>>>>,
    compare: RefCell<Option<oneshot::Receiver<Result<(), CryptoError>>>>,
}

#[wafer_core::wafer_async_trait]
impl CryptoService for GatedCrypto {
    async fn hash(&self, _password: &str) -> Result<String, CryptoError> {
        let gate = self.hash.borrow_mut().take().expect("one hash per gate");
        gate.await
            .unwrap_or_else(|_| Err(CryptoError::HashError("gate dropped".into())))
    }

    async fn compare_hash(&self, _password: &str, _hash: &str) -> Result<(), CryptoError> {
        let gate = self
            .compare
            .borrow_mut()
            .take()
            .expect("one compare per gate");
        gate.await
            .unwrap_or_else(|_| Err(CryptoError::Other("gate dropped".into())))
    }

    async fn sign_for(
        &self,
        _block_id: &str,
        _claims: BTreeMap<String, serde_json::Value>,
        _expiry: Duration,
    ) -> Result<String, CryptoError> {
        unreachable!("only the password ops are exercised")
    }

    async fn verify_for(
        &self,
        _block_id: &str,
        _token: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
        unreachable!("only the password ops are exercised")
    }

    async fn random_bytes(&self, _n: usize) -> Result<Vec<u8>, CryptoError> {
        unreachable!("only the password ops are exercised")
    }
}

/// Grants every resource check; the crypto handler consults nothing else.
struct AllowCtx;

#[wafer_core::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(&self, _: &str, _: Message, _: InputStream) -> OutputStream {
        unimplemented!("the crypto block calls no block")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(AllowCtx)
    }
    fn check_resource_access(
        &self,
        _: &str,
        _: ResourceType,
        _: ResourceAccess,
    ) -> Result<(), WaferError> {
        Ok(())
    }
    fn resource_access_admitted(&self, _: &str, _: ResourceType, _: ResourceAccess) -> bool {
        true
    }
}

/// Records whether the waker handed to a poll was woken.
#[derive(Default)]
struct Woken(AtomicBool);

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The call stayed pending until the service answered, and its answer is
/// `Ok(terminal)`; `Err(code)` names the first step that did not hold.
fn drive_gated_call(
    block: &CryptoBlock,
    kind: &str,
    request: Vec<u8>,
    release: impl FnOnce(),
) -> Result<Result<Vec<u8>, TerminalNotResponse>, u32> {
    let woken = Arc::new(Woken::default());
    let waker = Waker::from(Arc::clone(&woken));
    let mut cx = std::task::Context::from_waker(&waker);

    let mut call = pin!(block.handle(
        &AllowCtx,
        Message::new(kind),
        InputStream::from_bytes(request)
    ));
    if call.as_mut().poll(&mut cx).is_ready() {
        return Err(1); // answered before the service did
    }
    release();
    if !woken.0.swap(false, Ordering::SeqCst) {
        return Err(2); // the service's answer did not wake the call
    }
    let Poll::Ready(out) = call.as_mut().poll(&mut cx) else {
        return Err(3); // woken, but still pending
    };
    let mut collect = pin!(out.collect_buffered());
    match collect.as_mut().poll(&mut cx) {
        Poll::Ready(terminal) => Ok(terminal.map(|resp| resp.body)),
        Poll::Pending => Err(5), // a buffered answer did not complete
    }
}

/// The body of a call that should have succeeded; `Err(4)` if it did not.
fn answered(call: Result<Result<Vec<u8>, TerminalNotResponse>, u32>) -> Result<Vec<u8>, u32> {
    match call? {
        Ok(body) => Ok(body),
        Err(_) => Err(4), // the block answered with an error
    }
}

/// 0 when a call whose service failed with `CryptoError::Unavailable`
/// answered `ErrorCode::Unavailable`.
fn answered_unavailable(call: Result<Result<Vec<u8>, TerminalNotResponse>, u32>) -> u32 {
    match call {
        Err(code) => code,
        Ok(Ok(_)) => 7, // the block answered, but the service failed
        Ok(Err(TerminalNotResponse::Error(e))) if e.code == ErrorCode::Unavailable => 0,
        Ok(Err(_)) => 8, // the block's error is not `Unavailable`
    }
}

fn unreachable_hasher() -> CryptoError {
    CryptoError::Unavailable("hasher durable object: fetch timed out".into())
}

/// `crypto.hash` through the block, with a hash that completes only after an
/// await point.
#[no_mangle]
pub extern "C" fn hash_awaits_the_service() -> u32 {
    let (release, gate) = oneshot::channel();
    let block = CryptoBlock::new(Arc::new(GatedCrypto {
        hash: RefCell::new(Some(gate)),
        compare: RefCell::new(None),
    }));
    let request = codec::encode(&wire::HashRequest {
        password: "correct horse".into(),
    })
    .expect("encode");
    let body = match answered(drive_gated_call(
        &block,
        ServiceOp::CRYPTO_HASH,
        request,
        || {
            release.send(Ok("$gated$hash".to_string())).expect("send");
        },
    )) {
        Ok(body) => body,
        Err(code) => return code,
    };
    match codec::decode::<wire::HashResponse>(&body) {
        Ok(resp) if resp.hash == "$gated$hash" => 0,
        _ => 6, // not the service's answer
    }
}

/// `crypto.compare_hash` through the block, with a check that completes only
/// after an await point: `verdict` is what the service answers.
#[no_mangle]
pub extern "C" fn compare_hash_awaits_the_service(verdict: u32) -> u32 {
    let (release, gate) = oneshot::channel();
    let block = CryptoBlock::new(Arc::new(GatedCrypto {
        hash: RefCell::new(None),
        compare: RefCell::new(Some(gate)),
    }));
    let request = codec::encode(&wire::CompareHashRequest {
        password: "correct horse".into(),
        hash: "$gated$hash".into(),
    })
    .expect("encode");
    let body = match answered(drive_gated_call(
        &block,
        ServiceOp::CRYPTO_COMPARE_HASH,
        request,
        || {
            let answer = if verdict != 0 {
                Ok(())
            } else {
                Err(CryptoError::PasswordMismatch)
            };
            release.send(answer).expect("send");
        },
    )) {
        Ok(body) => body,
        Err(code) => return code,
    };
    match codec::decode::<wire::CompareHashResponse>(&body) {
        Ok(resp) if resp.matches == (verdict != 0) => 0,
        _ => 6, // not the service's answer
    }
}

/// `crypto.hash` through the block, with a service that fails after an await
/// point because its backend cannot be reached.
#[no_mangle]
pub extern "C" fn unreachable_hash_answers_unavailable() -> u32 {
    let (release, gate) = oneshot::channel();
    let block = CryptoBlock::new(Arc::new(GatedCrypto {
        hash: RefCell::new(Some(gate)),
        compare: RefCell::new(None),
    }));
    let request = codec::encode(&wire::HashRequest {
        password: "correct horse".into(),
    })
    .expect("encode");
    answered_unavailable(drive_gated_call(
        &block,
        ServiceOp::CRYPTO_HASH,
        request,
        || {
            release.send(Err(unreachable_hasher())).expect("send");
        },
    ))
}

/// `crypto.compare_hash` through the block, with a service that fails after
/// an await point because its backend cannot be reached.
#[no_mangle]
pub extern "C" fn unreachable_compare_hash_answers_unavailable() -> u32 {
    let (release, gate) = oneshot::channel();
    let block = CryptoBlock::new(Arc::new(GatedCrypto {
        hash: RefCell::new(None),
        compare: RefCell::new(Some(gate)),
    }));
    let request = codec::encode(&wire::CompareHashRequest {
        password: "correct horse".into(),
        hash: "$gated$hash".into(),
    })
    .expect("encode");
    answered_unavailable(drive_gated_call(
        &block,
        ServiceOp::CRYPTO_COMPARE_HASH,
        request,
        || {
            release.send(Err(unreachable_hasher())).expect("send");
        },
    ))
}
