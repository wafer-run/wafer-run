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
 * Resolve all block references in registered chains.
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

/* --- Block Registration -------------------------------------------------- */

/*
 * Register a WASM block from a file path.
 * type_name: block type identifier (e.g. "my-block")
 * wasm_path: filesystem path to the .wasm file
 * Returns NULL on success, or a JSON error string on failure.
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_register_wasm_block(WaferRuntime* w, const char* type_name, const char* wasm_path);

/* --- Chain Management ---------------------------------------------------- */

/*
 * Add a chain definition from JSON.
 * chain_def_json: JSON string matching the ChainDef schema.
 * Returns NULL on success, or a JSON error string on failure.
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_add_chain_def(WaferRuntime* w, const char* chain_def_json);

/* --- Execution ----------------------------------------------------------- */

/*
 * Execute a chain with the given message.
 * chain_id: the chain identifier.
 * message_json: JSON string matching the Message schema:
 *   {"kind": "...", "data": "...", "meta": {"key": "val"}}
 *
 * Returns a JSON result string:
 *   {"action": "continue|respond|drop|error", "response": {...}, "error": {...}}
 *
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_execute(WaferRuntime* w, const char* chain_id, const char* message_json);

/* --- Introspection ------------------------------------------------------- */

/*
 * Get info about all registered chains.
 * Returns a JSON array of ChainInfo objects.
 * Caller must free the returned string with wafer_free_string().
 */
char* wafer_chains_info(WaferRuntime* w);

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
