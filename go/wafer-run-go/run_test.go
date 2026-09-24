package wafer

import (
	"encoding/json"
	"strings"
	"testing"
	"time"
)

// The example/echo guest (examples/wasmi-block), built into
// crates/wafer-run/testdata by scripts/build-fixtures.sh, and the flow the
// wafer-ffi smoke tests run through it.
const (
	echoWasm = "../../crates/wafer-run/testdata/echo_block.wasm"
	echoFlow = "../../crates/wafer-ffi/testdata/echo-flow.json"
	// A flow whose one step names a block nobody registers, so resolving
	// it fails.
	brokenFlow = "../../crates/wafer-ffi/testdata/missing-block-flow.json"
)

// A flow that succeeds comes back through libwafer_ffi and the callback
// plumbing as a Respond Result carrying the block's body.
func TestRunRespondsThroughARegisteredFlow(t *testing.T) {
	w := New()
	defer w.Close()
	if err := w.Register("example/echo", echoWasm); err != nil {
		t.Fatalf("register block: %v", err)
	}
	if err := w.Register("smoke", echoFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}
	if err := w.Start(); err != nil {
		t.Fatalf("start: %v", err)
	}
	defer w.Stop()

	msg := NewMessage("smoke.kind")
	msg.SetMeta("a", "1")
	res := w.Run("smoke", msg)
	if !res.IsRespond() {
		t.Fatalf("expected a respond result, got %+v (error %+v)", res, res.Error)
	}
	var body struct {
		Echo bool   `json:"echo"`
		Kind string `json:"kind"`
	}
	if err := json.Unmarshal([]byte(res.Body), &body); err != nil {
		t.Fatalf("respond body is not the echo guest's JSON: %q: %v", res.Body, err)
	}
	if !body.Echo || body.Kind != "smoke.kind" {
		t.Fatalf("unexpected echo body: %s", res.Body)
	}
}

// Start after a failed Resolve reports that failure again: the runtime never
// finished sealing, so it must not start as if it had.
func TestStartReportsAFailedResolve(t *testing.T) {
	w := New()
	defer w.Close()
	if err := w.Register("broken", brokenFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}
	resolveErr := w.Resolve()
	if resolveErr == nil {
		t.Fatal("resolve must fail: the flow names an unregistered block")
	}
	startErr := w.Start()
	if startErr == nil {
		t.Fatal("start after a failed resolve must fail")
	}
	if startErr.Error() != resolveErr.Error() {
		t.Fatalf("start must re-report the resolve failure: resolve %q, start %q", resolveErr, startErr)
	}
}

// Run dispatches only on a runtime that sealed successfully: before Start,
// and after a failed Resolve, it answers an error naming why.
func TestRunRefusesARuntimeThatDidNotSeal(t *testing.T) {
	w := New()
	defer w.Close()
	if err := w.Register("example/echo", echoWasm); err != nil {
		t.Fatalf("register block: %v", err)
	}
	if err := w.Register("smoke", echoFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}
	res := w.Run("smoke", NewMessage("smoke.kind"))
	if !res.IsError() || !strings.Contains(res.Error.Message, "not sealed") {
		t.Fatalf("run before start must be refused, got %+v (error %+v)", res, res.Error)
	}

	broken := New()
	defer broken.Close()
	if err := broken.Register("broken", brokenFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}
	if err := broken.Resolve(); err == nil {
		t.Fatal("resolve must fail: the flow names an unregistered block")
	}
	res = broken.Run("broken", NewMessage("x"))
	if !res.IsError() || !strings.Contains(res.Error.Message, "failed to seal") {
		t.Fatalf("run after a failed resolve must be refused, got %+v (error %+v)", res, res.Error)
	}
}

// RegisterBlock registers a guest under a capability bound, which then runs;
// invalid capabilities JSON is refused and registers nothing.
func TestRegisterBlockTakesACapabilityBound(t *testing.T) {
	w := New()
	defer w.Close()
	err := w.RegisterBlock("example/echo", echoWasm, `{"collections":"All"}`)
	if err == nil || !strings.HasPrefix(err.Error(), "invalid capabilities JSON:") {
		t.Fatalf("invalid capabilities JSON must be refused, got %v", err)
	}
	if has, err := w.HasBlock("example/echo"); err != nil || has {
		t.Fatalf("a refused registration must register nothing: HasBlock = %v, %v", has, err)
	}

	if err := w.RegisterBlock("example/echo", echoWasm, `{"crypto":true}`); err != nil {
		t.Fatalf("register block: %v", err)
	}
	if err := w.Register("smoke", echoFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}
	if err := w.Start(); err != nil {
		t.Fatalf("start: %v", err)
	}
	defer w.Stop()
	if res := w.Run("smoke", NewMessage("smoke.kind")); !res.IsRespond() {
		t.Fatalf("expected a respond result, got %+v (error %+v)", res, res.Error)
	}
}

// A call the FFI refuses (here a NULL completion callback) returns an error
// instead of waiting forever for a callback that will never fire; the
// runtime is untouched and still resolves afterwards.
func TestARefusedCallReturnsAnErrorInsteadOfWaiting(t *testing.T) {
	w := New()
	defer w.Close()
	if err := w.Register("example/echo", echoWasm); err != nil {
		t.Fatalf("register block: %v", err)
	}
	if err := w.Register("smoke", echoFlow); err != nil {
		t.Fatalf("register flow: %v", err)
	}

	done := make(chan error, 1)
	go func() { done <- w.resolveWith(nil) }()
	select {
	case err := <-done:
		if err == nil || !strings.Contains(err.Error(), "refused") {
			t.Fatalf("a NULL callback must be refused with an error, got %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("resolveWith(nil) is still waiting for a callback that never fires")
	}

	if err := w.Resolve(); err != nil {
		t.Fatalf("the refused call must leave the runtime resolvable: %v", err)
	}
	if err := w.Stop(); err != nil {
		t.Fatalf("stop: %v", err)
	}
}
