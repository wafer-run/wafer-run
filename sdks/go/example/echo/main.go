// Echo block — example WAFER block written in Go.
//
// Receives any message, logs it, and responds with a JSON object echoing the
// message kind, data length, and metadata count.
package main

import (
	"encoding/json"

	wafer "github.com/suppers-ai/wafer-sdk-go"
)

// EchoBlock echoes incoming messages back as JSON responses.
type EchoBlock struct{}

// Info returns the block's identity metadata.
func (b *EchoBlock) Info() wafer.BlockInfo {
	return wafer.BlockInfo{
		Name:         "example/echo-go",
		Version:      "0.1.0",
		Interface:    "handler@v1",
		Summary:      "Echo block written in Go",
		InstanceMode: wafer.InstanceModePerNode,
	}
}

// Handle processes an incoming message and returns an echo response.
func (b *EchoBlock) Handle(msg wafer.Message) wafer.BlockResult {
	wafer.Log("info", "echo-go: handling message kind="+msg.Kind)

	response := map[string]any{
		"echo":       true,
		"kind":       msg.Kind,
		"data_len":   len(msg.Data),
		"meta_count": len(msg.Meta),
	}

	data, err := json.Marshal(response)
	if err != nil {
		return wafer.Error(wafer.ErrorCodeInternal, "marshal echo response: "+err.Error())
	}

	return wafer.BlockResult{
		Action: wafer.ActionRespond,
		Response: &wafer.Response{
			Data: data,
			Meta: []wafer.MetaEntry{
				{Key: "content-type", Value: "application/json"},
			},
		},
	}
}

func main() {
	wafer.Register(&EchoBlock{})
}
