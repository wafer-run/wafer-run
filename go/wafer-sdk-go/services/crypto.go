package services

import (
	"github.com/wafer-run/wafer-run/go/wafer-sdk-go/gen/wafer/crypto"
)

// CryptoHash computes a hash of the given password (e.g., bcrypt).
func CryptoHash(password string) (string, error) {
	return crypto.Hash(password)
}

// CryptoCompareHash compares a plaintext password against a hash.
// Returns nil on match, error on mismatch.
func CryptoCompareHash(password, hash string) error {
	return crypto.CompareHash(password, hash)
}

// CryptoSign creates a signed JWT token from claims JSON with the given
// expiry in seconds.
func CryptoSign(claims string, expirySecs uint64) (string, error) {
	return crypto.Sign(claims, expirySecs)
}

// CryptoVerify verifies a JWT token and returns the claims JSON string.
func CryptoVerify(token string) (string, error) {
	return crypto.Verify(token)
}

// CryptoRandomBytes generates n cryptographically secure random bytes.
func CryptoRandomBytes(n uint32) ([]byte, error) {
	return crypto.RandomBytes(n)
}
