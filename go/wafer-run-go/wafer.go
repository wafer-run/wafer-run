// Package wafer provides Go bindings for the WAFER runtime via CGO.
//
// The Go package links against libwafer_ffi.so (or .dylib/.dll).
// Users must have the shared library installed or set LD_LIBRARY_PATH.
package wafer

/*
#cgo LDFLAGS: -lwafer_ffi
#include "wafer.h"
#include <stdlib.h>
#include <stdint.h>

// Forward declaration so we can take the function pointer of the Go-exported
// callback below and pass it through C as a wafer_done_cb. The Go-exported
// signature is `char*` (cgo strips const); cast at the call site to satisfy
// the wafer_done_cb typedef.
extern void waferDoneCallback(char* result, void* user_data);

// Thin wrappers that convert a uintptr_t user_data to void* before calling
// into wafer-ffi. This lets us pass a cgo.Handle (which is uintptr) through
// the cgo boundary without unsafe.Pointer(uintptr(...)) — go vet's pattern
// matcher only accepts that conversion in narrow contexts that don't apply
// to cgo.Handle.
static inline void cgo_wafer_resolve(WaferRuntime* w, wafer_done_cb cb, uintptr_t ud) {
    wafer_resolve(w, cb, (void*)ud);
}
static inline void cgo_wafer_start(WaferRuntime* w, wafer_done_cb cb, uintptr_t ud) {
    wafer_start(w, cb, (void*)ud);
}
static inline void cgo_wafer_stop(WaferRuntime* w, wafer_done_cb cb, uintptr_t ud) {
    wafer_stop(w, cb, (void*)ud);
}
static inline void cgo_wafer_run(WaferRuntime* w,
                                  const char* flow_id,
                                  const char* message_json,
                                  wafer_done_cb cb,
                                  uintptr_t ud) {
    wafer_run(w, flow_id, message_json, cb, (void*)ud);
}
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime/cgo"
	"unsafe"
)

// Wafer is the Go host runtime backed by the Rust wafer-run core.
type Wafer struct {
	ptr *C.WaferRuntime
}

// New creates a new WAFER runtime instance.
func New() *Wafer {
	ptr := C.wafer_new()
	if ptr == nil {
		panic("wafer: failed to allocate runtime")
	}
	return &Wafer{ptr: ptr}
}

// Close frees the underlying runtime. The Wafer must not be used after Close.
// Call Stop first so that block lifecycle(Stop) handlers can run; otherwise
// they are skipped.
func (w *Wafer) Close() {
	if w.ptr != nil {
		C.wafer_free(w.ptr)
		w.ptr = nil
	}
}

// Register registers a block or flow definition from a file path.
// If path ends with .wasm, registers a WASM block with the given name.
// Otherwise, reads the file as a JSON flow definition.
//
// This is a synchronous operation in the FFI layer.
func (w *Wafer) Register(name, path string) error {
	cName := C.CString(name)
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cName))
	defer C.free(unsafe.Pointer(cPath))

	cResult := C.wafer_register(w.ptr, cName, cPath)
	return parseFFIError(cResult)
}

// Resolve walks all flow trees and resolves block references.
//
// Async in the FFI layer; this wrapper blocks the calling goroutine until the
// FFI callback fires.
func (w *Wafer) Resolve() error {
	return parseFFIErrorAsync(func(cb C.wafer_done_cb, ud C.uintptr_t) {
		C.cgo_wafer_resolve(w.ptr, cb, ud)
	})
}

// Start initializes the runtime. Calls Resolve() if not already resolved.
//
// Async in the FFI layer; this wrapper blocks the calling goroutine until the
// FFI callback fires.
func (w *Wafer) Start() error {
	return parseFFIErrorAsync(func(cb C.wafer_done_cb, ud C.uintptr_t) {
		C.cgo_wafer_start(w.ptr, cb, ud)
	})
}

// Stop shuts down all resolved block instances.
//
// Async in the FFI layer; this wrapper blocks the calling goroutine until the
// FFI callback fires.
func (w *Wafer) Stop() {
	_ = parseFFIErrorAsync(func(cb C.wafer_done_cb, ud C.uintptr_t) {
		C.cgo_wafer_stop(w.ptr, cb, ud)
	})
}

// Run runs a flow by ID with the given message.
//
// Async in the FFI layer; this wrapper blocks the calling goroutine until the
// FFI callback fires.
func (w *Wafer) Run(flowID string, msg *Message) *Result {
	msgJSON, err := json.Marshal(msg)
	if err != nil {
		return ErrorResult("marshal_error", fmt.Sprintf("failed to marshal message: %v", err))
	}

	cFlowID := C.CString(flowID)
	cMsg := C.CString(string(msgJSON))
	defer C.free(unsafe.Pointer(cFlowID))
	defer C.free(unsafe.Pointer(cMsg))

	resultStr := runAsync(func(cb C.wafer_done_cb, ud C.uintptr_t) {
		C.cgo_wafer_run(w.ptr, cFlowID, cMsg, cb, ud)
	})

	var result Result
	if err := json.Unmarshal([]byte(resultStr), &result); err != nil {
		return ErrorResult("unmarshal_error", fmt.Sprintf("failed to unmarshal result: %v", err))
	}
	return &result
}

// FlowsInfo returns info about all registered flows.
//
// Synchronous in the FFI layer (read-only introspection).
func (w *Wafer) FlowsInfo() []FlowInfo {
	cResult := C.wafer_flows_info(w.ptr)
	defer C.wafer_free_string(cResult)

	resultStr := C.GoString(cResult)

	var info []FlowInfo
	if err := json.Unmarshal([]byte(resultStr), &info); err != nil {
		return nil
	}
	return info
}

// HasBlock returns true if a block type is registered.
//
// Synchronous in the FFI layer.
func (w *Wafer) HasBlock(typeName string) bool {
	cTypeName := C.CString(typeName)
	defer C.free(unsafe.Pointer(cTypeName))
	return C.wafer_has_block(w.ptr, cTypeName) != 0
}

// --- Async callback plumbing ---------------------------------------------

// asyncResult is the value pushed onto the channel by waferDoneCallback. It
// holds either an error JSON string or "" for the NULL-result success case.
type asyncResult struct {
	// Empty when the FFI callback received a NULL result (success for
	// lifecycle ops). Otherwise the JSON string returned by Rust.
	body string
}

// runAsync invokes an async FFI function via the supplied closure, blocks
// until waferDoneCallback fires, and returns the JSON result string. For
// lifecycle ops the returned string is empty on success.
//
// The closure receives the callback fn ptr and a `uintptr_t` carrying a
// cgo.Handle that resolves back to the result channel inside
// waferDoneCallback. Using `uintptr_t` (rather than `void*`) avoids
// unsafe.Pointer conversion at the cgo boundary.
func runAsync(invoke func(C.wafer_done_cb, C.uintptr_t)) string {
	ch := make(chan asyncResult, 1)
	h := cgo.NewHandle(ch)
	defer h.Delete()

	invoke(C.wafer_done_cb(C.waferDoneCallback), C.uintptr_t(h))

	r := <-ch
	return r.body
}

// parseFFIErrorAsync wraps runAsync for the lifecycle ops whose callback
// result is either NULL (success) or a JSON error string.
func parseFFIErrorAsync(invoke func(C.wafer_done_cb, C.uintptr_t)) error {
	body := runAsync(invoke)
	if body == "" {
		return nil
	}
	var ffiErr ffiError
	if err := json.Unmarshal([]byte(body), &ffiErr); err != nil {
		return errors.New(body)
	}
	return errors.New(ffiErr.Error)
}

//export waferDoneCallback
func waferDoneCallback(result *C.char, userData unsafe.Pointer) {
	// The C signature of wafer_done_cb has `void* user_data`; cgo emits the
	// Go-exported function with the same signature. We round-trip via
	// uintptr to retrieve the cgo.Handle we passed in (uintptr_t-typed)
	// through the static C wrappers above.
	h := cgo.Handle(uintptr(userData)) //nolint:govet
	ch := h.Value().(chan asyncResult)
	if result == nil {
		ch <- asyncResult{}
	} else {
		// Copy the string before returning — Rust frees `result` once this
		// callback returns.
		ch <- asyncResult{body: C.GoString(result)}
	}
}

// --- Synchronous error helpers ------------------------------------------

// parseFFIError converts a C result pointer into a Go error. NULL means
// success (returns nil). Non-NULL is a JSON error string that must be freed.
//
// Only for synchronous FFI ops (e.g. wafer_register). Async ops route through
// waferDoneCallback + parseFFIErrorAsync instead.
func parseFFIError(cResult *C.char) error {
	if cResult == nil {
		return nil
	}
	defer C.wafer_free_string(cResult)

	resultStr := C.GoString(cResult)

	var ffiErr ffiError
	if err := json.Unmarshal([]byte(resultStr), &ffiErr); err != nil {
		return errors.New(resultStr)
	}
	return errors.New(ffiErr.Error)
}
