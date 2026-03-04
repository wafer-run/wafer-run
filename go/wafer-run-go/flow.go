package wafer

import "encoding/json"

// FlowDef defines a flow in JSON configuration.
type FlowDef struct {
	ID      string     `json:"id"`
	Summary string     `json:"summary,omitempty"`
	Config  FlowConfig `json:"config,omitempty"`
	Root    NodeDef    `json:"root"`
}

// FlowConfig holds flow-level configuration.
type FlowConfig struct {
	OnError string `json:"on_error,omitempty"`
	Timeout string `json:"timeout,omitempty"`
}

// NodeDef defines a node in the flow tree.
type NodeDef struct {
	Block    string           `json:"block,omitempty"`
	Flow     string           `json:"flow,omitempty"`
	Match    string           `json:"match,omitempty"`
	Config   *json.RawMessage `json:"config,omitempty"`
	Instance string           `json:"instance,omitempty"`
	Next     []NodeDef        `json:"next,omitempty"`
}

// FlowInfo provides read-only info about a flow.
type FlowInfo struct {
	ID      string `json:"id"`
	Summary string `json:"summary"`
	OnError string `json:"on_error,omitempty"`
	Timeout string `json:"timeout,omitempty"`
}
