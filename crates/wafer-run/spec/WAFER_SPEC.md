---
title: WAFER Specification
sidebar_label: Core Specification
sidebar_position: 1
slug: /spec
hide_title: true
---

# WAFER Specification

**W**ired **A**rchitecture **F**or **F**low-**L**inked **E**xecution

Version: 0.0.1-draft

---

## Overview

WAFER is a language-agnostic specification for building block-based processing pipelines. Flows can be standalone applications or embedded as logic components within existing programs. Blocks are pure processors that don't know about each other; the runtime handles all wiring.

This specification defines:
- Block interface contract
- Message and result types
- Flow configuration schema
- Execution semantics

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           WAFER RUNTIME                                 │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   FLOWS (nested tree structure defines message flow)                     │
│                                                                          │
│   flow-a ──→ validate ──→ process ──→ store  (sequential: nested)       │
│                                                                          │
│   flow-b ──→ route ─┬─→ handler-a           (first-match: siblings)    │
│                      ├─→ handler-b                                      │
│                      └─→ fallback                                       │
│                                                                          │
│   Each block can:                                                        │
│   • Continue  - pass message to next block                              │
│   • Respond   - short-circuit, return response                          │
│   • Drop      - end flow silently, no response                          │
│   • Error     - short-circuit with error                                │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Usage Modes

WAFER flows are flexible - use them however fits your architecture:

### Embedded Library

Use flows as composable logic within existing applications:

```
┌─────────────────────────────────────────────────────────────────┐
│                      YOUR APPLICATION                            │
│                                                                  │
│   ┌──────────┐    ┌─────────────────────┐    ┌──────────┐      │
│   │  HTTP    │───→│   WAFER FLOW       │───→│  Your    │      │
│   │  Handler │    │  (validation/auth)  │    │  Logic   │      │
│   └──────────┘    └─────────────────────┘    └──────────┘      │
│                                                                  │
│   Your code calls runtime.Execute(flowID, msg)                  │
└─────────────────────────────────────────────────────────────────┘
```

Use cases:
- Validation pipelines for complex input
- Authorization/permission checking
- Data transformation and enrichment
- Business rules that change frequently
- Plugin systems for user extensibility

### Standalone Application

Build entire applications from flows with connection blocks (implementation-specific):

```
┌─────────────────────────────────────────────────────────────────┐
│                       WAFER RUNTIME                             │
│                                                                  │
│   http ──→ auth ──→ validate ──→ transform ──→ db               │
│                                                                  │
│   mqtt ──→ process ──→ store                                    │
│         └──→ notify                                              │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Hybrid

Mix both - use WAFER for specific subsystems within a larger application.

---

## Core Types

### Message

Messages flow through the flow. A message contains a kind identifier, payload data, and metadata.

```
Message {
    kind: string               // e.g., "user.create", "order.process"
    data: bytes                // Payload (typically JSON)
    meta: map<string, string>  // Headers, trace ID, context, etc.
}
```

The `kind` field identifies what kind of message this is. It's used for conditional routing via `match` patterns in flow configuration.

The `meta` field uses `map<string, string>` intentionally. String-only values keep metadata simple, serialization-safe, and compatible across language boundaries (especially WASM). For structured data, use `data` (the payload). For metadata that needs structure (e.g., a list of scopes), serialize to a string representation (e.g., comma-separated or JSON).

Blocks can read and modify all fields.

### Result

Every block returns a Result that tells the runtime what to do next:

```
Action enum {
    Continue   // Pass message to next block in flow
    Respond    // Short-circuit flow, return response to caller
    Drop       // End flow silently, no response
    Error      // Short-circuit flow with error
}

Result {
    action:   Action
    response: Response?  // Required for Respond action
    error:    Error?     // Required for Error action
}

Response {
    data: bytes
    meta: map<string, string>
}

