;; Test block that:
;;   - calls __wafer_host_load_asset with id="ffmpeg"
;;   - returns a BlockResult JSON ("Respond") whose inner response body is
;;     {"status":N} where N is the status code returned by the host import.
;;
;; Memory layout:
;;   16  : asset id "ffmpeg" (6 bytes)
;;   64  : BlockInfo JSON (82 bytes)
;;   256 : pre-baked outer BlockResult JSON (123 bytes), with a placeholder
;;         "48" (the two ASCII digits representing the byte value 48 = '0')
;;         at offsets 332..334 — these are patched at runtime based on the
;;         status code the host returns.
;;
;; The outer JSON template is:
;;   {"action":"Respond","response":{"data":[123,34,115,116,97,116,117,115,34,58,48,125],"meta":[]},"error":null,"message":null}
;;   ^                                                                                  ^^
;;   offset 0 in the data section                                        offset 76..78 — patched

(module
  (import "wafer" "__wafer_host_load_asset"
    (func $load_asset (param i32 i32) (result i32)))

  (memory (export "memory") 1)

  ;; asset id
  (data (i32.const 16) "ffmpeg")

  ;; BlockInfo JSON (82 bytes)
  (data (i32.const 64) "{\"name\":\"test/load-asset\",\"version\":\"0.0.1\",\"interface\":\"handler@v1\",\"summary\":\"\"}")

  ;; BlockResult JSON template (123 bytes) — the two-digit number at offsets
  ;; 332..334 (76..78 within this data section) is patched at runtime.
  (data (i32.const 256) "{\"action\":\"Respond\",\"response\":{\"data\":[123,34,115,116,97,116,117,115,34,58,48,125],\"meta\":[]},\"error\":null,\"message\":null}")

  (func (export "__wafer_alloc") (param i32) (result i32)
    ;; Return a scratch pointer beyond our static data layout.
    (i32.const 1024))

  (func (export "__wafer_info") (result i64)
    ;; Pack (offset=64, len=82) into i64.
    (i64.or
      (i64.shl (i64.extend_i32_u (i32.const 64)) (i64.const 32))
      (i64.extend_i32_u (i32.const 82))))

  (func (export "__wafer_handle") (param i32 i32) (result i64)
    (local $status i32)
    (local $is_failed i32)
    (local $digit1 i32)
    (local $digit2 i32)

    ;; Call __wafer_host_load_asset(id_ptr=16, id_len=6) → status
    (local.set $status (call $load_asset (i32.const 16) (i32.const 6)))

    ;; is_failed = (status == 2)
    (local.set $is_failed (i32.eq (local.get $status) (i32.const 2)))

    ;; digit1: '5' (0x35) if failed, else '4' (0x34)
    (local.set $digit1
      (select (i32.const 0x35) (i32.const 0x34) (local.get $is_failed)))

    ;; digit2: '0' (0x30) if failed, else '8' + status → 0x38 + status
    (local.set $digit2
      (select
        (i32.const 0x30)
        (i32.add (i32.const 0x38) (local.get $status))
        (local.get $is_failed)))

    ;; Patch the two status digits in the pre-baked template.
    ;; Template base is at 256; the "48" placeholder starts at offset 76 → 332.
    (i32.store8 (i32.const 332) (local.get $digit1))
    (i32.store8 (i32.const 333) (local.get $digit2))

    ;; Return pack(256, 123).
    (i64.or
      (i64.shl (i64.extend_i32_u (i32.const 256)) (i64.const 32))
      (i64.extend_i32_u (i32.const 123))))

  (func (export "__wafer_lifecycle") (param i32 i32) (result i64)
    ;; Return pack(0, 4) pointing at bytes "null" is not reliable;
    ;; simpler: return an empty buffer by packing (0, 0). The host
    ;; deserializes the 0-byte buffer as a Result<(), WaferError> which fails,
    ;; but lifecycle isn't exercised in these tests.
    (i64.const 0)))
