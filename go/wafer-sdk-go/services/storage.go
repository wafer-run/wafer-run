package services

import (
	"encoding/json"
	"fmt"
	"strconv"

	wafer "github.com/wafer-run/wafer-run/go/wafer-sdk-go"
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/storage"
)

// ObjectInfo is a convenience alias for the WIT-generated ObjectInfo.
type ObjectInfo = storage.ObjectInfo

// ObjectList is a convenience alias for the WIT-generated ObjectList.
type ObjectList = storage.ObjectList

// storageGetResponse is used to unmarshal the CallBlock response for storage.get.
type storageGetResponse struct {
	Data       []byte     `json:"data"`
	ObjectInfo ObjectInfo `json:"object_info"`
}

// --- CallBlock-based implementations (context-aware) ---

// StoragePutCtx stores content in a folder under the given key using CallBlock.
func StoragePutCtx(ctx wafer.Context, folder, key string, data []byte, contentType string) error {
	msg := wafer.NewMessage("storage.put", data)
	msg.SetMeta("folder", folder)
	msg.SetMeta("key", key)
	msg.SetMeta("content_type", contentType)
	result := ctx.CallBlock("@wafer/storage", msg)
	if result.Error != nil {
		return fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return nil
}

// StorageGetCtx retrieves content from a folder by key using CallBlock.
// Returns the data and object metadata.
func StorageGetCtx(ctx wafer.Context, folder, key string) ([]byte, ObjectInfo, error) {
	msg := wafer.NewMessage("storage.get", nil)
	msg.SetMeta("folder", folder)
	msg.SetMeta("key", key)
	result := ctx.CallBlock("@wafer/storage", msg)
	if result.Error != nil {
		return nil, ObjectInfo{}, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var resp storageGetResponse
	if err := json.Unmarshal(result.Response.Data, &resp); err != nil {
		return nil, ObjectInfo{}, err
	}
	return resp.Data, resp.ObjectInfo, nil
}

// StorageDeleteCtx removes content from a folder by key using CallBlock.
func StorageDeleteCtx(ctx wafer.Context, folder, key string) error {
	msg := wafer.NewMessage("storage.delete", nil)
	msg.SetMeta("folder", folder)
	msg.SetMeta("key", key)
	result := ctx.CallBlock("@wafer/storage", msg)
	if result.Error != nil {
		return fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return nil
}

// StorageListCtx lists objects in a folder matching an optional prefix using
// CallBlock.
func StorageListCtx(ctx wafer.Context, folder, prefix string, limit, offset int64) (ObjectList, error) {
	msg := wafer.NewMessage("storage.list", nil)
	msg.SetMeta("folder", folder)
	msg.SetMeta("prefix", prefix)
	msg.SetMeta("limit", strconv.FormatInt(limit, 10))
	msg.SetMeta("offset", strconv.FormatInt(offset, 10))
	result := ctx.CallBlock("@wafer/storage", msg)
	if result.Error != nil {
		return ObjectList{}, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var ol ObjectList
	if err := json.Unmarshal(result.Response.Data, &ol); err != nil {
		return ObjectList{}, err
	}
	return ol, nil
}

// StorageListAllCtx lists all objects in a folder using CallBlock.
func StorageListAllCtx(ctx wafer.Context, folder string) (ObjectList, error) {
	return StorageListCtx(ctx, folder, "", 0, 0)
}

// --- Legacy direct-import implementations (backward compatible) ---

// StoragePut stores content in a folder under the given key.
// When CallBlock is available, it routes through the "@wafer/storage" block.
func StoragePut(folder, key string, data []byte, contentType string) error {
	if wafer.HasCallBlock() {
		return StoragePutCtx(wafer.NewContext(), folder, key, data, contentType)
	}
	return storage.Put(folder, key, data, contentType)
}

// StorageGet retrieves content from a folder by key. Returns the data and
// object metadata.
func StorageGet(folder, key string) ([]byte, ObjectInfo, error) {
	if wafer.HasCallBlock() {
		return StorageGetCtx(wafer.NewContext(), folder, key)
	}
	return storage.Get(folder, key)
}

// StorageDelete removes content from a folder by key.
func StorageDelete(folder, key string) error {
	if wafer.HasCallBlock() {
		return StorageDeleteCtx(wafer.NewContext(), folder, key)
	}
	return storage.Delete(folder, key)
}

// StorageList lists objects in a folder matching an optional prefix.
func StorageList(folder, prefix string, limit, offset int64) (ObjectList, error) {
	if wafer.HasCallBlock() {
		return StorageListCtx(wafer.NewContext(), folder, prefix, limit, offset)
	}
	return storage.List(folder, prefix, limit, offset)
}

// StorageListAll lists all objects in a folder.
func StorageListAll(folder string) (ObjectList, error) {
	if wafer.HasCallBlock() {
		return StorageListAllCtx(wafer.NewContext(), folder)
	}
	return storage.List(folder, "", 0, 0)
}
