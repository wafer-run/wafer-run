package wafer

import "encoding/json"

// Block is the interface every WAFER block must implement.
//
// Info returns the block's identity metadata. Handle processes an incoming
// message and returns a result telling the runtime what to do next.
type Block interface {
	Info() BlockInfo
	Handle(msg Message) BlockResult
}

// registeredBlock holds the single block registered via Register.
// WASM blocks are single-threaded so no synchronisation is needed.
var registeredBlock Block

// Register registers the block that will be invoked by the WAFER runtime.
// Call this exactly once, from main().
func Register(b Block) {
	registeredBlock = b
}

// waferInfo is exported as __wafer_info. The runtime calls it once (no args)
// to obtain the block's metadata as a JSON-encoded BlockInfo.
// Returns a packed (ptr << 32 | len) int64 pointing to the JSON bytes.
//
//go:export __wafer_info
func waferInfo() int64 {
	if registeredBlock == nil {
		panic("wafer: no block registered — call wafer.Register(block) in main()")
	}
	return jsonToPtr(registeredBlock.Info())
}

// waferHandle is exported as __wafer_handle. The runtime calls it for each
// incoming message. msgPtr/msgLen point to a JSON-encoded Message in guest
// memory. Returns a packed (ptr << 32 | len) int64 pointing to a
// JSON-encoded BlockResult.
//
//go:export __wafer_handle
func waferHandle(msgPtr int32, msgLen int32) int64 {
	data := ptrToBytes(msgPtr, msgLen)
	// Copy data out before unmarshal — the slice aliases guest memory that
	// could in principle be reused, though in practice WASM is single-threaded.
	buf := make([]byte, len(data))
	copy(buf, data)

	var msg Message
	if err := json.Unmarshal(buf, &msg); err != nil {
		panic("wafer: waferHandle unmarshal Message: " + err.Error())
	}

	result := registeredBlock.Handle(msg)
	return jsonToPtr(result)
}

// waferLifecycle is exported as __wafer_lifecycle. The runtime calls it for
// lifecycle events (init, start, stop). Currently a no-op; returns a
// JSON-encoded null (representing a successful Result<(), WaferError>).
//
//go:export __wafer_lifecycle
func waferLifecycle(_ int32, _ int32) int64 {
	// No-op — return JSON null which deserialises as Ok(()) on the Rust side.
	return jsonToPtr(nil)
}
