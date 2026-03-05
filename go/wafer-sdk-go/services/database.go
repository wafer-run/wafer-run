// Package services provides ergonomic client wrappers for WAFER runtime
// capabilities. Each function delegates to WIT-generated host imports in gen/wafer/*.
package services

import (
	"encoding/json"
	"fmt"

	wafer "github.com/wafer-run/wafer-run/go/wafer-sdk-go"
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/database"
)

// Record is a convenience alias for the WIT-generated DbRecord.
type Record = database.DbRecord

// RecordList is a convenience alias for the WIT-generated RecordList.
type RecordList = database.RecordList

// FilterOp is a convenience alias for database.FilterOp.
type FilterOp = database.FilterOp

// Filter is a convenience alias for database.Filter.
type Filter = database.Filter

// SortField is a convenience alias for database.SortField.
type SortField = database.SortField

// ListOptions is a convenience alias for database.ListOptions.
type ListOptions = database.ListOptions

// Re-export filter operator constants for convenience.
const (
	OpEqual      = database.FilterOpEq
	OpNotEqual   = database.FilterOpNeq
	OpGreater    = database.FilterOpGt
	OpGreaterEq  = database.FilterOpGte
	OpLess       = database.FilterOpLt
	OpLessEq     = database.FilterOpLte
	OpLike       = database.FilterOpLike
	OpIn         = database.FilterOpIn
	OpIsNull     = database.FilterOpIsNull
	OpIsNotNull  = database.FilterOpIsNotNull
)

// --- CallBlock-based implementations (context-aware) ---

// DatabaseGetCtx retrieves a single record by collection and ID using CallBlock.
func DatabaseGetCtx(ctx wafer.Context, collection, id string) (Record, error) {
	msg := wafer.NewMessage("database.get", nil)
	msg.SetMeta("collection", collection)
	msg.SetMeta("id", id)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return Record{}, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var record Record
	if err := json.Unmarshal(result.Response.Data, &record); err != nil {
		return Record{}, err
	}
	return record, nil
}

// DatabaseGetIntoCtx retrieves a single record and unmarshals its JSON data
// field into the provided value using CallBlock.
func DatabaseGetIntoCtx(ctx wafer.Context, collection, id string, v any) error {
	rec, err := DatabaseGetCtx(ctx, collection, id)
	if err != nil {
		return err
	}
	return json.Unmarshal([]byte(rec.Data), v)
}

// DatabaseListCtx retrieves records from a collection with the given options
// using CallBlock.
func DatabaseListCtx(ctx wafer.Context, collection string, opts ListOptions) (RecordList, error) {
	optsJSON, err := json.Marshal(opts)
	if err != nil {
		return RecordList{}, fmt.Errorf("internal: failed to marshal list options: %w", err)
	}
	msg := wafer.NewMessage("database.list", optsJSON)
	msg.SetMeta("collection", collection)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return RecordList{}, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var rl RecordList
	if err := json.Unmarshal(result.Response.Data, &rl); err != nil {
		return RecordList{}, err
	}
	return rl, nil
}

// DatabaseListAllCtx retrieves all records from a collection with no filters
// using CallBlock.
func DatabaseListAllCtx(ctx wafer.Context, collection string) (RecordList, error) {
	return DatabaseListCtx(ctx, collection, ListOptions{})
}

// DatabaseCreateCtx inserts a new record using CallBlock. The data argument is
// JSON-encoded and sent as the record's data field.
func DatabaseCreateCtx(ctx wafer.Context, collection string, data any) (Record, error) {
	jsonData, err := json.Marshal(data)
	if err != nil {
		return Record{}, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal record: " + err.Error(),
		}
	}
	msg := wafer.NewMessage("database.create", jsonData)
	msg.SetMeta("collection", collection)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return Record{}, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var record Record
	if err := json.Unmarshal(result.Response.Data, &record); err != nil {
		return Record{}, err
	}
	return record, nil
}

