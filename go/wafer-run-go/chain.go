package wafer

import "encoding/json"

// ChainDef defines a chain in JSON configuration.
type ChainDef struct {
	ID      string        `json:"id"`
	Summary string        `json:"summary,omitempty"`
	Config  ChainConfig   `json:"config,omitempty"`
	HTTP    *HTTPRouteDef `json:"http,omitempty"`
	Root    NodeDef       `json:"root"`
}

// ChainConfig holds chain-level configuration.
type ChainConfig struct {
	OnError string `json:"on_error,omitempty"`
	Timeout string `json:"timeout,omitempty"`
}

// NodeDef defines a node in the chain tree.
type NodeDef struct {
	Block    string           `json:"block,omitempty"`
	Chain    string           `json:"chain,omitempty"`
	Match    string           `json:"match,omitempty"`
	Config   *json.RawMessage `json:"config,omitempty"`
	Instance string           `json:"instance,omitempty"`
	Next     []NodeDef        `json:"next,omitempty"`
}

// HTTPRouteDef declares HTTP routes for a chain.
type HTTPRouteDef struct {
	Routes []HTTPRoute `json:"routes"`
}

// HTTPRoute defines an HTTP route that a chain handles.
type HTTPRoute struct {
	Methods    []string `json:"methods,omitempty"`
	Path       string   `json:"path"`
	PathPrefix bool     `json:"path_prefix,omitempty"`
}

// ChainInfo provides read-only info about a chain.
type ChainInfo struct {
	ID      string `json:"id"`
	Summary string `json:"summary"`
	OnError string `json:"on_error,omitempty"`
	Timeout string `json:"timeout,omitempty"`
}
