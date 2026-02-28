package wafer

import "encoding/json"

// Message represents a message flowing through a WAFER chain.
type Message struct {
	Kind string
	Data []byte
	Meta map[string]string
}

// GetMeta returns the metadata value for the given key, or "" if absent.
func (m *Message) GetMeta(key string) string {
	if m.Meta == nil {
		return ""
	}
	return m.Meta[key]
}

// SetMeta sets a metadata key-value pair on the message.
func (m *Message) SetMeta(key, value string) {
	if m.Meta == nil {
		m.Meta = make(map[string]string)
	}
	m.Meta[key] = value
}

// Unmarshal deserializes the data payload as JSON.
func (m *Message) Unmarshal(v interface{}) error {
	return json.Unmarshal(m.Data, v)
}

// Continue returns a BlockResult that passes this message to the next block.
func (m *Message) Continue() *BlockResult {
	return &BlockResult{
		Action:  ActionContinue,
		Message: m,
	}
}

// Respond returns a BlockResult that short-circuits with a response.
func (m *Message) Respond(r *Response) *BlockResult {
	return &BlockResult{
		Action:   ActionRespond,
		Response: r,
		Message:  m,
	}
}

// DropMsg returns a BlockResult that silently drops this message.
func (m *Message) DropMsg() *BlockResult {
	return &BlockResult{
		Action:  ActionDrop,
		Message: m,
	}
}

// Err returns a BlockResult that short-circuits with an error.
func (m *Message) Err(e *WaferError) *BlockResult {
	return &BlockResult{
		Action:  ActionError,
		Error:   e,
		Message: m,
	}
}

// Action indicates what the runtime should do after a block processes a message.
type Action int

const (
	ActionContinue Action = iota
	ActionRespond
	ActionDrop
	ActionError
)

func (a Action) String() string {
	switch a {
	case ActionContinue:
		return "continue"
	case ActionRespond:
		return "respond"
	case ActionDrop:
		return "drop"
	case ActionError:
		return "error"
	default:
		return "continue"
	}
}

// Response holds the data returned when a block short-circuits.
type Response struct {
	Data []byte
	Meta map[string]string
}

// WaferError represents an error returned by a block.
type WaferError struct {
	Code    string
	Message string
	Meta    map[string]string
}

func (e *WaferError) Error() string {
	return e.Code + ": " + e.Message
}

// BlockResult is the outcome of processing a message.
type BlockResult struct {
	Action   Action
	Response *Response
	Error    *WaferError
	Message  *Message
}

// InstanceMode controls how many instances of a block are created.
type InstanceMode int

const (
	PerNode InstanceMode = iota
	Singleton
	PerChain
	PerExecution
)

func (m InstanceMode) String() string {
	switch m {
	case PerNode:
		return "per-node"
	case Singleton:
		return "singleton"
	case PerChain:
		return "per-chain"
	case PerExecution:
		return "per-execution"
	default:
		return "per-node"
	}
}

// BlockInfo declares the identity and configuration of a block.
type BlockInfo struct {
	Name         string
	Version      string
	Interface    string
	Summary      string
	InstanceMode InstanceMode
	AllowedModes []InstanceMode
}

// LifecycleEvent represents a lifecycle event delivered to a block.
type LifecycleEvent struct {
	Type LifecycleType
	Data []byte
}

// LifecycleType identifies the kind of lifecycle event.
type LifecycleType int

const (
	Init LifecycleType = iota
	Start
	Stop
)

func (t LifecycleType) String() string {
	switch t {
	case Init:
		return "init"
	case Start:
		return "start"
	case Stop:
		return "stop"
	default:
		return "init"
	}
}
