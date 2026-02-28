// Package wafer provides Go bindings for the WAFER runtime via CGO.
//
// The Go package links against libwafer_ffi.so (or .dylib/.dll).
// Users must have the shared library installed or set LD_LIBRARY_PATH.
package wafer

/*
#cgo LDFLAGS: -lwafer_ffi
#include "wafer.h"
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
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
func (w *Wafer) Close() {
	if w.ptr != nil {
		C.wafer_free(w.ptr)
		w.ptr = nil
	}
}

// RegisterWASMBlock registers a WASM block from a file path.
func (w *Wafer) RegisterWASMBlock(typeName, wasmPath string) error {
	cTypeName := C.CString(typeName)
	cWasmPath := C.CString(wasmPath)
	defer C.free(unsafe.Pointer(cTypeName))
	defer C.free(unsafe.Pointer(cWasmPath))

	cResult := C.wafer_register_wasm_block(w.ptr, cTypeName, cWasmPath)
	return parseFFIError(cResult)
}

// AddChain adds a chain definition to the runtime.
func (w *Wafer) AddChain(def ChainDef) error {
	data, err := json.Marshal(def)
	if err != nil {
		return fmt.Errorf("wafer: marshal ChainDef: %w", err)
	}

	cJSON := C.CString(string(data))
	defer C.free(unsafe.Pointer(cJSON))

	cResult := C.wafer_add_chain_def(w.ptr, cJSON)
	return parseFFIError(cResult)
}

// Resolve walks all chain trees and resolves block references.
func (w *Wafer) Resolve() error {
	cResult := C.wafer_resolve(w.ptr)
	return parseFFIError(cResult)
}

// Start initializes the runtime. Calls Resolve() if not already resolved.
func (w *Wafer) Start() error {
	cResult := C.wafer_start(w.ptr)
	return parseFFIError(cResult)
}

// Stop shuts down all resolved block instances.
func (w *Wafer) Stop() {
	C.wafer_stop(w.ptr)
}

// Execute runs a chain by ID with the given message.
func (w *Wafer) Execute(chainID string, msg *Message) *Result {
	msgJSON, err := json.Marshal(msg)
	if err != nil {
		return ErrorResult("marshal_error", fmt.Sprintf("failed to marshal message: %v", err))
	}

	cChainID := C.CString(chainID)
	cMsg := C.CString(string(msgJSON))
	defer C.free(unsafe.Pointer(cChainID))
	defer C.free(unsafe.Pointer(cMsg))

	cResult := C.wafer_execute(w.ptr, cChainID, cMsg)
	defer C.wafer_free_string(cResult)

	resultStr := C.GoString(cResult)

	var result Result
	if err := json.Unmarshal([]byte(resultStr), &result); err != nil {
		return ErrorResult("unmarshal_error", fmt.Sprintf("failed to unmarshal result: %v", err))
	}
	return &result
}

// ChainsInfo returns info about all registered chains.
func (w *Wafer) ChainsInfo() []ChainInfo {
	cResult := C.wafer_chains_info(w.ptr)
	defer C.wafer_free_string(cResult)

	resultStr := C.GoString(cResult)

	var info []ChainInfo
	if err := json.Unmarshal([]byte(resultStr), &info); err != nil {
		return nil
	}
	return info
}

// HasBlock returns true if a block type is registered.
func (w *Wafer) HasBlock(typeName string) bool {
	cTypeName := C.CString(typeName)
	defer C.free(unsafe.Pointer(cTypeName))
	return C.wafer_has_block(w.ptr, cTypeName) != 0
}

// parseFFIError converts a C result pointer into a Go error.
// NULL means success (returns nil). Non-NULL is a JSON error string
// that must be freed.
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
