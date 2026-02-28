package wafer

import (
	"encoding/json"
	"strconv"
)

// Convenience constructors for common BlockResult values.

// ContinueResult returns a BlockResult that passes the message to the next block.
func ContinueResult() *BlockResult {
	return &BlockResult{Action: ActionContinue}
}

// RespondResult returns a BlockResult that short-circuits with a response.
func RespondResult(resp *Response) *BlockResult {
	return &BlockResult{Action: ActionRespond, Response: resp}
}

// DropResult returns a BlockResult that ends the chain silently.
func DropResult() *BlockResult {
	return &BlockResult{Action: ActionDrop}
}

// ErrorResult returns a BlockResult that short-circuits with an error.
func ErrorResult(err *WaferError) *BlockResult {
	return &BlockResult{Action: ActionError, Error: err}
}

// RespondData creates a Respond result with the given data and optional metadata.
func RespondData(data []byte, meta map[string]string) *BlockResult {
	return &BlockResult{
		Action: ActionRespond,
		Response: &Response{
			Data: data,
			Meta: meta,
		},
	}
}

// JsonRespond creates a Respond result by JSON-encoding the given value.
func JsonRespond(v any) *BlockResult {
	data, err := json.Marshal(v)
	if err != nil {
		return ErrInternal("failed to marshal response: " + err.Error())
	}
	return &BlockResult{
		Action: ActionRespond,
		Response: &Response{
			Data: data,
			Meta: map[string]string{"content-type": "application/json"},
		},
	}
}

// Error creates an error BlockResult with the given code and message.
func Error(code, message string) *BlockResult {
	return &BlockResult{
		Action: ActionError,
		Error: &WaferError{
			Code:    code,
			Message: message,
		},
	}
}

// ErrorWithMeta creates an error BlockResult with code, message, and metadata.
func ErrorWithMeta(code, message string, meta map[string]string) *BlockResult {
	return &BlockResult{
		Action: ActionError,
		Error: &WaferError{
			Code:    code,
			Message: message,
			Meta:    meta,
		},
	}
}

// Convenience error constructors for common error codes.

func ErrBadRequest(message string) *BlockResult      { return Error(ErrorCodeInvalidArgument, message) }
func ErrNotFound(message string) *BlockResult         { return Error(ErrorCodeNotFound, message) }
func ErrAlreadyExists(message string) *BlockResult    { return Error(ErrorCodeAlreadyExists, message) }
func ErrPermissionDenied(message string) *BlockResult { return Error(ErrorCodePermissionDenied, message) }
func ErrUnauthenticated(message string) *BlockResult  { return Error(ErrorCodeUnauthenticated, message) }
func ErrUnavailable(message string) *BlockResult      { return Error(ErrorCodeUnavailable, message) }
func ErrDeadlineExceeded(message string) *BlockResult { return Error(ErrorCodeDeadlineExceeded, message) }
func ErrResourceExhausted(message string) *BlockResult {
	return Error(ErrorCodeResourceExhausted, message)
}
func ErrFailedPrecondition(message string) *BlockResult {
	return Error(ErrorCodeFailedPrecondition, message)
}
func ErrInternal(message string) *BlockResult { return Error(ErrorCodeInternal, message) }

// RespondWithStatus creates a Respond result with an HTTP status code.
func RespondWithStatus(status int, data []byte, contentType string) *BlockResult {
	return &BlockResult{
		Action: ActionRespond,
		Response: &Response{
			Data: data,
			Meta: map[string]string{
				"resp.status":  strconv.Itoa(status),
				"content-type": contentType,
			},
		},
	}
}

// JsonRespondStatus creates a JSON Respond result with an HTTP status code.
func JsonRespondStatus(status int, v any) *BlockResult {
	data, err := json.Marshal(v)
	if err != nil {
		return ErrInternal("failed to marshal response: " + err.Error())
	}
	return &BlockResult{
		Action: ActionRespond,
		Response: &Response{
			Data: data,
			Meta: map[string]string{
				"resp.status":  strconv.Itoa(status),
				"content-type": "application/json",
			},
		},
	}
}

// ErrorStatus creates an error BlockResult with an HTTP status code.
func ErrorStatus(status int, code, message string) *BlockResult {
	return &BlockResult{
		Action: ActionError,
		Error: &WaferError{
			Code:    code,
			Message: message,
			Meta: map[string]string{
				"resp.status": strconv.Itoa(status),
			},
		},
	}
}

// ResponseBuilder provides a fluent API for constructing Response values.
type ResponseBuilder struct {
	data []byte
	meta map[string]string
}

// NewResponseBuilder creates a new ResponseBuilder.
func NewResponseBuilder() *ResponseBuilder {
	return &ResponseBuilder{
		meta: make(map[string]string),
	}
}

// Data sets the response payload to raw bytes.
func (b *ResponseBuilder) Data(data []byte) *ResponseBuilder {
	b.data = data
	return b
}

// JSON sets the response payload by JSON-encoding the given value.
func (b *ResponseBuilder) JSON(v any) *ResponseBuilder {
	data, err := json.Marshal(v)
	if err != nil {
		b.data = nil
		return b
	}
	b.data = data
	b.meta["content-type"] = "application/json"
	return b
}

// Meta sets a metadata key-value pair on the response.
func (b *ResponseBuilder) Meta(key, value string) *ResponseBuilder {
	b.meta[key] = value
	return b
}

// Build constructs the Response from the builder state.
func (b *ResponseBuilder) Build() *Response {
	return &Response{
		Data: b.data,
		Meta: b.meta,
	}
}

// Respond creates a Respond result from the builder state.
func (b *ResponseBuilder) Respond() *BlockResult {
	return &BlockResult{
		Action:   ActionRespond,
		Response: b.Build(),
	}
}
