package wafer

// Context provides runtime capabilities to blocks during message processing.
// It is the primary way for blocks to interact with the WAFER runtime, including
// calling other blocks in the flow.
type Context interface {
	// CallBlock invokes a named block (e.g. "wafer/database", "wafer/storage")
	// with the given message and returns the block's result.
	CallBlock(blockName string, msg *Message) *BlockResult
}

// callBlockFunc is the function type for the host-provided CallBlock implementation.
// It is set by the runtime before calling into block code.
type callBlockFunc func(blockName string, msg *Message) *BlockResult

// defaultContext is the standard Context implementation backed by a host-provided
// CallBlock function.
type defaultContext struct {
	callBlock callBlockFunc
}

// CallBlock delegates to the host-provided call_block import.
func (c *defaultContext) CallBlock(blockName string, msg *Message) *BlockResult {
	if c.callBlock == nil {
		return &BlockResult{
			Action: ActionError,
			Error: &WaferError{
				Code:    ErrorCodeInternal,
				Message: "call_block not available: runtime does not support CallBlock",
			},
		}
	}
	return c.callBlock(blockName, msg)
}

// globalCallBlock holds the host-provided CallBlock function.
// It is set by the runtime (or by tests) before block code executes.
var globalCallBlock callBlockFunc

// SetCallBlock registers the host-provided CallBlock function.
// This is called by the runtime glue code during initialization.
func SetCallBlock(fn func(blockName string, msg *Message) *BlockResult) {
	globalCallBlock = fn
}

// NewContext creates a new Context using the globally registered CallBlock.
func NewContext() Context {
	return &defaultContext{callBlock: globalCallBlock}
}

// NewContextWith creates a Context with a custom CallBlock implementation.
// Useful for testing.
func NewContextWith(fn func(blockName string, msg *Message) *BlockResult) Context {
	return &defaultContext{callBlock: fn}
}

// HasCallBlock reports whether a CallBlock function has been registered.
// Service wrappers use this to decide whether to route through CallBlock
// or fall back to direct WIT imports.
func HasCallBlock() bool {
	return globalCallBlock != nil
}
