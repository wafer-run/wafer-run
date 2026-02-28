package services

import (
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/config"
)

// ConfigGet retrieves a configuration value by key. Returns the value and true
// if the key exists, or empty string and false if not.
func ConfigGet(key string) (string, bool) {
	v := config.Get(key)
	if v == nil {
		return "", false
	}
	return *v, true
}

// ConfigGetDefault retrieves a configuration value, returning defaultValue if
// the key does not exist.
func ConfigGetDefault(key, defaultValue string) string {
	v := config.Get(key)
	if v == nil {
		return defaultValue
	}
	return *v
}

// ConfigSet sets a configuration value.
func ConfigSet(key, value string) {
	config.Set(key, value)
}
