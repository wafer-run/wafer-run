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
// Each returns the FFI's status: WAFER_ACCEPTED, or
// WAFER_REFUSED_NULL_CALLBACK when nothing will call back.
static inline int cgo_wafer_resolve(WaferRuntime* w, wafer_done_cb cb, uintptr_t ud) {
    return wafer_resolve(w, cb, (void*)ud);
}
static inline int cgo_wafer_start(WaferRuntime* w, wafer_done_cb cb, uintptr_t ud) {
    return wafer_start(w, cb, (void*)ud);
}
static inline int cgo_wafer_stop(WaferRuntime* w, wafer_done_cb cb, uintptr_t ud) {
    return wafer_stop(w, cb, (void*)ud);
}
static inline int cgo_wafer_run(WaferRuntime* w,
                                 const char* flow_id,
                                 const char* message_json,
                                 wafer_done_cb cb,
                                 uintptr_t ud) {
    return wafer_run(w, flow_id, message_json, cb, (void*)ud);
}
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime/cgo"
	"sync"
	"unsafe"
)

// Wafer is the Go host runtime backed by the Rust wafer-run core. Its
// methods are safe for concurrent use, including Close.
type Wafer struct {
	// mu guards ptr: every FFI call holds it for reading while it uses ptr,
	// and Close takes it for writing to retire ptr, so Close never frees the
	// runtime under a call that is still using it.
	mu  sync.RWMutex
	ptr *C.WaferRuntime
}

// ErrClosed is returned by a call made on a Wafer after Close.
var ErrClosed = errors.New("wafer: runtime is closed")

// testHookFFICall, when set, runs while a call holds the runtime, just
// before it calls into the FFI.
var testHookFFICall func()

// testHookDoneCallback, when set, runs inside waferDoneCallback, on the
// FFI's thread, before the result is delivered.
var testHookDoneCallback func()

// with runs f on the runtime pointer, holding it so Close waits for f; it
// returns ErrClosed after Close.
func (w *Wafer) with(f func(p *C.WaferRuntime)) error {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.ptr == nil {
		return ErrClosed
	}
	if testHookFFICall != nil {
		testHookFFICall()
	}
	f(w.ptr)
	return nil
}

// New creates a new WAFER runtime instance.
func New() *Wafer {
	ptr := C.wafer_new()
	if ptr == nil {
		panic("wafer: failed to allocate runtime")
	}
	return &Wafer{ptr: ptr}
}

// Close stops the runtime (see Stop) and frees it. It waits for calls that
// are using the runtime and for accepted Runs, which complete; calls made
// after Close return ErrClosed (Run an error Result naming it). Closing a
// closed Wafer does nothing and returns nil. The error is Stop's.
func (w *Wafer) Close() error {
	w.mu.Lock()
	p := w.ptr
	w.ptr = nil
	w.mu.Unlock()
	if p == nil {
		return nil
	}
	ch, h, err := startAsync(p, C.wafer_done_cb(C.waferDoneCallback), stopCall)
	if err == nil {
		r := <-ch
		h.Delete()
		err = parseLifecycleResult(r.body, nil)
	}
	C.wafer_free(p)
	return err
}

// Register registers a block or flow definition from a file path.
// If path ends with .wasm, registers a WASM block with the given name, which
// must be the name the block reports in its BlockInfo (a mismatch is refused).
// Such a block runs with no capabilities, whatever it declares; RegisterBlock
// grants it some. Otherwise, reads the file as a JSON flow definition.
//
// This is a synchronous operation in the FFI layer.
func (w *Wafer) Register(name, path string) error {
	cName := C.CString(name)
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cName))
	defer C.free(unsafe.Pointer(cPath))

	var cResult *C.char
	if err := w.with(func(p *C.WaferRuntime) { cResult = C.wafer_register(p, cName, cPath) }); err != nil {
		return err
	}
	return parseFFIError(cResult)
}