// DatabaseUpdateCtx modifies an existing record using CallBlock. The data
// argument is JSON-encoded.
func DatabaseUpdateCtx(ctx wafer.Context, collection, id string, data any) (Record, error) {
	jsonData, err := json.Marshal(data)
	if err != nil {
		return Record{}, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal record: " + err.Error(),
		}
	}
	msg := wafer.NewMessage("database.update", jsonData)
	msg.SetMeta("collection", collection)
	msg.SetMeta("id", id)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return Record{}, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var record Record
	if err := json.Unmarshal(result.Response.Data, &record); err != nil {
		return Record{}, err
	}
	return record, nil
}

// DatabaseDeleteCtx removes a record from a collection by ID using CallBlock.
func DatabaseDeleteCtx(ctx wafer.Context, collection, id string) error {
	msg := wafer.NewMessage("database.delete", nil)
	msg.SetMeta("collection", collection)
	msg.SetMeta("id", id)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return nil
}

// DatabaseCountCtx returns the number of records matching the given filters
// using CallBlock.
func DatabaseCountCtx(ctx wafer.Context, collection string, filters []Filter) (int64, error) {
	filtersJSON, err := json.Marshal(filters)
	if err != nil {
		return 0, fmt.Errorf("internal: failed to marshal filters: %w", err)
	}
	msg := wafer.NewMessage("database.count", filtersJSON)
	msg.SetMeta("collection", collection)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return 0, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var count int64
	if err := json.Unmarshal(result.Response.Data, &count); err != nil {
		return 0, err
	}
	return count, nil
}

// DatabaseGetByFieldCtx retrieves a single record where field equals value
// using CallBlock.
func DatabaseGetByFieldCtx(ctx wafer.Context, collection, field, value string) (Record, error) {
	rl, err := DatabaseListCtx(ctx, collection, ListOptions{
		Filters: []Filter{{
			Field:    field,
			Operator: OpEqual,
			Value:    value,
		}},
		Limit: 1,
	})
	if err != nil {
		return Record{}, err
	}
	if len(rl.Records) == 0 {
		return Record{}, &wafer.WaferError{
			Code:    "not_found",
			Message: "record not found in " + collection + " where " + field + " = " + value,
		}
	}
	return rl.Records[0], nil
}

// DatabaseQueryRawCtx executes a raw SELECT query and returns records using
// CallBlock.
func DatabaseQueryRawCtx(ctx wafer.Context, query string, args ...any) ([]Record, error) {
	argsJSON, err := json.Marshal(args)
	if err != nil {
		return nil, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal query args: " + err.Error(),
		}
	}
	msg := wafer.NewMessage("database.query_raw", argsJSON)
	msg.SetMeta("query", query)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return nil, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var records []Record
	if err := json.Unmarshal(result.Response.Data, &records); err != nil {
		return nil, err
	}
	return records, nil
}

// DatabaseExecRawCtx executes a raw non-SELECT statement and returns affected
// rows using CallBlock.
func DatabaseExecRawCtx(ctx wafer.Context, query string, args ...any) (int64, error) {
	argsJSON, err := json.Marshal(args)
	if err != nil {
		return 0, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal query args: " + err.Error(),
		}
	}
	msg := wafer.NewMessage("database.exec_raw", argsJSON)
	msg.SetMeta("query", query)
	result := ctx.CallBlock("@wafer/database", msg)
	if result.Error != nil {
		return 0, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var affected int64
	if err := json.Unmarshal(result.Response.Data, &affected); err != nil {
		return 0, err
	}
	return affected, nil
}

// --- Legacy direct-import implementations (backward compatible) ---

// DatabaseGet retrieves a single record by collection and ID.
// When CallBlock is available, it routes through the "@wafer/database" block.
func DatabaseGet(collection, id string) (Record, error) {
	if wafer.HasCallBlock() {
		return DatabaseGetCtx(wafer.NewContext(), collection, id)
	}
	return database.Get(collection, id)
}

