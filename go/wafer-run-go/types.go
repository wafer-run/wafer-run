package wafer

import (
	"encoding/json"
	"fmt"
)

// Action tells the runtime what to do after a block processes a message.
type Action string

const (
	ActionContinue Action = "continue"
	ActionRespond  Action = "respond"
	ActionDrop     Action = "drop"
	ActionError    Action = "error"
	ActionHalt     Action = "halt"
)

// MetaEntry is one key-value metadata entry. The runtime's Message carries
// meta as an ordered list, not a map: a key may be set, replaced and read
// back in flow order.
type MetaEntry struct {
	Key   string `json:"key"`
	Value string `json:"value"`
}

// Message flows through the flow. It carries a kind identifier and metadata;
// the body travels separately as an input stream, and Wafer.Run dispatches
// with an empty one.
type Message struct {
	Kind string      `json:"kind"`
	Meta []MetaEntry `json:"meta"`
}

// MarshalJSON encodes a nil Meta as an empty list. The runtime's Message
// requires a meta list and rejects null, and a Message built as a literal
// without SetMeta has a nil Meta, which encoding/json writes as null.
func (m Message) MarshalJSON() ([]byte, error) {
	type wireMessage Message
	w := wireMessage(m)
	if w.Meta == nil {
		w.Meta = []MetaEntry{}
	}
	return json.Marshal(w)
}

// NewMessage creates a new Message with the given kind and no metadata.
func NewMessage(kind string) *Message {
	return &Message{Kind: kind}
}

// SetMeta sets a metadata key-value pair on the message, replacing any
// existing entry with the same key (the runtime's replace-by-key semantics).
func (m *Message) SetMeta(key, value string) {
	for i := range m.Meta {
		if m.Meta[i].Key == key {
			m.Meta[i].Value = value
			return
		}
	}
	m.Meta = append(m.Meta, MetaEntry{Key: key, Value: value})
}

// GetMeta returns a metadata value by key, or empty string if not found.
func (m *Message) GetMeta(key string) string {
	for _, e := range m.Meta {
		if e.Key == key {
			return e.Value
		}
	}
	return ""
}

// WaferError represents a structured error returned by a block.
//
// Code is the coarse error classification (e.g. "NotFound"); DetailCode is
// the block's application-level code (e.g. "auth.invalid_email"), empty when
// the block set none.
type WaferError struct {
	Code       string `json:"code"`
	Message    string `json:"message"`
	DetailCode string `json:"detail_code,omitempty"`
}

// Error implements the error interface.
func (e *WaferError) Error() string {
	return fmt.Sprintf("%s: %s", e.Code, e.Message)
}

// Result is the outcome of running a flow — the embedder wire format the
// runtime's embed::output_to_json produces.
//
// Body holds a Respond action's UTF-8 body; BodyBase64 holds it Base64-encoded
// when the body is not valid UTF-8, and always for Halt. Kind names the
// follow-up message on a Continue.
//
// Meta holds ONLY the canonical response keys — "resp.status",
// "resp.header.*", "resp.cookie.*", "resp.content_type" — for the host to
// apply to its response. Request state (headers, cookies, caller identity,
// client IP, query) never crosses this boundary, even when the block built
// its terminal from the request message. A Drop maps to a bodiless 204 that
// carries its Meta's headers and cookies.
type Result struct {
	Action     Action            `json:"action"`
	Body       string            `json:"body,omitempty"`
	BodyBase64 string            `json:"body_base64,omitempty"`
	Kind       string            `json:"kind,omitempty"`
	Meta       map[string]string `json:"meta,omitempty"`
	Error      *WaferError       `json:"error,omitempty"`
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

// IsHalt returns true if the result represents a halt action.
func (r *Result) IsHalt() bool {
	return r.Action == ActionHalt
}

// ffiError is the JSON error structure returned by FFI functions.
type ffiError struct {
	Error string `json:"error"`
}
