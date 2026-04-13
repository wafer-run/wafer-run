package wafer

import (
	"encoding/json"
	"unsafe"
)

// packPtrLen encodes a (ptr, length) pair as a single int64 using the
// WAFER ABI convention: high 32 bits = ptr, low 32 bits = length.
func packPtrLen(ptr, length uint32) int64 {
	return (int64(ptr) << 32) | int64(length)
}

// unpackPtrLen decodes an int64 produced by packPtrLen back to (ptr, length).
func unpackPtrLen(packed int64) (uint32, uint32) {
	ptr := uint32(packed >> 32)
	length := uint32(packed & 0xFFFFFFFF)
	return ptr, length
}

// jsonToPtr JSON-marshals value and returns a packed (ptr, len) int64
// pointing to the encoded bytes in guest (WASM linear) memory.
//
// The returned pointer is only valid for the lifetime of the current call.
// The host reads it before the guest function returns.
func jsonToPtr(value any) int64 {
	data, err := json.Marshal(value)
	if err != nil {
		panic("wafer: jsonToPtr marshal: " + err.Error())
	}
	if len(data) == 0 {
		// json.Marshal(nil) → "null"; should not produce empty slice.
		panic("wafer: jsonToPtr produced empty slice")
	}
	ptr := unsafe.Pointer(&data[0])
	// Keep data alive until after packPtrLen returns.
	_ = data
	return packPtrLen(uint32(uintptr(ptr)), uint32(len(data)))
}

// ptrToBytes returns a slice backed by the WASM linear memory at [ptr, ptr+length).
// The slice is only valid until the guest memory is next modified.
func ptrToBytes(ptr int32, length int32) []byte {
	return unsafe.Slice((*byte)(unsafe.Pointer(uintptr(ptr))), length)
}

// waferAlloc is the guest allocator exported as __wafer_alloc.
// The host calls this to reserve space in guest memory before writing data
// that will be passed to the guest (e.g. the serialized Message).
//
//go:export __wafer_alloc
func waferAlloc(size int32) int32 {
	buf := make([]byte, size)
	return int32(uintptr(unsafe.Pointer(&buf[0])))
}
