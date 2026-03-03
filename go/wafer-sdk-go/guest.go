// Package wafer provides the SDK for writing WAFER blocks that compile
// to WebAssembly components using the Component Model.
//
// Blocks built with this SDK run inside the WAFER runtime and communicate
// with the host through typed WIT interfaces — no manual serialization needed.
//
// Build blocks with:
//
//	tinygo build -target=wasip2 -o block.wasm .
//	# OR: GOOS=wasip1 GOARCH=wasm go build -o block.core.wasm . && wasm-tools component new block.core.wasm -o block.wasm
//
// A minimal block implementation looks like:
//
//	package main
//
//	import wafer "github.com/wafer-run/wafer-run/go/wafer-sdk-go"
//
//	type MyBlock struct{}
//
//	func (b *MyBlock) Info() wafer.BlockInfo {
//	    return wafer.BlockInfo{
//	        Name:      "@example/myblock",
//	        Version:   "1.0.0",
//	        Interface: "processor@v1",
//	        Summary:   "A simple message processor.",
//	    }
//	}
//
//	func (b *MyBlock) Handle(msg *wafer.Message) *wafer.BlockResult {
//	    return msg.Continue()
//	}
//
//	func (b *MyBlock) Lifecycle(event wafer.LifecycleEvent) error {
//	    return nil
//	}
//
//	func main() {
//	    wafer.Export(&MyBlock{})
//	}
package wafer

// Block is the interface that every WAFER block must implement.
// The runtime calls these methods through the WIT-generated component exports.
type Block interface {
	// Info returns the block's identity and configuration.
	Info() BlockInfo

	// Handle processes a message and returns a result.
	Handle(msg *Message) *BlockResult

	// Lifecycle handles lifecycle events (init, start, stop).
	Lifecycle(event LifecycleEvent) error
}

// ContextBlock is an optional interface that blocks may implement to receive
// a Context. When the runtime detects a ContextBlock, it passes a Context
// that provides CallBlock and other runtime capabilities.
//
// Usage:
//
//	func (b *MyBlock) HandleWithContext(ctx wafer.Context, msg *wafer.Message) *wafer.BlockResult {
//	    result := ctx.CallBlock("wafer/database", wafer.NewMessage("database.get", nil))
//	    ...
//	}
type ContextBlock interface {
	Block

	// HandleWithContext processes a message with access to runtime capabilities.
	// When implemented, the runtime will call this method instead of Handle.
	HandleWithContext(ctx Context, msg *Message) *BlockResult
}

// registeredBlock holds the globally registered block instance.
var registeredBlock Block

// Export stores a Block implementation as the global block for this WASM
// module. This must be called from main() before the runtime invokes any
// exported functions.
func Export(block Block) {
	registeredBlock = block
}

// Register is a deprecated alias for Export. Use Export instead.
func Register(block Block) {
	Export(block)
}
