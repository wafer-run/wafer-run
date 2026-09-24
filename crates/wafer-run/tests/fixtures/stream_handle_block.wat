;; Test block for the streaming-ABI host imports' unknown-handle answer.
;;
;; `__wafer_handle` passes a handle the host never issued (999) to
;; `__wafer_host_stream_write_chunk`, `_finish`, `_read_chunk` and
;; `_take_error`, in that order, and answers with a 4-byte body: byte i is
;; 0x40 + the ErrorCode ordinal import i returned as its negative sentinel
;; (`@` = 0/success, `C` = 3/InvalidArgument, `E` = 5/NotFound).
;;
;; Memory layout:
;;   64  : BlockInfo JSON (85 bytes)
;;   256 : BlockResult JSON template (92 bytes); its data array
;;         `[64,64,64,64]` starts at offset 40 (= 296), each entry two
;;         ASCII digits followed by one separator byte.

(module
  (import "wafer" "__wafer_host_stream_write_chunk"
    (func $write_chunk (param i64 i32 i32) (result i32)))
  (import "wafer" "__wafer_host_stream_finish"
    (func $finish (param i64) (result i32)))
  (import "wafer" "__wafer_host_stream_read_chunk"
    (func $read_chunk (param i64) (result i64)))
  (import "wafer" "__wafer_host_stream_take_error"
    (func $take_error (param i64) (result i64)))

  (memory (export "memory") 1)

  (data (i32.const 64) "{\"name\":\"test/stream-handle\",\"version\":\"0.0.1\",\"interface\":\"handler@v1\",\"summary\":\"\"}")
  (data (i32.const 256) "{\"action\":\"Respond\",\"response\":{\"data\":[64,64,64,64],\"meta\":[]},\"error\":null,\"message\":null}")

  (func (export "__wafer_alloc") (param i32) (result i32)
    (i32.const 1024))

  (func (export "__wafer_info") (result i64)
    (i64.or
      (i64.shl (i64.extend_i32_u (i32.const 64)) (i64.const 32))
      (i64.extend_i32_u (i32.const 85))))

  ;; Write 0x40 + (-sentinel) as two ASCII digits at `addr`.
  (func $put (param $addr i32) (param $sentinel i32)
    (local $v i32)
    (local.set $v (i32.sub (i32.const 0x40) (local.get $sentinel)))
    (i32.store8 (local.get $addr)
      (i32.add (i32.const 0x30) (i32.div_u (local.get $v) (i32.const 10))))
    (i32.store8 (i32.add (local.get $addr) (i32.const 1))
      (i32.add (i32.const 0x30) (i32.rem_u (local.get $v) (i32.const 10)))))

  (func (export "__wafer_handle") (param i32 i32) (result i64)
    (call $put (i32.const 296)
      (call $write_chunk (i64.const 999) (i32.const 64) (i32.const 1)))
    (call $put (i32.const 299)
      (call $finish (i64.const 999)))
    (call $put (i32.const 302)
      (i32.wrap_i64 (call $read_chunk (i64.const 999))))
    (call $put (i32.const 305)
      (i32.wrap_i64 (call $take_error (i64.const 999))))
    (i64.or
      (i64.shl (i64.extend_i32_u (i32.const 256)) (i64.const 32))
      (i64.extend_i32_u (i32.const 92))))

  (func (export "__wafer_lifecycle") (param i32 i32) (result i64)
    (i64.const 0)))
