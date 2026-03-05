package services

import (
	"encoding/json"
	"fmt"

	wafer "github.com/wafer-run/wafer-run/go/wafer-sdk-go"
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/network"
)

// HttpRequest is a convenience alias for the WIT-generated HttpRequest.
type HttpRequest = network.HttpRequest

// HttpResponse is a convenience alias for the WIT-generated HttpResponse.
type HttpResponse = network.HttpResponse

// MetaEntry is a convenience alias for the WIT-generated MetaEntry.
type MetaEntry = network.MetaEntry

// --- CallBlock-based implementations (context-aware) ---

// NetworkDoRequestCtx executes an outbound HTTP request through the runtime
// using CallBlock.
func NetworkDoRequestCtx(ctx wafer.Context, req HttpRequest) (HttpResponse, error) {
	reqJSON, err := json.Marshal(req)
	if err != nil {
		return HttpResponse{}, fmt.Errorf("internal: failed to marshal request: %w", err)
	}
	msg := wafer.NewMessage("network.do_request", reqJSON)
	result := ctx.CallBlock("@wafer/network", msg)
	if result.Error != nil {
		return HttpResponse{}, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var resp HttpResponse
	if err := json.Unmarshal(result.Response.Data, &resp); err != nil {
		return HttpResponse{}, err
	}
	return resp, nil
}

// NetworkGetCtx performs a GET request to the given URL using CallBlock.
func NetworkGetCtx(ctx wafer.Context, url string) (HttpResponse, error) {
	return NetworkDoRequestCtx(ctx, HttpRequest{
		Method: "GET",
		URL:    url,
	})
}

// NetworkPostJSONCtx performs a POST request with a JSON content-type header
// using CallBlock.
func NetworkPostJSONCtx(ctx wafer.Context, url string, body []byte) (HttpResponse, error) {
	return NetworkDoRequestCtx(ctx, HttpRequest{
		Method: "POST",
		URL:    url,
		Headers: []MetaEntry{
			{Key: "Content-Type", Value: "application/json"},
		},
		Body: &body,
	})
}

// --- Legacy direct-import implementations (backward compatible) ---

// NetworkDoRequest executes an outbound HTTP request through the runtime.
// When CallBlock is available, it routes through the "@wafer/network" block.
func NetworkDoRequest(req HttpRequest) (HttpResponse, error) {
	if wafer.HasCallBlock() {
		return NetworkDoRequestCtx(wafer.NewContext(), req)
	}
	return network.DoRequest(req)
}

// NetworkGet performs a GET request to the given URL.
func NetworkGet(url string) (HttpResponse, error) {
	if wafer.HasCallBlock() {
		return NetworkGetCtx(wafer.NewContext(), url)
	}
	return network.DoRequest(HttpRequest{
		Method: "GET",
		URL:    url,
	})
}

// NetworkPostJSON performs a POST request with a JSON content-type header.
func NetworkPostJSON(url string, body []byte) (HttpResponse, error) {
	if wafer.HasCallBlock() {
		return NetworkPostJSONCtx(wafer.NewContext(), url, body)
	}
	return network.DoRequest(HttpRequest{
		Method: "POST",
		URL:    url,
		Headers: []MetaEntry{
			{Key: "Content-Type", Value: "application/json"},
		},
		Body: &body,
	})
}
