//! Throwaway WASM guest for the streaming-ABI spike (see
//! `wafer-run/tests/streaming_spike.rs`).
//!
//! Calls the host's `wafer_spike::next_chunk` import in a tight loop,
//! verifies the recognisable byte pattern (`b[i] == i % 256`), and
//! accumulates the total bytes received. The guest pre-allocates one
//! reusable buffer per call to `read_all_chunks` so per-iteration cost
//! is dominated by the host trip itself.
#![no_main]

#[link(wasm_import_module = "wafer_spike")]
extern "C" {
    fn next_chunk(buf_ptr: i32, chunk_size: i32) -> i32;
    fn set_target(n: i32);
}

/// Configure the host's per-thread chunk-target counter.
#[no_mangle]
pub extern "C" fn spike_set_target(n: i32) {
    unsafe { set_target(n) }
}

/// Read chunks of size `chunk_size` from the host until end-of-stream.
/// Verifies each chunk matches the pattern `b[i] == i % 256` and returns
/// the total bytes received.
#[no_mangle]
pub extern "C" fn read_all_chunks(chunk_size: i32) -> i64 {
    let n = chunk_size.max(0) as usize;
    let mut buf: Vec<u8> = Vec::with_capacity(n);
    // SAFETY: capacity == n, all bytes will be overwritten by the host
    // before we read them.
    unsafe {
        buf.set_len(n);
    }
    let buf_ptr = buf.as_mut_ptr() as i32;

    let mut total: i64 = 0;
    loop {
        let got = unsafe { next_chunk(buf_ptr, chunk_size) };
        if got == 0 {
            return total;
        }
        // Verify pattern. Any mismatch is a spike correctness failure —
        // panic so the test surfaces it.
        for i in 0..n {
            assert_eq!(buf[i], (i % 256) as u8, "chunk pattern mismatch at {i}");
        }
        total += chunk_size as i64;
    }
}
