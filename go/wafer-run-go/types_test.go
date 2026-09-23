package wafer

import (
	"encoding/json"
	"strings"
	"testing"
)

// A Message with no metadata must encode meta as [] — the runtime rejects
// "meta":null — whether it came from NewMessage or a struct literal.
func TestMessageWithoutMetaEncodesEmptyList(t *testing.T) {
	for name, msg := range map[string]*Message{
		"NewMessage": NewMessage("x"),
		"literal":    {Kind: "x"},
	} {
		got, err := json.Marshal(msg)
		if err != nil {
			t.Fatalf("%s: marshal: %v", name, err)
		}
		if want := `{"kind":"x","meta":[]}`; string(got) != want {
			t.Errorf("%s: got %s, want %s", name, got, want)
		}
	}
}

// Wafer.Run hands the encoded Message to the runtime's wafer_run, which
// parses it before looking the flow up. A literal Message without meta must
// get past that parse: the run fails for the unknown flow, not for the JSON.
func TestRunAcceptsLiteralMessageWithoutMeta(t *testing.T) {
	w := New()
	defer w.Close()
	res := w.Run("no-such-flow", &Message{Kind: "x"})
	if !res.IsError() || res.Error == nil {
		t.Fatalf("expected an error result for an unknown flow, got %+v", res)
	}
	if msg := res.Error.Message; strings.HasPrefix(msg, "invalid Message JSON") {
		t.Fatalf("runtime rejected the encoded Message: %s", msg)
	}
}

// Encoding and then decoding keeps entries and their order.
func TestMessageMetaRoundTrips(t *testing.T) {
	msg := NewMessage("x")
	msg.SetMeta("b", "2")
	msg.SetMeta("a", "1")
	raw, err := json.Marshal(msg)
	if err != nil {
		t.Fatal(err)
	}
	var back Message
	if err := json.Unmarshal(raw, &back); err != nil {
		t.Fatal(err)
	}
	if len(back.Meta) != 2 || back.Meta[0].Key != "b" || back.Meta[1].Key != "a" {
		t.Fatalf("round trip lost order: %s -> %+v", raw, back.Meta)
	}
}
