// Minimal TinyGo WAFER guest for the cold/warm instantiation benchmarks in
// crates/wafer-run/benches/wasm_guest.rs (PERF-01 measurement prerequisite).
//
// Implements the raw `__wafer_*` guest ABI directly (no SDK): `__wafer_handle`
// ignores its input and returns a static `{"action":"Respond"}` payload. The
// point of this fixture is NOT guest work — it is that a TinyGo wasi module
// exports `_start`, which the wasmi loader runs on every instantiation (i.e.
// on every handle call today) to initialise the Go runtime. The benchmarks
// compare that per-call cost against the Rust guest, which has no `_start`.
//
// Built by scripts/build-fixtures.sh when `tinygo` is on PATH:
//
//	tinygo build -target wasip1 -o target/tinygo_guest.wasm .
package main

import "unsafe"

// Static payloads. Package-level vars stay live for the module lifetime, so
// the pointers handed to the host remain valid after the exported calls
// return (the host copies them out before the next guest call).
var (
	infoJSON = []byte(`{"name":"bench/tinygo-guest","version":"0.0.0","interface":"handler@v1","summary":"TinyGo minimal bench guest"}`)
	respJSON = []byte(`{"action":"Respond","response":{"data":[]}}`)
	okJSON   = []byte(`{"Ok":null}`)
	allocBuf []byte
)

func main() {}

// pack returns `(ptr << 32) | len` for a live byte slice — the packed i64
// format `WasmiBlock` expects from every guest export.
func pack(b []byte) int64 {
	ptr := uintptr(unsafe.Pointer(&b[0]))
	return int64(ptr)<<32 | int64(len(b))
}

//export __wafer_alloc
func waferAlloc(size int32) int32 {
	if size <= 0 {
		return 0
	}
	allocBuf = make([]byte, size)
	return int32(uintptr(unsafe.Pointer(&allocBuf[0])))
}

//export __wafer_info
func waferInfo() int64 {
	return pack(infoJSON)
}

//export __wafer_handle
func waferHandle(_ int32, _ int32) int64 {
	return pack(respJSON)
}

//export __wafer_lifecycle
func waferLifecycle(_ int32, _ int32) int64 {
	return pack(okJSON)
}