// RegisterBlock registers the WASM block at path under name (the name the
// block reports in its BlockInfo), bounded by capabilitiesJSON: a JSON
// BlockCapabilities object such as
//
//	{"collections": {"Only": ["acme__widget__items"]}, "crypto": true}
//
// An allowlist field is "None", "Any" or {"Only": [...]}; a flag is a bool.
// The block runs under that bound intersected with what it declares. A field
// the object omits denies, so {} grants nothing. Invalid JSON is an error.
//
// This is a synchronous operation in the FFI layer.
func (w *Wafer) RegisterBlock(name, path, capabilitiesJSON string) error {
	cName := C.CString(name)
	cPath := C.CString(path)
	cCaps := C.CString(capabilitiesJSON)
	defer C.free(unsafe.Pointer(cName))
	defer C.free(unsafe.Pointer(cPath))
	defer C.free(unsafe.Pointer(cCaps))

	var cResult *C.char
	if err := w.with(func(p *C.WaferRuntime) { cResult = C.wafer_register_block(p, cName, cPath, cCaps) }); err != nil {
		return err
	}
	return parseFFIError(cResult)
}

// Resolve walks all flow trees and resolves block references.
//
// Async in the FFI layer; this wrapper blocks the calling goroutine until the
// FFI callback fires.
func (w *Wafer) Resolve() error {
	return w.resolveWith(C.wafer_done_cb(C.waferDoneCallback))
}

// resolveWith is Resolve with the completion callback it hands the FFI.
func (w *Wafer) resolveWith(done C.wafer_done_cb) error {
	return w.lifecycle(done, func(p *C.WaferRuntime, cb C.wafer_done_cb, ud C.uintptr_t) C.int {
		return C.cgo_wafer_resolve(p, cb, ud)
	})
}

// Start initializes the runtime. It seals the runtime unless Resolve already
// did, and after a failed Resolve it reports that failure again.
//
// Async in the FFI layer; this wrapper blocks the calling goroutine until the
// FFI callback fires.
func (w *Wafer) Start() error {
	return w.lifecycle(C.wafer_done_cb(C.waferDoneCallback), func(p *C.WaferRuntime, cb C.wafer_done_cb, ud C.uintptr_t) C.int {
		return C.cgo_wafer_start(p, cb, ud)
	})
}

// Stop refuses Runs from now on, waits for the Runs already accepted, then
// runs the blocks' lifecycle(Stop) handlers. It returns an error if shutdown
// panicked. A second Stop waits for the first and does not stop the blocks
// again.
//
// Async in the FFI layer; this wrapper blocks the calling goroutine until the
// FFI callback fires.
func (w *Wafer) Stop() error {
	return w.lifecycle(C.wafer_done_cb(C.waferDoneCallback), stopCall)
}

func stopCall(p *C.WaferRuntime, cb C.wafer_done_cb, ud C.uintptr_t) C.int {
	return C.cgo_wafer_stop(p, cb, ud)
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

	resultStr, err := w.runAsync(C.wafer_done_cb(C.waferDoneCallback), func(p *C.WaferRuntime, cb C.wafer_done_cb, ud C.uintptr_t) C.int {
		return C.cgo_wafer_run(p, cFlowID, cMsg, cb, ud)
	})
	if err != nil {
		return ErrorResult("Internal", err.Error())
	}

	var result Result
	if err := json.Unmarshal([]byte(resultStr), &result); err != nil {
		return ErrorResult("unmarshal_error", fmt.Sprintf("failed to unmarshal result: %v", err))
	}
	return &result
}

// FlowsInfo returns info about all registered flows. It returns an error
// when the FFI reports one instead of the list.
//
// Synchronous in the FFI layer (read-only introspection).
func (w *Wafer) FlowsInfo() ([]FlowInfo, error) {
	var cResult *C.char
	if err := w.with(func(p *C.WaferRuntime) { cResult = C.wafer_flows_info(p) }); err != nil {
		return nil, err
	}
	defer C.wafer_free_string(cResult)

	resultStr := C.GoString(cResult)

	var info []FlowInfo
	if err := json.Unmarshal([]byte(resultStr), &info); err != nil {
		var ffiErr ffiError
		if json.Unmarshal([]byte(resultStr), &ffiErr) == nil && ffiErr.Error != "" {
			return nil, fmt.Errorf("wafer: flows info: %s", ffiErr.Error)
		}
		return nil, fmt.Errorf("wafer: flows info is not a JSON array: %q", resultStr)
	}
	return info, nil
}

