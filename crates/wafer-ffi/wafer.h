/*
 * wafer.h — C header for the WAFER runtime FFI layer.
 *
 * All complex data crosses the FFI boundary as JSON C strings.
 * Caller must free returned strings via wafer_free_string().
 * Functions that can fail return NULL on success, or a JSON error string on failure.
 */

#ifndef WAFER_H
#define WAFER_H

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle to the WAFER runtime. */
typedef struct WaferRuntime WaferRuntime;

/* --- Lifecycle ----------------------------------------------------------- */

/* Create a new WAFER runtime instance. Returns NULL on allocation failure. */
WaferRuntime* wafer_new(void);

/* Free a WAFER runtime instance. Passing NULL is a no-op. */
void wafer_free(WaferRuntime* w);

/*
 * Resolve all block references in registered flows.
 * Returns NULL on success, or a JSON error string on failure.
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_resolve(WaferRuntime* w);

/*
 * Start the runtime. Calls resolve() if not already resolved.
 * Returns NULL on success, or a JSON error string on failure.
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_start(WaferRuntime* w);

/* Stop the runtime and shut down all block instances. */
void wafer_stop(WaferRuntime* w);

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
 * Run a flow with the given message.
 * flow_id: the flow identifier.
 * message_json: JSON string matching the Message schema:
 *   {"kind": "...", "data": "...", "meta": {"key": "val"}}
 *
 * Returns a JSON result string:
 *   {"action": "continue|respond|drop|error", "response": {...}, "error": {...}}
 *
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_run(WaferRuntime* w, const char* flow_id, const char* message_json);

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

/* Free a string previously returned by any wafer_* function. */
void wafer_free_string(char* s);

#ifdef __cplusplus
}
#endif

#endif /* WAFER_H */
