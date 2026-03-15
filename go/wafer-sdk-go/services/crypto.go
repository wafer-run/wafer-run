package services

import (
	"encoding/json"
	"fmt"
	"strconv"

	wafer "github.com/wafer-run/wafer-run/go/wafer-sdk-go"
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/crypto"
)

// --- CallBlock-based implementations (context-aware) ---

// CryptoHashCtx computes a hash of the given password using CallBlock.
func CryptoHashCtx(ctx wafer.Context, password string) (string, error) {
	msg := wafer.NewMessage("crypto.hash", []byte(password))
	result := ctx.CallBlock("wafer-run/crypto", msg)
	if result.Error != nil {
		return "", fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return string(result.Response.Data), nil
}

// CryptoCompareHashCtx compares a plaintext password against a hash using
// CallBlock. Returns nil on match, error on mismatch.
func CryptoCompareHashCtx(ctx wafer.Context, password, hash string) error {
	msg := wafer.NewMessage("crypto.compare_hash", []byte(password))
	msg.SetMeta("hash", hash)
	result := ctx.CallBlock("wafer-run/crypto", msg)
	if result.Error != nil {
		return fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return nil
}

// CryptoSignCtx creates a signed JWT token from claims JSON with the given
// expiry in seconds using CallBlock.
func CryptoSignCtx(ctx wafer.Context, claims string, expirySecs uint64) (string, error) {
	msg := wafer.NewMessage("crypto.sign", []byte(claims))
	msg.SetMeta("expiry_secs", strconv.FormatUint(expirySecs, 10))
	result := ctx.CallBlock("wafer-run/crypto", msg)
	if result.Error != nil {
		return "", fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return string(result.Response.Data), nil
}

// CryptoVerifyCtx verifies a JWT token and returns the claims JSON string
// using CallBlock.
func CryptoVerifyCtx(ctx wafer.Context, token string) (string, error) {
	msg := wafer.NewMessage("crypto.verify", []byte(token))
	result := ctx.CallBlock("wafer-run/crypto", msg)
	if result.Error != nil {
		return "", fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	return string(result.Response.Data), nil
}

// CryptoRandomBytesCtx generates n cryptographically secure random bytes
// using CallBlock.
func CryptoRandomBytesCtx(ctx wafer.Context, n uint32) ([]byte, error) {
	msg := wafer.NewMessage("crypto.random_bytes", nil)
	msg.SetMeta("n", strconv.FormatUint(uint64(n), 10))
	result := ctx.CallBlock("wafer-run/crypto", msg)
	if result.Error != nil {
		return nil, fmt.Errorf("%s: %s", result.Error.Code, result.Error.Message)
	}
	var bytes []byte
	if err := json.Unmarshal(result.Response.Data, &bytes); err != nil {
		// If JSON unmarshal fails, try using raw data (may be raw bytes)
		return result.Response.Data, nil
	}
	return bytes, nil
}

// --- Legacy direct-import implementations (backward compatible) ---

// CryptoHash computes a hash of the given password (e.g., bcrypt).
// When CallBlock is available, it routes through the "wafer-run/crypto" block.
func CryptoHash(password string) (string, error) {
	if wafer.HasCallBlock() {
		return CryptoHashCtx(wafer.NewContext(), password)
	}
	return crypto.Hash(password)
}

// CryptoCompareHash compares a plaintext password against a hash.
// Returns nil on match, error on mismatch.
func CryptoCompareHash(password, hash string) error {
	if wafer.HasCallBlock() {
		return CryptoCompareHashCtx(wafer.NewContext(), password, hash)
	}
	return crypto.CompareHash(password, hash)
}

// CryptoSign creates a signed JWT token from claims JSON with the given
// expiry in seconds.
func CryptoSign(claims string, expirySecs uint64) (string, error) {
	if wafer.HasCallBlock() {
		return CryptoSignCtx(wafer.NewContext(), claims, expirySecs)
	}
	return crypto.Sign(claims, expirySecs)
}

// CryptoVerify verifies a JWT token and returns the claims JSON string.
func CryptoVerify(token string) (string, error) {
	if wafer.HasCallBlock() {
		return CryptoVerifyCtx(wafer.NewContext(), token)
	}
	return crypto.Verify(token)
}

// CryptoRandomBytes generates n cryptographically secure random bytes.
func CryptoRandomBytes(n uint32) ([]byte, error) {
	if wafer.HasCallBlock() {
		return CryptoRandomBytesCtx(wafer.NewContext(), n)
	}
	return crypto.RandomBytes(n)
}
