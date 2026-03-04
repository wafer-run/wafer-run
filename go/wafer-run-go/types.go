package wafer

import "fmt"

// Action tells the runtime what to do after a block processes a message.
type Action string

const (
	ActionContinue Action = "continue"
	ActionRespond  Action = "respond"
	ActionDrop     Action = "drop"
	ActionError    Action = "error"
)

// Message flows through the flow. A message contains a kind identifier,
// payload data, and metadata.
type Message struct {
	Kind string            `json:"kind"`
	Data []byte            `json:"data"`
	Meta map[string]string `json:"meta,omitempty"`
}

// NewMessage creates a new Message with the given kind and data.
func NewMessage(kind string, data []byte) *Message {
	return &Message{
		Kind: kind,
		Data: data,
		Meta: make(map[string]string),
	}
}

// SetMeta sets a metadata key-value pair on the message.
func (m *Message) SetMeta(key, value string) {
	if m.Meta == nil {
		m.Meta = make(map[string]string)
	}
	m.Meta[key] = value
}

// GetMeta returns a metadata value by key, or empty string if not found.
func (m *Message) GetMeta(key string) string {
	if m.Meta == nil {
		return ""
	}
	return m.Meta[key]
}

// Response carries data back to the caller when a block short-circuits.
type Response struct {
	Data []byte            `json:"data,omitempty"`
	Meta map[string]string `json:"meta,omitempty"`
}

// WaferError represents a structured error returned by a block.
type WaferError struct {
	Code    string            `json:"code"`
	Message string            `json:"message"`
	Meta    map[string]string `json:"meta,omitempty"`
}

// Error implements the error interface.
func (e *WaferError) Error() string {
	return fmt.Sprintf("%s: %s", e.Code, e.Message)
}

// Result is the outcome of a block processing a message.
type Result struct {
	Action   Action      `json:"action"`
	Response *Response   `json:"response,omitempty"`
	Error    *WaferError `json:"error,omitempty"`
}

// IsError returns true if the result represents an error.
func (r *Result) IsError() bool {
	return r.Action == ActionError
}

// IsContinue returns true if the result represents a continue action.
func (r *Result) IsContinue() bool {
	return r.Action == ActionContinue
}

// IsRespond returns true if the result represents a respond action.
func (r *Result) IsRespond() bool {
	return r.Action == ActionRespond
}

// ffiError is the JSON error structure returned by FFI functions.
type ffiError struct {
	Error string `json:"error"`
}
