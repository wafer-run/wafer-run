package services

import (
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/logger"
)

// LogField is a convenience alias for the WIT-generated LogField.
type LogField = logger.LogField

// LogDebug sends a debug-level log message.
func LogDebug(msg string, fields ...LogField) {
	logger.Debug(msg, fields)
}

// LogInfo sends an info-level log message.
func LogInfo(msg string, fields ...LogField) {
	logger.Info(msg, fields)
}

// LogWarn sends a warning-level log message.
func LogWarn(msg string, fields ...LogField) {
	logger.Warn(msg, fields)
}

// LogError sends an error-level log message.
func LogError(msg string, fields ...LogField) {
	logger.Error(msg, fields)
}
