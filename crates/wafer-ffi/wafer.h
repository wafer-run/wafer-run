/*
 * wafer.h — C header for the WAFER runtime FFI layer.
 *
 * All complex data crosses the FFI boundary as JSON C strings.
 *
 * Operations split into two flavours:
 *
 *   Synchronous (return-value):
 *     wafer_new, wafer_free, wafer_register, wafer_flows_info,
 *     wafer_has_block, wafer_free_string
 *
 *     Strings returned by these must be freed via wafer_free_string().
 *     Functions that can fail return NULL on success, or a JSON error
 *     string on failure.
 *
 *   Asynchronous (callback-based):
 *     wafer_resolve, wafer_start, wafer_stop, wafer_run
 *
 *     These spawn work on the FFI's internal tokio runtime and return
 *     immediately. The supplied wafer_done_cb is invoked when the work
 *     completes, possibly from a tokio worker thread. The `result`
 *     pointer passed to the callback is owned by the FFI and freed
 *     after the callback returns — copy any data you need before
 *     returning. For lifecycle ops the callback's result is NULL on
 *     success; for wafer_run the callback's result is always non-NULL.
 */

#ifndef WAFER_H
#define WAFER_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle to the WAFER runtime. */
typedef struct WaferRuntime WaferRuntime;

/*
 * Completion callback for async ops.
 *
 *   result    — for lifecycle ops: NULL on success, JSON error string on
 *               failure. For wafer_run: always non-NULL, JSON result string.
 *               Pointer is owned by the FFI; freed after the callback
 *               returns. Copy what you need before returning.
 *   user_data — opaque payload passed through unchanged from the call site.
 */
typedef void (*wafer_done_cb)(const char* result, void* user_data);

/* --- Lifecycle ----------------------------------------------------------- */

/* Create a new WAFER runtime instance. Returns NULL on allocation failure. */
WaferRuntime* wafer_new(void);

/*
 * Free a WAFER runtime instance. Passing NULL is a no-op.
 *
 * The caller must first call wafer_stop and wait for its callback to fire
 * before calling wafer_free; otherwise block lifecycle(Stop) handlers will
 * not run. wafer_free's drop of the internal tokio runtime waits for any
 * in-flight spawned tasks to complete.
 */
void wafer_free(WaferRuntime* w);

/*
 * Resolve all block references in registered flows (async).
 *
 * Returns immediately. Invokes `cb` with NULL on success, or a JSON error
 * string on failure.
 */
void wafer_resolve(WaferRuntime* w, wafer_done_cb cb, void* user_data);

/*
 * Start the runtime without spawning block listeners (async).
 *
 * Returns immediately. Invokes `cb` with NULL on success, or a JSON error
 * string on failure.
 */
void wafer_start(WaferRuntime* w, wafer_done_cb cb, void* user_data);

/*
 * Stop the runtime and shut down all block instances (async).
 *
 * Returns immediately. Invokes `cb` with NULL when shutdown finishes.
 * Must be awaited before wafer_free so that block lifecycle(Stop)
 * handlers run.
 */
void wafer_stop(WaferRuntime* w, wafer_done_cb cb, void* user_data);

/* --- Registration -------------------------------------------------------- */

/*
 * Register a block or flow definition from a file path.
 * If path ends with .wasm, registers a WASM block with the given name.
 * Otherwise, reads the file as a JSON flow definition.
 * name: identifier (block type name for .wasm, ignored for flow defs)
 * path: filesystem path to the .wasm or .json file
 * Returns NULL on success, or a JSON error string on failure.
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_register(WaferRuntime* w, const char* name, const char* path);

/* --- Execution ----------------------------------------------------------- */

/*
 * Run a flow with the given message (async).
 *
 *   flow_id      — the flow identifier
 *   message_json — JSON string matching the Message schema:
 *                  {"kind": "...", "data": "...", "meta": {"key": "val"}}
 *
 * Returns immediately. Invokes `cb` with a JSON result string of the form
 *   {"action": "respond|drop|error|continue", ...}
 * The result pointer is freed by the FFI after the callback returns.
 */
void wafer_run(WaferRuntime* w,
               const char* flow_id,
               const char* message_json,
               wafer_done_cb cb,
               void* user_data);

/* --- Introspection ------------------------------------------------------- */

/*
 * Get info about all registered flows.
 * Returns a JSON array of FlowInfo objects.
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_flows_info(WaferRuntime* w);

/*
 * Check whether a block type is registered.
 * Returns 1 if registered, 0 if not.
 */
int wafer_has_block(WaferRuntime* w, const char* type_name);

/* --- Memory -------------------------------------------------------------- */

/*
 * Free a string previously returned by a synchronous wafer_* function.
 * Async callbacks receive FFI-owned strings that are freed automatically
 * when the callback returns; do not pass those pointers to this function.
 */
void wafer_free_string(char* s);

#ifdef __cplusplus
}
#endif

#endif /* WAFER_H */