Error {
    code:    string             // e.g., "invalid_argument", "not_found"
    message: string             // Human readable
    meta:    map<string, string>
}
```

**Recommended error codes** (based on gRPC status codes for interoperability):

| Code | Description |
|------|-------------|
| `invalid_argument` | Invalid input data or validation failure |
| `not_found` | Requested resource does not exist |
| `already_exists` | Resource already exists (conflict) |
| `permission_denied` | No permission for this action |
| `unauthenticated` | Authentication required or invalid |
| `unavailable` | Service temporarily unavailable |
| `deadline_exceeded` | Operation timed out |
| `resource_exhausted` | Rate limit or quota exceeded |
| `failed_precondition` | Operation rejected due to current state |
| `internal` | Internal error (unexpected) |

Custom codes are allowed but should use namespacing: `myapp.custom_error`

**Action semantics:**
- `Continue` - Pass the (possibly modified) message to the next block
- `Respond` - Stop processing, return response to caller immediately
- `Drop` - Stop processing, return nothing (fire-and-forget)
- `Error` - Stop processing, return error to caller immediately

### BlockInfo

Every block declares its identity:

```
BlockInfo {
    name:          string          // e.g., "@example/myblock"
    version:       string          // e.g., "2.1.0" (semver)
    interface:     string          // e.g., "database@v1" (required)
    summary:       string          // Brief description of this implementation
    instance_mode: InstanceMode    // Default instance lifecycle (default: PerNode)
    allowed_modes: []InstanceMode  // Modes this block supports (default: all)
    runtime:       BlockRuntime    // Native or Wasm (default: Native)
    requires:      []string        // Block names this block may call via call_block()
}
```

The `interface` field declares what contract the block implements. Every block MUST implement an interface. The `summary` describes this specific implementation (e.g., "SQLite database using local file storage").

The `instance_mode` and `allowed_modes` fields control block instantiation. See [Instance Modes](#instance-modes) for details.

The `runtime` field indicates whether a block requires native OS access (`Native`) or can run sandboxed as WebAssembly (`Wasm`). Blocks that make external calls (database drivers, filesystem, network sockets) are typically `Native`; blocks containing pure logic or that access services only via `call_block()` are typically `Wasm`.

The `requires` field lists the block names that this block is allowed to call via `call_block()`. If non-empty, the runtime enforces this at call time — any `call_block()` to a block not in the list returns `PERMISSION_DENIED`. If empty, the block may call any registered block.

### InterfaceDefinition

Interfaces define contracts with methods and their input/output schemas using JSON Schema. This enables AI agents and tooling to understand data flow without reading implementation code.

```
InterfaceDefinition {
    name:    string                      // e.g., "database"
    version: string                      // e.g., "1.0.0" (semver)
    summary: string                      // What this interface does
    methods: map<string, MethodDefinition>
}

MethodDefinition {
    summary: string       // What this method does
    input:   JSONSchema   // JSON Schema for input
    output:  JSONSchema   // JSON Schema for output
}
```

**Example interface definition (JSON):**

```json
{
  "name": "database",
  "version": "0.0.1-draft",
  "summary": "Standard database operations for CRUD functionality",
  "methods": {
    "query": {
      "summary": "Query records from a table with optional filtering and pagination",
      "input": {
        "type": "object",
        "properties": {
          "table": { "type": "string", "description": "The table name to query" },
          "where": { "type": "object", "description": "Filter conditions as key-value pairs" },
          "limit": { "type": "number", "description": "Maximum records to return" },
          "offset": { "type": "number", "description": "Records to skip for pagination" }
        },
        "required": ["table"]
      },
      "output": {
        "type": "object",
        "properties": {
          "rows": { "type": "array", "description": "Matching records", "items": { "type": "object" } },
          "count": { "type": "number", "description": "Total count of matching records" }
        },
        "required": ["rows", "count"]
      }
    },
    "insert": {
      "summary": "Insert a new record into a table",
      "input": {
        "type": "object",
        "properties": {
          "table": { "type": "string", "description": "The table name" },
          "data": { "type": "object", "description": "Record data to insert" }
        },
        "required": ["table", "data"]
      },
      "output": {
        "type": "object",
        "properties": {
          "id": { "type": "string", "description": "ID of the inserted record" }
        },
        "required": ["id"]
      }
    }
  }
}
```

Interfaces are stored in separate files (e.g., `interfaces/database@v1.json`) and referenced by blocks.

### Lifecycle Events

```
LifecycleType enum {
    Init   // Block is being initialized, data = config JSON
    Start  // Flow is starting
    Stop   // Flow is stopping
}

LifecycleEvent {
    type: LifecycleType
    data: bytes
}
```

### Context

The runtime provides a Context to blocks for accessing runtime capabilities. Context uses a generic message-based interface for extensibility.

```
Context {
    Send(msg: Message) -> Result    // Send message to runtime capability
    Capabilities() -> []CapabilityInfo  // List available capabilities
    Done() -> channel               // Cancellation signal (implementation-specific)
}

