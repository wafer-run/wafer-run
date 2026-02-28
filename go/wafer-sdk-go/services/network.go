package services

import (
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/network"
)

// HttpRequest is a convenience alias for the WIT-generated HttpRequest.
type HttpRequest = network.HttpRequest

// HttpResponse is a convenience alias for the WIT-generated HttpResponse.
type HttpResponse = network.HttpResponse

// MetaEntry is a convenience alias for the WIT-generated MetaEntry.
type MetaEntry = network.MetaEntry

// NetworkDoRequest executes an outbound HTTP request through the runtime.
func NetworkDoRequest(req HttpRequest) (HttpResponse, error) {
	return network.DoRequest(req)
}

// NetworkGet performs a GET request to the given URL.
func NetworkGet(url string) (HttpResponse, error) {
	return network.DoRequest(HttpRequest{
		Method: "GET",
		URL:    url,
	})
}

// NetworkPostJSON performs a POST request with a JSON content-type header.
func NetworkPostJSON(url string, body []byte) (HttpResponse, error) {
	return network.DoRequest(HttpRequest{
		Method: "POST",
		URL:    url,
		Headers: []MetaEntry{
			{Key: "Content-Type", Value: "application/json"},
		},
		Body: &body,
	})
}
