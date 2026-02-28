package wafer

import "encoding/json"

// NewChainDef creates a minimal ChainDef with a root block.
func NewChainDef(id, rootBlock string) ChainDef {
	return ChainDef{
		ID:   id,
		Root: NodeDef{Block: rootBlock},
	}
}

// NewNodeDef creates a NodeDef for a given block type.
func NewNodeDef(block string) NodeDef {
	return NodeDef{Block: block}
}

// WithMatch sets the match pattern on a NodeDef and returns it.
func (n NodeDef) WithMatch(pattern string) NodeDef {
	n.Match = pattern
	return n
}

// WithNext appends child nodes and returns the NodeDef.
func (n NodeDef) WithNext(children ...NodeDef) NodeDef {
	n.Next = append(n.Next, children...)
	return n
}

// WithConfig sets the config on a NodeDef and returns it.
func (n NodeDef) WithConfig(config interface{}) NodeDef {
	data, err := json.Marshal(config)
	if err == nil {
		raw := json.RawMessage(data)
		n.Config = &raw
	}
	return n
}

// ErrorResult creates a Result representing an error.
func ErrorResult(code, message string) *Result {
	return &Result{
		Action: ActionError,
		Error: &WaferError{
			Code:    code,
			Message: message,
		},
	}
}