// HasBlock reports whether a block type is registered. It returns an error
// when the FFI call failed rather than answering.
//
// Synchronous in the FFI layer.
func (w *Wafer) HasBlock(typeName string) (bool, error) {
	cTypeName := C.CString(typeName)
	defer C.free(unsafe.Pointer(cTypeName))
	var status C.int
	if err := w.with(func(p *C.WaferRuntime) { status = C.wafer_has_block(p, cTypeName) }); err != nil {
		return false, err
	}
	switch status {
	case 1:
		return true, nil
	case 0:
		return false, nil
	default:
		return false, fmt.Errorf("wafer: has block %q: the FFI call failed (status %d)", typeName, int(status))
	}
}

// --- Async callback plumbing ---------------------------------------------

// asyncResult is the value pushed onto the channel by waferDoneCallback. It
// holds either an error JSON string or "" for the NULL-result success case.
type asyncResult struct {
	// Empty when the FFI callback received a NULL result (success for
	// lifecycle ops). Otherwise the JSON string returned by Rust.
	body string
}

// asyncCall is an async FFI function: it takes the runtime pointer, the
// callback fn ptr and a `uintptr_t` carrying a cgo.Handle that resolves back
// to the result channel inside waferDoneCallback (a `uintptr_t` rather than
// a `void*` avoids unsafe.Pointer conversion at the cgo boundary), and
// returns the FFI's status.
type asyncCall func(*C.WaferRuntime, C.wafer_done_cb, C.uintptr_t) C.int

// startAsync makes one async FFI call on p. Anything but WAFER_ACCEPTED
// means no callback will fire, so it returns an error instead of a channel
// to wait on. An accepted call always calls back — wafer_free cancels one
// still pending with an error — so a wait on the channel ends. The caller
// deletes the handle once the result is in.
func startAsync(p *C.WaferRuntime, cb C.wafer_done_cb, invoke asyncCall) (chan asyncResult, cgo.Handle, error) {
	ch := make(chan asyncResult, 1)
	h := cgo.NewHandle(ch)
	if status := invoke(p, cb, C.uintptr_t(h)); status != C.WAFER_ACCEPTED {
		h.Delete()
		return nil, 0, fmt.Errorf("wafer: the FFI refused the call (status %d)", int(status))
	}
	return ch, h, nil
}

// runAsync makes an async FFI call holding the runtime (see with), then
// waits — no longer holding it — for the callback, and returns its JSON
// result string. For lifecycle ops the string is empty on success.
func (w *Wafer) runAsync(cb C.wafer_done_cb, invoke asyncCall) (string, error) {
	var ch chan asyncResult
	var h cgo.Handle
	var callErr error
	if err := w.with(func(p *C.WaferRuntime) { ch, h, callErr = startAsync(p, cb, invoke) }); err != nil {
		return "", err
	}
	if callErr != nil {
		return "", callErr
	}
	defer h.Delete()
	r := <-ch
	return r.body, nil
}

// lifecycle runs a lifecycle op (resolve, start, stop), whose callback
// result is either NULL (success) or a JSON error string.
func (w *Wafer) lifecycle(cb C.wafer_done_cb, invoke asyncCall) error {
	return parseLifecycleResult(w.runAsync(cb, invoke))
}

// parseLifecycleResult turns a lifecycle op's callback result into an error:
// "" (a NULL result) is success, anything else a JSON error string.
func parseLifecycleResult(body string, err error) error {
	if err != nil {
		return err
	}
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
	if testHookDoneCallback != nil {
		testHookDoneCallback()
	}
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
// waferDoneCallback + parseLifecycleResult instead.
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