CapabilityInfo {
    kind:    string      // e.g., "log", "config.get", "http.request"
    summary: string      // What this capability does
    input:   JSONSchema  // Expected input format
    output:  JSONSchema  // Expected output format
}
```

**Standard capabilities** (implementations SHOULD support):

| Kind | Description | Input | Output |
|------|-------------|-------|--------|
| `log` | Write log message | `{level, message}` | - |
| `config.get` | Get configuration value | `{key}` | `{value}` |

Implementations MAY add additional capabilities (e.g., `dispatch`, `http.request`, `secret.get`, `cache.get`).

**Why message-based?**
- Extensible without interface changes
- Same pattern works for WASM and native blocks
- AI agents can discover capabilities via `Capabilities()`

### Instance Modes

Blocks can declare their instance lifecycle requirements. This controls how many instances are created and when.

```
InstanceMode enum {
    PerNode       // One instance per flow node (default)
    Singleton     // One instance shared across all flows
    PerFlow       // One instance per flow, shared across nodes
    PerExecution  // New instance for every message
}
```

| Mode | Use Case |
|------|----------|
| `PerNode` | Node-specific config, isolated state per usage |
| `Singleton` | Connection pools, rate limiters, global caches |
| `PerFlow` | Flow-level transaction context |
| `PerExecution` | Complete isolation, stateless processing |

Blocks declare their default mode and allowed modes. Flow configuration can override within allowed modes.

---

## Block Interface

Every block MUST implement this interface:

```
Block {
    Info() -> BlockInfo
    Handle(ctx: Context, msg: Message) -> Result
    Lifecycle(ctx: Context, event: LifecycleEvent) -> error?  // Optional
}
```

The `ctx` parameter provides access to runtime capabilities (logging, config, etc.) via the [Context](#context) interface.

Blocks are pure processors. They receive messages and return results. All external interactions go through Context.

---

## Flow Configuration

Flows define message flow through a nested tree structure.

### Schema

```json
{
  "version": "0.0.1-draft",
  "flows": [
    {
      "id": "string (required, unique identifier)",
      "summary": "string (brief description of what this flow does)",
      "config": {
        "on_error": "stop | continue",
        "timeout": "30s"
      },
      "root": {
        "block": "string (block type identifier)",
        "config": { },
        "next": [ ]
      }
    }
  ]
}
```

### Top-Level Fields

| Field | Description | Default |
|-------|-------------|---------|
| `version` | WAFER spec version this config targets (e.g., `"0.0.1-draft"`) | required |
| `flows` | Array of flow definitions | required |

### Flow Fields

| Field | Description | Default |
|-------|-------------|---------|
| `id` | Unique flow identifier | required |
| `summary` | Brief description of what this flow accomplishes | required |
| `config` | Flow-level configuration (see below) | `{}` |
| `root` | Root node of the flow | required |

### Flow Config Fields

| Field | Description | Default |
|-------|-------------|---------|
| `on_error` | `"stop"` or `"continue"` | `"stop"` |
| `timeout` | Maximum duration for the entire flow execution (e.g., `"30s"`, `"5m"`) | none (no timeout) |

**on_error behavior:**
- `"stop"` - If any block returns Error, stop flow and return error
- `"continue"` - Log error, continue to next block

**timeout behavior:**
- When a flow exceeds its timeout, the runtime cancels the context (signals `Done()`) and returns an Error result with code `deadline_exceeded`
- Blocks SHOULD check `ctx.Done()` during long-running operations and return early when cancelled

### Node Fields

| Field | Description |
|-------|-------------|
| `block` | Block type identifier |
| `flow` | Reference another flow by ID (alternative to `block`) |
| `match` | Pattern to match against `message.kind` (optional) |
| `config` | Per-instance configuration (see below) |
| `instance` | Instance mode override (see [Instance Modes](#instance-modes)) |
| `next` | Array of child nodes |

The `instance` field overrides the block's default instance mode.

### Match Patterns

The `match` field uses glob-style patterns against `message.kind`:

| Pattern | Matches |
|---------|---------|
| `user.create` | Exact match |
| `user.*` | Any type starting with `user.` (single segment) |
| `*` | Anything |
| *(omitted)* | Always matches (unconditional) |

**Limitations:** Match patterns support single-segment wildcards only. Multi-segment wildcards (e.g., `user.**.created`), negation, and regex are not supported in this version. For complex routing logic, use a dedicated router block.

### Config Fields

The `config` object is passed to the block's `Lifecycle(Init)`. It also supports reserved fields:

| Field | Description | Default |
|-------|-------------|---------|
| `timeout` | Maximum duration for this node's execution (e.g., `"5s"`) | inherited from flow |
| `*` | All other fields passed to block | - |

Blocks receive the full config object, including reserved fields (they can ignore them).

**Reserved field names:** Block implementations MUST NOT use `timeout` as a custom configuration key. It is reserved by the runtime. Future spec versions may add additional reserved fields, which will always be documented here.

### Example

```json
{
  "flows": [
    {
      "id": "user-operations",
      "summary": "Handles all user CRUD operations with authentication and validation",
      "config": { "on_error": "stop" },
      "root": {
        "block": "auth",
        "next": [
          {
            "match": "user.create",
            "block": "validate",
            "next": [
              { "block": "store" },
              { "block": "email-welcome" }
            ]
          },
          {
            "match": "user.delete",
            "block": "soft-delete"
          },
          {
            "match": "user.*",
            "block": "generic-handler"
          },
          {
            "block": "fallback"
          }
        ]
      }
    }
  ]
}
```

In this example:
1. All messages go through `auth` first
2. Siblings in the `next` array are evaluated in order; the first matching node wins
3. `user.create` → validate → store → email-welcome (sequential)
4. `user.delete` → soft-delete
5. Any other `user.*` → generic-handler
6. Anything else → fallback (no `match` = always matches)

---

## Execution Semantics

### Sequential Execution (Nested)

Blocks at different depths run in sequence:

```json
{ "block": "a", "next": [{ "block": "b", "next": [{ "block": "c" }] }] }
```
```
a → b → c
```

### First-Match Routing (Siblings)

Multiple items in the same `next` array are evaluated in order. The first matching node executes; remaining siblings are skipped.

```json
{
  "block": "router",
  "next": [
    { "match": "user.create", "block": "create-handler" },
    { "match": "user.*", "block": "generic-handler" },
    { "block": "fallback" }
  ]
}
```

**Evaluation rules:**
1. Evaluate siblings in order (top to bottom)
2. Skip nodes whose `match` pattern doesn't match `message.kind`
3. Execute the first matching node
4. Stop — remaining siblings are not evaluated

A node with no `match` field always matches, making it useful as a fallback at the end of a `next` array. The message is passed by reference to the matched node (no copying).

### Flow References

A node can reference another flow instead of a block:

```json
{
  "flows": [
    {
      "id": "auth-flow",
      "summary": "Validates input and verifies JWT token",
      "config": { "on_error": "stop" },
      "root": {
        "block": "validate",
        "next": [{ "block": "jwt-verify" }]
      }
    },
    {
      "id": "create-user",
      "summary": "Creates a user after authentication",
      "config": { "on_error": "stop" },
      "root": {
        "flow": "auth-flow",
        "next": [{ "block": "db" }]
      }
    }
  ]
}
```

The referenced flow executes as if inlined. If it returns `Respond`, `Drop`, or `Error`, the parent flow short-circuits.

The `match` field on the reference node is evaluated before the referenced flow executes. The `next` field on the reference node defines what happens after the referenced flow completes with `Continue`.

### Message Handling

- Messages are **passed by reference** in sequential flow (modifications carry forward)

### End-of-Flow Behavior

When a block returns `Continue` but there are no more blocks in the flow:

1. The runtime returns the `Continue` result to the caller
2. The message (with any modifications made by blocks) is available in the result
3. This is the expected "success" path for flows that process and transform data

**Caller responsibilities:**
- The caller interprets what `Continue` at end-of-flow means for their use case
- For embedded use: the caller proceeds with application logic
- For connection blocks: the connection block decides the appropriate response (e.g., HTTP 200)

```
Execute("my-flow", msg) -> Result{Action: Continue}

