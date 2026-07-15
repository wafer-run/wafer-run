//go:build !singleton

package main

// instanceMode is the declared InstanceMode carried in infoJSON. The default
// build keeps the guest cold ("PerNode" — fresh instance + _start per call,
// the baseline the cold/warm bench arms measure). Selected via build tags
// (not -ldflags -X: TinyGo's compile-time interp folds the infoJSON
// initializer before link-time overrides apply, so -X silently no-ops here).
const instanceMode = "PerNode"
