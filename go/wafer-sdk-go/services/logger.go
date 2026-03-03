package services

import (
	"encoding/json"

	wafer "github.com/wafer-run/wafer-run/go/wafer-sdk-go"
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/logger"
)

// LogField is a convenience alias for the WIT-generated LogField.
type LogField = logger.LogField

// --- CallBlock-based implementations (context-aware) ---

// logWithCallBlock sends a log message through CallBlock.
func logWithCallBlock(ctx wafer.Context, level, text string, fields []LogField) {
	var fieldsData []byte
	if len(fields) > 0 {
		fieldsData, _ = json.Marshal(fields)
	}
	msg := wafer.NewMessage("logger."+level, fieldsData)
	msg.SetMeta("message", text)
	msg.SetMeta("level", level)
	ctx.CallBlock("wafer/logger", msg)
}

// LogDebugCtx sends a debug-level log message using CallBlock.
func LogDebugCtx(ctx wafer.Context, msg string, fields ...LogField) {
	logWithCallBlock(ctx, "debug", msg, fields)
}

// LogInfoCtx sends an info-level log message using CallBlock.
func LogInfoCtx(ctx wafer.Context, msg string, fields ...LogField) {
	logWithCallBlock(ctx, "info", msg, fields)
}

// LogWarnCtx sends a warning-level log message using CallBlock.
func LogWarnCtx(ctx wafer.Context, msg string, fields ...LogField) {
	logWithCallBlock(ctx, "warn", msg, fields)
}

// LogErrorCtx sends an error-level log message using CallBlock.
func LogErrorCtx(ctx wafer.Context, msg string, fields ...LogField) {
	logWithCallBlock(ctx, "error", msg, fields)
}

// --- Legacy direct-import implementations (backward compatible) ---

// LogDebug sends a debug-level log message.
// When CallBlock is available, it routes through the "wafer/logger" block.
func LogDebug(msg string, fields ...LogField) {
	if wafer.HasCallBlock() {
		LogDebugCtx(wafer.NewContext(), msg, fields...)
		return
	}
	logger.Debug(msg, fields)
}

// LogInfo sends an info-level log message.
func LogInfo(msg string, fields ...LogField) {
	if wafer.HasCallBlock() {
		LogInfoCtx(wafer.NewContext(), msg, fields...)
		return
	}
	logger.Info(msg, fields)
}

// LogWarn sends a warning-level log message.
func LogWarn(msg string, fields ...LogField) {
	if wafer.HasCallBlock() {
		LogWarnCtx(wafer.NewContext(), msg, fields...)
		return
	}
	logger.Warn(msg, fields)
}

// LogError sends an error-level log message.
func LogError(msg string, fields ...LogField) {
	if wafer.HasCallBlock() {
		LogErrorCtx(wafer.NewContext(), msg, fields...)
		return
	}
	logger.Error(msg, fields)
}
