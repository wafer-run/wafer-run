package services

import (
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/storage"
)

// ObjectInfo is a convenience alias for the WIT-generated ObjectInfo.
type ObjectInfo = storage.ObjectInfo

// ObjectList is a convenience alias for the WIT-generated ObjectList.
type ObjectList = storage.ObjectList

// StoragePut stores content in a folder under the given key.
func StoragePut(folder, key string, data []byte, contentType string) error {
	return storage.Put(folder, key, data, contentType)
}

// StorageGet retrieves content from a folder by key. Returns the data and
// object metadata.
func StorageGet(folder, key string) ([]byte, ObjectInfo, error) {
	return storage.Get(folder, key)
}

// StorageDelete removes content from a folder by key.
func StorageDelete(folder, key string) error {
	return storage.Delete(folder, key)
}

// StorageList lists objects in a folder matching an optional prefix.
func StorageList(folder, prefix string, limit, offset int64) (ObjectList, error) {
	return storage.List(folder, prefix, limit, offset)
}

// StorageListAll lists all objects in a folder.
func StorageListAll(folder string) (ObjectList, error) {
	return storage.List(folder, "", 0, 0)
}
