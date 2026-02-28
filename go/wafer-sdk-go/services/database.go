// Package services provides ergonomic client wrappers for WAFER runtime
// capabilities. Each function delegates to WIT-generated host imports in gen/wafer/*.
package services

import (
	"encoding/json"

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

// DatabaseGet retrieves a single record by collection and ID.
func DatabaseGet(collection, id string) (Record, error) {
	return database.Get(collection, id)
}

// DatabaseGetInto retrieves a single record and unmarshals its JSON data field
// into the provided value.
func DatabaseGetInto(collection, id string, v any) error {
	rec, err := database.Get(collection, id)
	if err != nil {
		return err
	}
	return json.Unmarshal([]byte(rec.Data), v)
}

// DatabaseList retrieves records from a collection with the given options.
func DatabaseList(collection string, opts ListOptions) (RecordList, error) {
	return database.List(collection, opts)
}

// DatabaseListAll retrieves all records from a collection with no filters.
func DatabaseListAll(collection string) (RecordList, error) {
	return database.List(collection, ListOptions{})
}

// DatabaseCreate inserts a new record. The data argument is JSON-encoded and
// sent as the record's data field.
func DatabaseCreate(collection string, data any) (Record, error) {
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
	return database.Delete(collection, id)
}

// DatabaseCount returns the number of records matching the given filters.
func DatabaseCount(collection string, filters []Filter) (int64, error) {
	return database.Count(collection, filters)
}

// DatabaseGetByField retrieves a single record where field equals value.
func DatabaseGetByField(collection, field, value string) (Record, error) {
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
	argsJSON, err := json.Marshal(args)
	if err != nil {
		return 0, &wafer.WaferError{
			Code:    "internal",
			Message: "failed to marshal query args: " + err.Error(),
		}
	}
	return database.ExecRaw(query, string(argsJSON))
}