if result.Action == Continue {
    // Flow completed successfully
    // Use msg.Data for further processing
}
```

### Lifecycle Handling

1. `Init` is called when the flow loads, with the node's `config` as data
2. `Start` is called before the first message
3. `Stop` is called when the flow is shutting down

If `Lifecycle(Init)` returns an error, the flow MUST NOT start.

---

## Conformance

### Runtime Requirements

A WAFER-compliant runtime MUST:

1. Load and instantiate blocks according to their `instance_mode`
2. Parse flow configurations
3. Execute flows by ID: `Execute(flowID, message) -> Result`
4. Provide Context to blocks with at least `log` and `config.get` capabilities
5. Support all four actions: Continue, Respond, Drop, Error
6. Support sequential (nested) and first-match (sibling) execution
7. Call Lifecycle events in order: Init → Start → (handle messages) → Stop
8. Respect `on_error` configuration
9. Enforce `timeout` on flows and nodes, cancelling context and returning `deadline_exceeded` error when exceeded

### Block Requirements

A WAFER-compliant block MUST:

1. Implement `Info()` returning valid BlockInfo
2. Implement `Handle(ctx, msg)` returning valid Result
3. Optionally implement `Lifecycle(ctx, event)`
4. Be thread-safe if `allowed_modes` includes `Singleton` or `PerFlow`

---

## Implementation Notes

This specification is intentionally minimal. Implementations typically add:

- **Block loading** - How blocks are discovered, compiled, or loaded (local files, packages, WASM, etc.)
- **Connection blocks** - Blocks that initiate message flow (HTTP servers, message queues, cron, etc.)
- **Additional Context capabilities** - HTTP requests, secrets, caching, etc. beyond the required `log` and `config.get`
- **Type validation** - Schema validation for message data
- **Observability** - Tracing, metrics, logging integration

See implementation-specific documentation (e.g., WAFER-Go) for these features.

---

## License

This specification is released under the MIT License.
