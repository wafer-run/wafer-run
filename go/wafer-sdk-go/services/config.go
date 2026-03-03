package services

import (
	"fmt"

	wafer "github.com/wafer-run/wafer-run/go/wafer-sdk-go"
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/config"
)

// --- CallBlock-based implementations (context-aware) ---

// ConfigGetCtx retrieves a configuration value by key using CallBlock. Returns
// the value and true if the key exists, or empty string and false if not.
func ConfigGetCtx(ctx wafer.Context, key string) (string, bool) {
	msg := wafer.NewMessage("config.get", nil)
	msg.SetMeta("key", key)
	result := ctx.CallBlock("wafer/config", msg)
	if result.Error != nil {
		return "", false
	}
	if result.Response == nil || result.Response.Data == nil {
		return "", false
	}
	return string(result.Response.Data), true
}

// ConfigGetDefaultCtx retrieves a configuration value using CallBlock,
// returning defaultValue if the key does not exist.
func ConfigGetDefaultCtx(ctx wafer.Context, key, defaultValue string) string {
	v, ok := ConfigGetCtx(ctx, key)
	if !ok {
		return defaultValue
	}
	return v
}

// ConfigSetCtx sets a configuration value using CallBlock.
func ConfigSetCtx(ctx wafer.Context, key, value string) error {
	msg := wafer.NewMessage("config.set", []byte(value))
	msg.SetMeta("key", key)
	result := ctx.CallBlock("wafer/config", msg)
	if result.Error != nil {
		return fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return nil
}

// --- Legacy direct-import implementations (backward compatible) ---

// ConfigGet retrieves a configuration value by key. Returns the value and true
// if the key exists, or empty string and false if not.
// When CallBlock is available, it routes through the "wafer/config" block.
func ConfigGet(key string) (string, bool) {
	if wafer.HasCallBlock() {
		return ConfigGetCtx(wafer.NewContext(), key)
	}
	v := config.Get(key)
	if v == nil {
		return "", false
	}
	return *v, true
}

// ConfigGetDefault retrieves a configuration value, returning defaultValue if
// the key does not exist.
func ConfigGetDefault(key, defaultValue string) string {
	if wafer.HasCallBlock() {
		return ConfigGetDefaultCtx(wafer.NewContext(), key, defaultValue)
	}
	v := config.Get(key)
	if v == nil {
		return defaultValue
	}
	return *v
}

// ConfigSet sets a configuration value.
func ConfigSet(key, value string) {
	if wafer.HasCallBlock() {
		_ = ConfigSetCtx(wafer.NewContext(), key, value)
		return
	}
	config.Set(key, value)
}
