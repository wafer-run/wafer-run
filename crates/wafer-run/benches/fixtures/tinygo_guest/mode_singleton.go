//go:build singleton

package main

// Pooled-arm variant (PERF-01 Part B): declare a state-retaining
// InstanceMode so WasmiBlock's warm instance pool engages — this is the arm
// that amortizes TinyGo's per-call `_start`. Built by
// scripts/build-fixtures.sh with `tinygo build -tags singleton`.
const instanceMode = "Singleton"