// DatabaseGetInto retrieves a single record and unmarshals its JSON data field
// into the provided value.
func DatabaseGetInto(collection, id string, v any) error {
	if wafer.HasCallBlock() {
		return DatabaseGetIntoCtx(wafer.NewContext(), collection, id, v)
	}
	rec, err := database.Get(collection, id)
	if err != nil {
		return err
	}
	return json.Unmarshal([]byte(rec.Data), v)
}

// DatabaseList retrieves records from a collection with the given options.
func DatabaseList(collection string, opts ListOptions) (RecordList, error) {
	if wafer.HasCallBlock() {
		return DatabaseListCtx(wafer.NewContext(), collection, opts)
	}
	return database.List(collection, opts)
}

// DatabaseListAll retrieves all records from a collection with no filters.
func DatabaseListAll(collection string) (RecordList, error) {
	if wafer.HasCallBlock() {
		return DatabaseListAllCtx(wafer.NewContext(), collection)
	}
	return database.List(collection, ListOptions{})
}

// DatabaseCreate inserts a new record. The data argument is JSON-encoded and
// sent as the record's data field.
func DatabaseCreate(collection string, data any) (Record, error) {
	if wafer.HasCallBlock() {
		return DatabaseCreateCtx(wafer.NewContext(), collection, data)
	}
	jsonData, err := json.Marshal(data)
	if err != nil {
		return Record{}, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal record: " + err.Error(),
		}
	}
	return database.Create(collection, string(jsonData))
}

// DatabaseUpdate modifies an existing record. The data argument is JSON-encoded.
func DatabaseUpdate(collection, id string, data any) (Record, error) {
	if wafer.HasCallBlock() {
		return DatabaseUpdateCtx(wafer.NewContext(), collection, id, data)
	}
	jsonData, err := json.Marshal(data)
	if err != nil {
		return Record{}, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal record: " + err.Error(),
		}
	}
	return database.Update(collection, id, string(jsonData))
}

// DatabaseDelete removes a record from a collection by ID.
func DatabaseDelete(collection, id string) error {
	if wafer.HasCallBlock() {
		return DatabaseDeleteCtx(wafer.NewContext(), collection, id)
	}
	return database.Delete(collection, id)
}

// DatabaseCount returns the number of records matching the given filters.
func DatabaseCount(collection string, filters []Filter) (int64, error) {
	if wafer.HasCallBlock() {
		return DatabaseCountCtx(wafer.NewContext(), collection, filters)
	}
	return database.Count(collection, filters)
}

// DatabaseGetByField retrieves a single record where field equals value.
func DatabaseGetByField(collection, field, value string) (Record, error) {
	if wafer.HasCallBlock() {
		return DatabaseGetByFieldCtx(wafer.NewContext(), collection, field, value)
	}
	rl, err := database.List(collection, ListOptions{
		Filters: []Filter{{
			Field:    field,
			Operator: OpEqual,
			Value:    value,
		}},
		Limit: 1,
	})
	if err != nil {
		return Record{}, err
	}
	if len(rl.Records) == 0 {
		return Record{}, &wafer.WaferError{
			Code:    "not_found",
			Message: "record not found in " + collection + " where " + field + " = " + value,
		}
	}
	return rl.Records[0], nil
}

// DatabaseQueryRaw executes a raw SELECT query and returns records.
func DatabaseQueryRaw(query string, args ...any) ([]Record, error) {
	if wafer.HasCallBlock() {
		return DatabaseQueryRawCtx(wafer.NewContext(), query, args...)
	}
	argsJSON, err := json.Marshal(args)
	if err != nil {
		return nil, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal query args: " + err.Error(),
		}
	}
	return database.QueryRaw(query, string(argsJSON))
}

// DatabaseExecRaw executes a raw non-SELECT statement and returns affected rows.
func DatabaseExecRaw(query string, args ...any) (int64, error) {
	if wafer.HasCallBlock() {
		return DatabaseExecRawCtx(wafer.NewContext(), query, args...)
	}
	argsJSON, err := json.Marshal(args)
	if err != nil {
		return 0, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal query args: " + err.Error(),
		}
	}
	return database.ExecRaw(query, string(argsJSON))
}
