package wafer

import (
	"encoding/json"
	"testing"
)

// The example/echo guest (examples/wasmi-block), built into
// crates/wafer-run/testdata by scripts/build-fixtures.sh, and the flow the
// wafer-ffi smoke tests run through it.
const (
	echoWasm = "../../crates/wafer-run/testdata/echo_block.wasm"
	echoFlow = "../../crates/wafer-ffi/testdata/echo-flow.json"
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
