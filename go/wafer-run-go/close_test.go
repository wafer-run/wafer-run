package wafer

import (
	"errors"
	"strings"
	"sync"
	"testing"
	"time"
)

// startEcho returns a started Wafer running the smoke flow over the echo
// guest.
func startEcho(t *testing.T) *Wafer {
	t.Helper()
	w := New()
	if err := w.Register("example/echo", echoWasm); err != nil {
		t.Fatalf("register block: %v", err)
	}
	if err := w.Register("smoke", echoFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}
	if err := w.Start(); err != nil {
		t.Fatalf("start: %v", err)
	}
	return w
}

// Close does not free the runtime under a Run that is using it: it waits,
// the Run completes, and calls after Close are refused with ErrClosed. The
// Run is held inside its FFI call until Close has started, which forces the
// interleaving where a Close that did not wait would free the pointer the
// Run is about to pass to wafer_run.
func TestCloseWaitsForARunUsingTheRuntime(t *testing.T) {
	w := startEcho(t)

	entered := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	testHookFFICall = func() {
		once.Do(func() {
			close(entered)
			<-release
		})
	}
	defer func() { testHookFFICall = nil }()

	runDone := make(chan *Result, 1)
	go func() { runDone <- w.Run("smoke", NewMessage("smoke.kind")) }()
	<-entered

	closeDone := make(chan error, 1)
	go func() { closeDone <- w.Close() }()
	select {
	case err := <-closeDone:
		close(release)
		t.Fatalf("Close returned (%v) while a Run was still using the runtime", err)
	case <-time.After(300 * time.Millisecond):
	}
	close(release)

	select {
	case res := <-runDone:
		if !res.IsRespond() {
			t.Fatalf("the Run Close waited for must complete, got %+v (error %+v)", res, res.Error)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the Run never returned")
	}
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("close: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Close never returned")
	}

	if res := w.Run("smoke", NewMessage("smoke.kind")); !res.IsError() || !strings.Contains(res.Error.Message, ErrClosed.Error()) {
		t.Fatalf("a Run after Close must be refused, got %+v (error %+v)", res, res.Error)
	}
	if _, err := w.HasBlock("example/echo"); !errors.Is(err, ErrClosed) {
		t.Fatalf("HasBlock after Close: got %v, want ErrClosed", err)
	}
	if err := w.Close(); err != nil {
		t.Fatalf("a second Close: %v", err)
	}
}

// Stop refuses Runs from then on; the Run's error says why.
func TestRunAfterStopIsRefused(t *testing.T) {
	w := startEcho(t)
	defer w.Close()
	if err := w.Stop(); err != nil {
		t.Fatalf("stop: %v", err)
	}
	res := w.Run("smoke", NewMessage("smoke.kind"))
	if !res.IsError() || res.Error.Code != "Unavailable" {
		t.Fatalf("a Run after Stop must be refused, got %+v (error %+v)", res, res.Error)
	}
}

// HasBlock and FlowsInfo return an error when the FFI call crashes, instead
// of reading wafer_has_block's -1 as "registered" and the error object as
// "no flows". They crash here because they are called on a thread of the
// FFI's tokio runtime (inside a completion callback), where the FFI cannot
// block to take its lock.
func TestIntrospectionCrashesAreErrors(t *testing.T) {
	w := New()
	defer w.Close()
	if err := w.Register("example/echo", echoWasm); err != nil {
		t.Fatalf("register block: %v", err)
	}
	if err := w.Register("smoke", echoFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}

	var has bool
	var hasErr, flowsErr error
	var flows []FlowInfo
	var once sync.Once
	testHookDoneCallback = func() {
		once.Do(func() {
			has, hasErr = w.HasBlock("example/echo")
			flows, flowsErr = w.FlowsInfo()
		})
	}
	err := w.Resolve()
	testHookDoneCallback = nil
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}

	if hasErr == nil || has {
		t.Fatalf("HasBlock on a crashed call must return an error, got %v, %v", has, hasErr)
	}
	if flowsErr == nil || !strings.Contains(flowsErr.Error(), "panic in wafer_flows_info") {
		t.Fatalf("FlowsInfo on a crashed call must return the FFI's error, got %v, %v", flows, flowsErr)
	}

	// Called from the Go side, both answer.
	if has, err := w.HasBlock("example/echo"); err != nil || !has {
		t.Fatalf("HasBlock = %v, %v; want true, nil", has, err)
	}
	if flows, err := w.FlowsInfo(); err != nil || len(flows) != 1 || flows[0].ID != "smoke" {
		t.Fatalf("FlowsInfo = %+v, %v; want the smoke flow", flows, err)
	}
}
