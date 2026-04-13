// Package wafer provides the Go SDK for writing WAFER blocks compiled to
// WebAssembly via TinyGo.
//
// Block authors implement the Block interface and call Register in their
// main() function. The SDK handles all WASM ABI plumbing.
//
// # Quick start
//
//	package main
//
//	import "github.com/suppers-ai/wafer-sdk-go"
//
//	type EchoBlock struct{}
//
//	func (b *EchoBlock) Info() wafer.BlockInfo {
//		return wafer.BlockInfo{
//			Name:         "example/echo",
//			Version:      "0.1.0",
//			Interface:    "handler@v1",
//			Summary:      "Echo block",
//			InstanceMode: "PerNode",
//		}
//	}
//
//	func (b *EchoBlock) Handle(msg wafer.Message) wafer.BlockResult {
//		return wafer.Respond(map[string]any{"echo": msg.Kind})
//	}
//
//	func main() { wafer.Register(&EchoBlock{}) }
package wafer

import (
	"bytes"
	"encoding/json"
	"strconv"
)

// ByteArray is a []byte that serializes as a JSON array of numbers,
// matching Rust's serde serialization of Vec<u8>.
// Go's encoding/json marshals []byte as base64; this type corrects that.
type ByteArray []byte

// MarshalJSON encodes the byte slice as a JSON array of unsigned integers,
// e.g. [104,101,108,108,111]. A nil slice encodes as [].
func (b ByteArray) MarshalJSON() ([]byte, error) {
	if b == nil {
		return []byte("[]"), nil
	}
	var buf bytes.Buffer
	buf.WriteByte('[')
	for i, v := range b {
		if i > 0 {
			buf.WriteByte(',')
		}
		buf.WriteString(strconv.Itoa(int(v)))
	}
	buf.WriteByte(']')
	return buf.Bytes(), nil
}

// UnmarshalJSON decodes a JSON array of unsigned integers into a ByteArray.
func (b *ByteArray) UnmarshalJSON(data []byte) error {
	var arr []uint8
	if err := json.Unmarshal(data, &arr); err != nil {
		return err
	}
	*b = ByteArray(arr)
	return nil
}

// MetaEntry is a key-value metadata entry attached to a Message or Response.
// Matches the Rust MetaEntry struct (serde field names: "key", "value").
type MetaEntry struct {
	Key   string `json:"key"`
	Value string `json:"value"`
}

// Message is a message flowing through the block pipeline.
// Matches the Rust Message struct exactly.
type Message struct {
	Kind string      `json:"kind"`
	Data ByteArray   `json:"data"`
	Meta []MetaEntry `json:"meta"`
}

// Action is the action a block wants the runtime to take after processing a
// message. Values must match the serde-serialized Rust Action enum variants
// (PascalCase, no rename_all attribute).
type Action string

const (
	// ActionContinue passes the message to the next block in the pipeline.
	ActionContinue Action = "Continue"
	// ActionRespond short-circuits the pipeline and returns a response.
	ActionRespond Action = "Respond"
	// ActionDrop silently discards the message.
	ActionDrop Action = "Drop"
	// ActionError signals that the block encountered an error.
	ActionError Action = "Error"
)

// Response is the payload returned when a block short-circuits the pipeline.
// Matches the Rust Response struct.
type Response struct {
	Data ByteArray   `json:"data"`
	Meta []MetaEntry `json:"meta"`
}

// ErrorCode mirrors the Rust ErrorCode enum. Values are PascalCase strings.
type ErrorCode string

const (
	ErrorCodeOk                 ErrorCode = "Ok"
	ErrorCodeCancelled          ErrorCode = "Cancelled"
	ErrorCodeUnknown            ErrorCode = "Unknown"
	ErrorCodeInvalidArgument    ErrorCode = "InvalidArgument"
	ErrorCodeDeadlineExceeded   ErrorCode = "DeadlineExceeded"
	ErrorCodeNotFound           ErrorCode = "NotFound"
	ErrorCodeAlreadyExists      ErrorCode = "AlreadyExists"
	ErrorCodePermissionDenied   ErrorCode = "PermissionDenied"
	ErrorCodeResourceExhausted  ErrorCode = "ResourceExhausted"
	ErrorCodeFailedPrecondition ErrorCode = "FailedPrecondition"
	ErrorCodeAborted            ErrorCode = "Aborted"
	ErrorCodeOutOfRange         ErrorCode = "OutOfRange"
	ErrorCodeUnimplemented      ErrorCode = "Unimplemented"
	ErrorCodeInternal           ErrorCode = "Internal"
	ErrorCodeUnavailable        ErrorCode = "Unavailable"
	ErrorCodeDataLoss           ErrorCode = "DataLoss"
	ErrorCodeUnauthenticated    ErrorCode = "Unauthenticated"
)

// WaferError is a structured error returned by a block.
// Matches the Rust WaferError struct: code is an ErrorCode enum, not a plain string.
type WaferError struct {
	Code    ErrorCode   `json:"code"`
	Message string      `json:"message"`
	Meta    []MetaEntry `json:"meta"`
}

// BlockResult is the result of processing a message.
// Matches the Rust BlockResult struct exactly.
type BlockResult struct {
	Action   Action      `json:"action"`
	Response *Response   `json:"response,omitempty"`
	Error    *WaferError `json:"error,omitempty"`
	Message  *Message    `json:"message,omitempty"`
}

// InstanceMode controls how many block instances are created and when.
// Values match the Rust InstanceMode enum (PascalCase).
type InstanceMode string

const (
	InstanceModePerNode      InstanceMode = "PerNode"
	InstanceModeSingleton    InstanceMode = "Singleton"
	InstanceModePerFlow      InstanceMode = "PerFlow"
	InstanceModePerExecution InstanceMode = "PerExecution"
)

// BlockInfo is the block's identity and metadata, returned by __wafer_info.
// The runtime deserializes this JSON and uses it to register the block.
// Only Name, Version, Interface, and Summary are required; InstanceMode
// defaults to "PerNode" in the runtime if omitted.
type BlockInfo struct {
	Name         string       `json:"name"`
	Version      string       `json:"version"`
	Interface    string       `json:"interface"`
	Summary      string       `json:"summary"`
	InstanceMode InstanceMode `json:"instance_mode,omitempty"`
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

// Respond JSON-encodes v and returns a BlockResult with ActionRespond.
// If v is already a []byte or ByteArray it is used as-is (raw JSON or binary data).
func Respond(v any) BlockResult {
	var data ByteArray
	switch val := v.(type) {
	case ByteArray:
		data = val
	case []byte:
		data = ByteArray(val)
	default:
		encoded, err := json.Marshal(v)
		if err != nil {
			panic("wafer: Respond marshal: " + err.Error())
		}
		data = ByteArray(encoded)
	}
	return RespondBytes(data)
}

// RespondBytes returns a BlockResult with ActionRespond carrying raw bytes.
func RespondBytes(data ByteArray) BlockResult {
	return BlockResult{
		Action:   ActionRespond,
		Response: &Response{Data: data, Meta: nil},
	}
}

// Continue returns a BlockResult that passes the message to the next block.
func Continue() BlockResult {
	return BlockResult{Action: ActionContinue}
}

// Drop returns a BlockResult that silently discards the message.
func Drop() BlockResult {
	return BlockResult{Action: ActionDrop}
}

// Error returns a BlockResult signalling an error.
func Error(code ErrorCode, message string) BlockResult {
	return BlockResult{
		Action: ActionError,
		Error: &WaferError{
			Code:    code,
			Message: message,
			Meta:    nil,
		},
	}
}
