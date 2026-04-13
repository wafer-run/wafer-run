package wafer

import (
	"encoding/json"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Host imports — declared with //go:wasmimport so TinyGo emits the correct
// WASM import section entries. These are linked by the WAFER wasmi runtime.
// ---------------------------------------------------------------------------

//go:wasmimport wafer __wafer_host_is_cancelled
func waferHostIsCancelled() int32

//go:wasmimport wafer __wafer_host_log
func waferHostLog(levelPtr, levelLen, msgPtr, msgLen int32)

//go:wasmimport wafer __wafer_host_call_block
func waferHostCallBlock(namePtr, nameLen, msgPtr, msgLen int32) int64

// ---------------------------------------------------------------------------
// Public wrappers
// ---------------------------------------------------------------------------

// IsCancelled reports whether the current execution has been cancelled by the
// runtime (e.g. client disconnect or deadline exceeded).
func IsCancelled() bool {
	return waferHostIsCancelled() != 0
}

// Log emits a log message at the given level. Conventional level strings are
// "trace", "debug", "info", "warn", and "error".
func Log(level, msg string) {
	if level == "" || msg == "" {
		return
	}
	levelBytes := []byte(level)
	msgBytes := []byte(msg)
	waferHostLog(
		int32(uintptr(unsafe.Pointer(&levelBytes[0]))), int32(len(levelBytes)),
		int32(uintptr(unsafe.Pointer(&msgBytes[0]))), int32(len(msgBytes)),
	)
}

// CallBlock calls another block by name and returns its BlockResult.
// msg is JSON-marshalled before the call; the result is JSON-unmarshalled.
// The packed (ptr << 32 | len) return value from the host points to a
// JSON-encoded BlockResult allocated in guest memory via __wafer_alloc.
func CallBlock(name string, msg *Message) BlockResult {
	if name == "" {
		panic("wafer: CallBlock called with empty block name")
	}
	nameBytes := []byte(name)
	msgData, err := json.Marshal(msg)
	if err != nil {
		panic("wafer: CallBlock marshal message: " + err.Error())
	}
	if len(msgData) == 0 {
		panic("wafer: CallBlock: marshalled message is empty")
	}

	packed := waferHostCallBlock(
		int32(uintptr(unsafe.Pointer(&nameBytes[0]))), int32(len(nameBytes)),
		int32(uintptr(unsafe.Pointer(&msgData[0]))), int32(len(msgData)),
	)

	ptr, length := unpackPtrLen(packed)
	resultData := ptrToBytes(int32(ptr), int32(length))

	// Copy before unmarshal — the slice aliases memory owned by the guest
	// allocator that the runtime may reuse.
	buf := make([]byte, len(resultData))
	copy(buf, resultData)

	var result BlockResult
	if err := json.Unmarshal(buf, &result); err != nil {
		panic("wafer: CallBlock unmarshal BlockResult: " + err.Error())
	}
	return result
}
