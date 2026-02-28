---
title: WAFER-Go
sidebar_label: Go Implementation
sidebar_position: 2
slug: /go
hide_title: true
---

# WAFER-Go

Go implementation of the [WAFER specification](./WAFER_SPEC.md).

---

## Overview

WAFER-Go is the runtime layer that sits between the WAFER spec and applications built on it:

```
┌─────────────────────────────────────────────┐
│           APPLICATION LAYER                 │
│  (e.g., Solobase — see WAFER_SOLOBASE.md)  │
├─────────────────────────────────────────────┤
│              WAFER-GO                      │
│    Runtime, SDK, WASM Loader, CLI           │
├─────────────────────────────────────────────┤
│             WAFER SPEC                     │
│   Blocks, Chains, Interfaces, Registry      │
└─────────────────────────────────────────────┘
```

WAFER-Go provides:
- Go SDK for writing blocks
- Runtime for loading and executing chains
- WASM block loader using wazero
- CLI tools for development

For a real-world example of an application built on WAFER-Go, see Solobase — a BaaS (Backend as a Service) where every feature is a block with optional standalone UI.

---

## Installation

```bash
go get github.com/suppers-ai/wafer-go
```

CLI (optional):
```bash
go install github.com/suppers-ai/wafer-go/cmd/wafer@latest
```

---

## Embedding in Existing Applications

WAFER doesn't require building a full application from scratch. You can embed chains as reusable logic pipelines within your existing codebase.

### Use Cases

- **Validation pipelines** - Chain validators for complex input processing
- **Authorization flows** - Compose auth checks without nested if-statements
- **Data transformation** - Build configurable ETL pipelines
- **Business rules** - Externalize logic that changes frequently
- **Plugin systems** - Let users extend your app with custom blocks

### Basic Embedding

```go
package main

import (
    "net/http"

    "github.com/suppers-ai/wafer-go"
)

func main() {
    // Your existing app setup
    db := connectDatabase()
    cache := setupCache()

    // Set up WAFER chain for specific logic
    wfl := wafer.New()
    wfl.RegisterBlock("validate-user", &UserValidationBlock{})
    wfl.AddChain(wafer.Chain{
        ID:   "user-operations",
        Root: &wafer.Node{Block: "validate-user"},
    })
    wfl.Resolve()

    // Use in your existing HTTP handler
    http.HandleFunc("/users", func(w http.ResponseWriter, r *http.Request) {
        // Your existing code...
        body := readBody(r)

        // Run input through WAFER chain by ID
        msg := &wafer.Message{Kind: "user.create", Data: body}
        result := wfl.Execute("user-operations", msg)

        if result.Action == wafer.Error {
            http.Error(w, result.Error.Message, 400)
            return
        }

        // Continue with your existing logic
        user := createUser(db, body)
        cache.Set(user.ID, user)
        json.NewEncoder(w).Encode(user)
    })

    http.ListenAndServe(":8080", nil)
}
```

### Chain as Middleware

```go
// Use WAFER chain as middleware in any router
func waferMiddleware(wfl *wafer.Wafer, chainID string) func(http.Handler) http.Handler {
    return func(next http.Handler) http.Handler {
        return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
            msg := &wafer.Message{
                Kind: r.Method + ":" + r.URL.Path, // e.g., "POST:/api/users"
                Data: readBody(r),
                Meta: map[string]string{
                    "path":   r.URL.Path,
                    "method": r.Method,
                },
            }

            result := wfl.Execute(chainID, msg)

            switch result.Action {
            case wafer.Error:
                http.Error(w, result.Error.Message, 400)
            case wafer.Respond:
                w.Write(result.Response.Data)
            case wafer.Continue:
                // Pass modified data to next handler
                r = r.WithContext(context.WithValue(r.Context(), "wafer_data", msg.Data))
                next.ServeHTTP(w, r)
            }
        })
    }
}

// Usage with chi router
r := chi.NewRouter()
r.With(waferMiddleware(wfl, "auth-verify")).Post("/api/*", apiHandler)
```

### Programmatic Chain Building

You don't need JSON config files - build chains in code:

```go
w := wafer.New()

// Register blocks
w.RegisterBlock("validate", &ValidateBlock{})
w.RegisterBlock("transform", &TransformBlock{})
w.RegisterBlock("enrich", &EnrichBlock{})

// Build chain programmatically
w.AddChain(wafer.Chain{
    ID:      "process-order",
    Summary: "Validates, transforms, and enriches incoming orders",
    Config:  wafer.ChainConfig{OnError: "stop"},
    Root: &wafer.Node{
        Block:  "validate",
        Config: json.RawMessage(`{"schema": "order"}`),
        Next: []*wafer.Node{
            {
                Block: "transform",
                Next: []*wafer.Node{
                    {Block: "enrich"},
                },
            },
        },
    },
})

w.Start()

// Execute by ID
result := w.Execute("process-order", msg)
```

### Inline Blocks

Define blocks inline without separate files:

```go
w.RegisterBlockFunc("log", func(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    wafer.Log(ctx, "info", string(msg.Data))
    return msg.Continue()
})

w.RegisterBlockFunc("add-timestamp", func(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    msg.SetMeta("timestamp", time.Now().Format(time.RFC3339))
    return msg.Continue()
})
```

### Minimal Chain (No Config File)

```go
// Entire WAFER setup in a few lines
w := wafer.New()

w.RegisterBlockFunc("validate", validateUser)
w.RegisterBlockFunc("normalize", normalizeEmail)

w.AddChain(wafer.Chain{
    ID:      "user-input",
    Summary: "Validates and normalizes user input data",
    Root:    &wafer.Node{Block: "validate", Next: []*wafer.Node{{Block: "normalize"}}},
})

w.Start()

// Execute chain by ID anywhere in your app
result := w.Execute("user-input", &wafer.Message{Kind: "user.create", Data: userData})
```

This approach lets you:
- Add WAFER to specific parts of your app incrementally
- Keep your existing architecture and frameworks
- Use chains only where block composition makes sense
- Mix WAFER logic with regular code freely

### Real-World Example: Solobase

Solobase embeds WAFER-Go to build a block-based BaaS platform. Each feature (auth, database admin, storage, IAM, etc.) is a block with its own backend logic and optional Preact UI page. Chains compose blocks for request processing:

```
HTTP Request → Router
  ├── POST /api/auth/login    → auth-login chain
  ├── GET  /admin/database    → admin-guard → database-ui chain
  └── POST /api/database/*    → admin-guard → database chain
```

See the Solobase documentation for full architecture details.

---

## Types

```go
package wafer

type Message struct {
    Kind string            // e.g., "user.create", "order.process"
    Data []byte            // Payload (typically JSON)
    Meta map[string]string // Headers, trace ID, auth context
}

// Convenience methods on Message
func (m *Message) Unmarshal(v any) error        // Parse Data into struct
func (m *Message) GetMeta(key string) string    // Get Meta value
func (m *Message) SetMeta(key, value string)    // Set Meta value
func (m *Message) SetData(v any) error          // Marshal and set Data

type Action int

const (
    Continue Action = iota  // Pass to next block in chain
    Respond                 // Short-circuit, return response
    Drop                    // End chain silently, no response
    Error                   // Short-circuit with error
)

type Response struct {
    Data []byte
    Meta map[string]string
}

type Error struct {
    Code    string
    Message string
    Meta    map[string]string
}

type Result struct {
    Action   Action
    Response *Response
    Error    *Error
}

// Fluent API on Message
func (m *Message) Continue() Result
func (m *Message) Respond(r Response) Result
func (m *Message) Drop() Result
func (m *Message) Error(e Error) Result

type BlockInfo struct {
    Name         string         // e.g., "@wafer/sqlite"
    Version      string         // e.g., "2.1.0" (semver)
    Interface    string         // e.g., "database@v1" (required)
    Summary      string         // Brief description of this implementation
    InstanceMode InstanceMode   // Default instance lifecycle (default: PerNode)
    AllowedModes []InstanceMode // Modes this block supports (default: all)
}

type InstanceMode int

const (
    PerNode      InstanceMode = iota // One instance per chain node (default)
    Singleton                        // One instance shared across all chains
    PerChain                         // One instance per chain, shared across nodes
    PerExecution                     // New instance for every message
)

type LifecycleType int

const (
    Init  LifecycleType = iota
    Start
    Stop
)

type LifecycleEvent struct {
    Type LifecycleType
    Data []byte
}

// Context provides capabilities to blocks via message passing
// Same interface for both native and WASM blocks
type Context interface {
    // Send a message to a runtime capability (log, config, http, etc.)
    Send(msg *Message) Result

    // Capabilities returns available runtime capabilities (for AI agents)
    Capabilities() []CapabilityInfo

    // Done returns a channel that closes when the context is cancelled
    // Blocks should check this for long-running operations
    Done() <-chan struct{}
}

// CapabilityInfo describes a runtime capability
type CapabilityInfo struct {
    Kind    string          // e.g., "log", "config.get", "http.request"
    Summary string          // What this capability does
    Input   json.RawMessage // JSON Schema for input
    Output  json.RawMessage // JSON Schema for output
}
```

### Block Interface

```go
type Block interface {
    Info() BlockInfo
    Handle(ctx Context, msg *Message) Result
    Lifecycle(ctx Context, event LifecycleEvent) error
}
```

### Context Convenience Wrappers

Optional helpers that use `Send()` under the hood:

```go
// helpers.go

func Log(ctx Context, level, message string) {
    ctx.Send(&Message{
        Kind: "log",
        Meta: map[string]string{"level": level},
        Data: []byte(message),
    })
}

func ConfigGet(ctx Context, key string) (string, bool) {
    result := ctx.Send(&Message{
        Kind: "config.get",
        Meta: map[string]string{"key": key},
    })
    if result.Action == Error {
        return "", false
    }
    return string(result.Response.Data), true
}

// Usage - either style works:
wafer.Log(ctx, "info", "processing user")
ctx.Send(&wafer.Message{Kind: "log", Meta: map[string]string{"level": "info"}, Data: []byte("processing user")})
```

### Interface Definition Types

```go
// InterfaceDefinition defines a contract that blocks implement
// Uses JSON Schema for input/output definitions (AI-agent friendly)
type InterfaceDefinition struct {
    Name    string                       `json:"name"`    // e.g., "database"
    Version string                       `json:"version"` // e.g., "1.0.0" (semver)
    Summary string                       `json:"summary"` // What this interface does
    Methods map[string]MethodDefinition  `json:"methods"`
}

type MethodDefinition struct {
    Summary string          `json:"summary"` // What this method does
    Input   json.RawMessage `json:"input"`   // JSON Schema
    Output  json.RawMessage `json:"output"`  // JSON Schema
}
```

Interfaces are defined in JSON files using standard JSON Schema:

```json
// interfaces/database@v1.json
{
  "name": "database",
  "version": "0.0.1-draft",
  "summary": "Standard database operations for CRUD functionality",
  "methods": {
    "query": {
      "summary": "Query records from a table with optional filtering",
      "input": {
        "type": "object",
        "properties": {
          "table": { "type": "string", "description": "The table name to query" },
          "where": { "type": "object", "description": "Filter conditions" },
          "limit": { "type": "number", "description": "Max records to return" }
        },
        "required": ["table"]
      },
      "output": {
        "type": "object",
        "properties": {
          "rows": { "type": "array", "description": "Matching records" },
          "count": { "type": "number", "description": "Total count" }
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
          "id": { "type": "string", "description": "ID of inserted record" }
        },
        "required": ["id"]
      }
    }
  }
}
```

---

## Runtime Behavior

### Block Instance Model

Block instance lifecycle is configurable. The block declares its default mode and which modes it supports, while the chain config can override within allowed modes.

#### Instance Modes

| Mode | Description | Use Case |
|------|-------------|----------|
| `PerNode` | One instance per chain node (default) | Node-specific config, isolated state per usage |
| `Singleton` | One instance shared across all chains | Connection pools, rate limiters, global caches |
| `PerChain` | One instance per chain, shared across nodes | Chain-level transaction context |
| `PerExecution` | New instance for every message | Complete isolation, stateless processing |

#### Block Declaration

Blocks declare their default mode and which modes are safe to use:

```go
func Info() wafer.BlockInfo {
    return wafer.BlockInfo{
        Name:         "@app/db-pool",
        Version:      "1.0.0",
        Interface:    "database@v1",
        Summary:      "PostgreSQL connection pool",
        InstanceMode: wafer.Singleton,  // Default: share one pool
        AllowedModes: []wafer.InstanceMode{
            wafer.Singleton,
            wafer.PerChain,
        },
    }
}
```

#### Chain Override

Chains can override the instance mode within allowed modes:

```json
{
  "block": "db-pool",
  "instance": "per-chain",
  "config": { "connection_string": "..." }
}
```

The runtime validates that the requested mode is in `AllowedModes`. If not specified, the block's default `InstanceMode` is used. If the block doesn't specify `AllowedModes`, all modes are permitted.

#### Lifecycle Behavior by Mode

| Mode | Init | Start | Stop | Handle |
|------|------|-------|------|--------|
| `Singleton` | Once globally | Once globally | Once globally | Concurrent from all chains |
| `PerNode` | Once per node | Once per node | Once per node | Concurrent for that node |
| `PerChain` | Once per chain | Once per chain | Once per chain | Concurrent within chain |
| `PerExecution` | Every message | N/A | N/A | Sequential (own instance) |

#### Example: Thread-Safe Singleton

```go
// Block instances are reused - state persists between Handle calls
type CacheBlock struct {
    cache map[string][]byte  // Persists across executions
    mu    sync.RWMutex       // Required for thread safety
}

func (b *CacheBlock) Info() wafer.BlockInfo {
    return wafer.BlockInfo{
        Name:         "@app/cache",
        InstanceMode: wafer.Singleton,
        AllowedModes: []wafer.InstanceMode{wafer.Singleton, wafer.PerChain},
        // ...
    }
}
```

#### Example: Stateless Per-Execution

```go
type TransformBlock struct{}

func (b *TransformBlock) Info() wafer.BlockInfo {
    return wafer.BlockInfo{
        Name:         "@app/transform",
        InstanceMode: wafer.PerExecution,  // No shared state
        AllowedModes: []wafer.InstanceMode{wafer.PerExecution},  // Only safe mode
        // ...
    }
}
```

### Thread Safety

Blocks **must be thread-safe** because:

1. **Multiple chains**: The same singleton block may be used in different chains executing concurrently
2. **Concurrent requests**: Multiple HTTP requests may trigger chain execution in parallel

**Requirements for block authors:**

```go
// WRONG - Race condition
var cache map[string][]byte

func Handle(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    cache[key] = value  // Data race!
    return msg.Continue()
}

// CORRECT - Use sync primitives
type CacheBlock struct {
    cache map[string][]byte
    mu    sync.RWMutex
}

func (b *CacheBlock) Handle(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    b.mu.Lock()
    b.cache[key] = value
    b.mu.Unlock()
    return msg.Continue()
}
```

The runtime guarantees:
- `Lifecycle(Init)` completes before any `Handle` calls
- `Lifecycle(Start)` completes before any traffic is accepted
- `Lifecycle(Stop)` waits for in-flight `Handle` calls to complete

### Panic Recovery

The runtime recovers panics in block execution and converts them to `Error` results:

```go
func (w *Wafer) executeNode(node *Node, msg *Message, onError string) (result Result) {
    defer func() {
        if r := recover(); r != nil {
            result = Result{
                Action: Error,
                Error: &Error{
                    Code: "panic",
                    Message:  fmt.Sprintf("block panicked: %v", r),
                    Meta: map[string]string{"stack": string(debug.Stack())},
                },
            }
        }
    }()
    // ... execution logic
}
```

Blocks should still handle errors gracefully rather than panicking.

### Observability

The runtime provides hooks for monitoring and tracing:

```go
type Wafer struct {
    // ... other fields

    // Observability hooks (optional)
    OnBlockStart func(ctx ObservabilityContext)
    OnBlockEnd   func(ctx ObservabilityContext, result Result, duration time.Duration)
    OnChainStart func(chainID string, msg *Message)
    OnChainEnd   func(chainID string, result Result, duration time.Duration)
}

type ObservabilityContext struct {
    ChainID   string
    NodePath  string  // e.g., "root.0.1" for navigation
    BlockName string
    TraceID   string
    Message   *Message
}
```

Example usage:
```go
w := wafer.New()
w.OnBlockEnd = func(ctx wafer.ObservabilityContext, result wafer.Result, d time.Duration) {
    metrics.RecordBlockLatency(ctx.BlockName, d)
    if result.Action == wafer.Error {
        metrics.IncrementBlockErrors(ctx.BlockName)
    }
}
```

---

## Writing Blocks

### Minimal Block

```go
// blocks/validate.go
package main

import "github.com/suppers-ai/wafer-go"

func Info() wafer.BlockInfo {
    return wafer.BlockInfo{
        Name:      "@app/validate",
        Version:   "1.0.0",
        Interface: "validator@v1",
        Summary:   "Validates user registration data. Checks required fields and email format.",
    }
}

func Handle(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    var user User
    if err := msg.Unmarshal(&user); err != nil {
        wafer.Log(ctx, "error", "failed to parse request")
        return msg.Error(wafer.Error{
            Code: "invalid_json",
            Message:  "failed to parse request",
        })
    }

    if user.Email == "" {
        return msg.Error(wafer.Error{
            Code: "validation_error",
            Message:  "email is required",
            Meta: map[string]string{"field": "email"},
        })
    }

    wafer.Log(ctx, "info", "validation passed")
    return msg.Continue()
}
```

### Block with Lifecycle

```go
// blocks/cache.go
package main

import (
    "sync"

    "github.com/suppers-ai/wafer-go"
)

// CacheBlock is a thread-safe in-memory cache.
// Methods are on the struct, so the runtime's BlockFactory can create
// proper instances per the configured instance mode.
type CacheBlock struct {
    cache map[string][]byte
    mu    sync.RWMutex
}

func (b *CacheBlock) Info() wafer.BlockInfo {
    return wafer.BlockInfo{
        Name:         "@app/cache",
        Version:      "1.0.0",
        Interface:    "cache@v1",
        Summary:      "In-memory cache. Returns cached response if exists, otherwise continues.",
        InstanceMode: wafer.Singleton,
        AllowedModes: []wafer.InstanceMode{wafer.Singleton, wafer.PerChain},
    }
}

func (b *CacheBlock) Lifecycle(ctx wafer.Context, event wafer.LifecycleEvent) error {
    switch event.Type {
    case wafer.Init:
        b.mu.Lock()
        b.cache = make(map[string][]byte)
        b.mu.Unlock()
    case wafer.Stop:
        b.mu.Lock()
        b.cache = nil
        b.mu.Unlock()
    }
    return nil
}

func (b *CacheBlock) Handle(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    key := string(msg.Data) // or use a hash

    b.mu.RLock()
    cached, ok := b.cache[key]
    b.mu.RUnlock()

    if ok {
        wafer.Log(ctx, "debug", "cache hit")
        return msg.Respond(wafer.Response{Data: cached})
    }

    wafer.Log(ctx, "debug", "cache miss")
    return msg.Continue()
}
```

### Block with Configuration

```go
// blocks/auth.go
package main

import (
    "encoding/json"
    "time"

    "github.com/suppers-ai/wafer-go"
)

// AuthBlock holds configuration set once during Init.
// Read-only after initialization is thread-safe without mutex.
type AuthBlock struct {
    jwtSecret   string
    tokenExpiry time.Duration
}

func (b *AuthBlock) Info() wafer.BlockInfo {
    return wafer.BlockInfo{
        Name:         "@app/auth",
        Version:      "1.0.0",
        Interface:    "auth@v1",
        Summary:      "JWT authentication. Handles login (issues tokens) and verify (validates tokens).",
        InstanceMode: wafer.PerNode,
    }
}

func (b *AuthBlock) Lifecycle(ctx wafer.Context, event wafer.LifecycleEvent) error {
    if event.Type == wafer.Init {
        var cfg struct {
            JWTSecret   string `json:"jwt_secret"`
            TokenExpiry string `json:"token_expiry"`
        }
        json.Unmarshal(event.Data, &cfg)
        // Safe: written only during Init, read-only after
        b.jwtSecret = cfg.JWTSecret
        b.tokenExpiry, _ = time.ParseDuration(cfg.TokenExpiry)
    }
    return nil
}

func (b *AuthBlock) Handle(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    switch msg.GetMeta("operation") {
    case "login":
        wafer.Log(ctx, "info", "login attempt")
        return handleLogin(ctx, msg)
    case "verify":
        return handleVerify(ctx, msg)
    default:
        return msg.Continue()
    }
}
```

### Block Implementing Interface

```go
// blocks/sqlite.go
package main

import (
    "database/sql"
    "encoding/json"

    "github.com/suppers-ai/wafer-go"
    databasev1 "github.com/suppers-ai/wafer-go/interfaces/database/v1"
    _ "github.com/mattn/go-sqlite3"
)

// SQLiteBlock wraps a database connection pool.
// *sql.DB is thread-safe and designed to be shared.
type SQLiteBlock struct {
    db *sql.DB
}

func (b *SQLiteBlock) Info() wafer.BlockInfo {
    return wafer.BlockInfo{
        Name:         "@wafer/sqlite",
        Version:      "2.1.0",
        Interface:    "database@v1",
        Summary:      "SQLite database using local file storage. Supports query, insert, update, delete.",
        InstanceMode: wafer.Singleton,  // Share connection pool
        AllowedModes: []wafer.InstanceMode{wafer.Singleton, wafer.PerChain},
    }
}

func (b *SQLiteBlock) Lifecycle(ctx wafer.Context, event wafer.LifecycleEvent) error {
    if event.Type == wafer.Init {
        var cfg struct {
            Path string `json:"path"`
        }
        json.Unmarshal(event.Data, &cfg)

        var err error
        b.db, err = sql.Open("sqlite3", cfg.Path)
        return err
    }
    if event.Type == wafer.Stop && b.db != nil {
        return b.db.Close()
    }
    return nil
}

func (b *SQLiteBlock) Handle(ctx wafer.Context, msg *wafer.Message) wafer.Result {
    switch msg.GetMeta("operation") {
    case databasev1.MethodQuery:
        var req databasev1.QueryRequest
        msg.Unmarshal(&req)
        wafer.Log(ctx, "debug", "executing query on "+req.Table)
        rows := executeQuery(req)
        resp, _ := json.Marshal(databasev1.QueryResponse{Rows: rows, Count: len(rows)})
        return msg.Respond(wafer.Response{Data: resp})

    case databasev1.MethodInsert:
        var req databasev1.InsertRequest
        msg.Unmarshal(&req)
        wafer.Log(ctx, "debug", "inserting into "+req.Table)
        id := executeInsert(req)
        resp, _ := json.Marshal(databasev1.InsertResponse{ID: id})
        return msg.Respond(wafer.Response{Data: resp})

    default:
        return msg.Continue()
    }
}
```

---

## Wafer

### Core Types

```go
package wafer

type Wafer struct {
    registry *Registry          // Block type registry (name -> factory)
    chains   map[string]*Chain  // Chain definitions by ID
    resolved map[string]Block   // blockType -> resolved instance (for Stop lifecycle)

    // Observability hooks (optional)
    OnBlockStart func(ctx ObservabilityContext)
    OnBlockEnd   func(ctx ObservabilityContext, result Result, duration time.Duration)
    OnChainStart func(chainID string, msg *Message)
    OnChainEnd   func(chainID string, result Result, duration time.Duration)
}

type Chain struct {
    ID      string
    Summary string          // Brief description of what this chain accomplishes
    Config  ChainConfig     // Chain-level configuration
    Root    *Node
}

type ChainConfig struct {
    OnError string        // "stop" or "continue" (default: "stop")
    Timeout time.Duration // 0 = no timeout
}

type Node struct {
    Block    string           // Block type name (from registry)
    Chain    string           // Chain reference (alternative to Block)
    Match    string           // Pattern to match against message.Kind
    Config   json.RawMessage  // Block-specific config
    Instance *InstanceMode    // Instance mode override (nil = use block default)
    Next     []*Node

    // Resolved at startup by Wafer.Resolve()
    resolvedBlock Block              // Direct reference to block instance
    configMap     map[string]string  // Pre-parsed config for context
}
```

### Resolving Blocks

After all blocks are registered and chains are added, `Resolve()` walks all chain trees and resolves block references to direct instances. This must be called before `Execute`.

```go
func (w *Wafer) Resolve() error {
    for _, chain := range w.chains {
        if err := w.resolveNode(chain.Root); err != nil {
            return fmt.Errorf("chain %q: %w", chain.ID, err)
        }
    }
    return nil
}

func (w *Wafer) resolveNode(node *Node) error {
    // Pre-parse config map for block-specific config
    if node.Config != nil {
        node.configMap = parseConfigMap(node.Config)
    }

    if node.Block != "" {
        // Singleton: one instance per block type, shared across all nodes
        if block, ok := w.resolved[node.Block]; ok {
            node.resolvedBlock = block
        } else {
            factory, ok := w.registry.Get(node.Block)
            if !ok {
                return fmt.Errorf("block type not found: %s", node.Block)
            }
            block := factory.Create(node.Config)
            ctx := &runtimeContext{config: node.configMap}
            if err := block.Lifecycle(ctx, LifecycleEvent{Type: Init, Data: node.Config}); err != nil {
                return fmt.Errorf("init block %q: %w", node.Block, err)
            }
            w.resolved[node.Block] = block
            node.resolvedBlock = block
        }
    }

    for _, child := range node.Next {
        if err := w.resolveNode(child); err != nil {
            return err
        }
    }
    return nil
}
```

Resolution happens once at startup. After resolution, each node holds a direct `resolvedBlock` reference and a pre-parsed `configMap`, so execution has no factory lookups or config parsing overhead.

### Executing Chains

```go
// Execute runs a chain by ID
func (w *Wafer) Execute(chainID string, msg *Message) Result {
    chain, ok := w.chains[chainID]
    if !ok {
        return Result{
            Action: Error,
            Error:  &Error{Code: "chain_not_found", Message: "chain not found: " + chainID},
        }
    }

    return w.executeNode(chain.Root, msg, chainID, chain.Config.OnError, nil, "root")
}

// executeNode runs a single node in the chain tree.
func (w *Wafer) executeNode(node *Node, msg *Message, chainID, onError string, done <-chan struct{}, nodePath string) (result Result) {
    // Panic recovery
    defer func() {
        if r := recover(); r != nil {
            result = Result{
                Action: Error,
                Error: &Error{
                    Code:    "panic",
                    Message: fmt.Sprintf("block panicked: %v", r),
                    Meta:    map[string]string{"stack": string(debug.Stack())},
                },
            }
        }
    }()

    // Handle chain references
    if node.Chain != "" {
        return w.executeChainRef(node, msg, onError, done)
    }

    // Build context for this node (uses pre-parsed configMap)
    ctx := &runtimeContext{
        chainID: chainID,
        nodeID:  nodePath,
        config:  node.configMap,
        done:    done,
    }

    // Execute block directly via resolved reference
    result = node.resolvedBlock.Handle(ctx, msg)

    // Process result
    switch result.Action {
    case Respond, Drop:
        return result

    case Error:
        if onError == "stop" {
            return result
        }
        // on_error=continue: fall through to children
    }

    // Continue: proceed to children
    if len(node.Next) == 0 {
        return result
    }

    return w.executeFirstMatch(node.Next, msg, chainID, onError, done, nodePath)
}

// executeChainRef executes a chain reference node.
func (w *Wafer) executeChainRef(node *Node, msg *Message, onError string, done <-chan struct{}) Result {
    chain, ok := w.chains[node.Chain]
    if !ok {
        return Result{
            Action: Error,
            Error:  &Error{Code: "not_found", Message: "referenced chain not found: " + node.Chain},
        }
    }

    result := w.executeNode(chain.Root, msg, chain.ID, chain.Config.OnError, done, "root")

    // If chain completed with Continue, run our Next nodes
    if result.Action == Continue && len(node.Next) > 0 {
        return w.executeFirstMatch(node.Next, msg, chain.ID, onError, done, "ref:"+node.Chain)
    }

    return result
}

// executeFirstMatch runs the first child node whose Match pattern matches msg.Kind.
func (w *Wafer) executeFirstMatch(nodes []*Node, msg *Message, chainID, onError string, done <-chan struct{}, parentPath string) Result {
    for i, child := range nodes {
        if !matchesPattern(child.Match, msg.Kind) {
            continue
        }
        childPath := fmt.Sprintf("%s.%d", parentPath, i)
        return w.executeNode(child, msg, chainID, onError, done, childPath)
    }
    return Result{Action: Continue, Message: msg}
}

// matchesPattern checks if messageKind matches the pattern
func matchesPattern(pattern, messageKind string) bool {
    // Empty pattern = always matches
    if pattern == "" {
        return true
    }
    // Exact match
    if pattern == messageKind {
        return true
    }
    // Wildcard match: "user.*" matches "user.create", "user.delete"
    if strings.HasSuffix(pattern, ".*") {
        prefix := strings.TrimSuffix(pattern, ".*")
        return strings.HasPrefix(messageKind, prefix+".")
    }
    // Match all
    if pattern == "*" {
        return true
    }
    return false
}
```

---

## WASM Support

### Loading WASM Blocks (wazero)

```go
package wafer

import (
    "context"
    "os"

    "github.com/tetratelabs/wazero"
    "github.com/tetratelabs/wazero/api"
)

type WASMBlock struct {
    runtime wazero.Runtime
    module  api.Module
}

func (w *Wafer) loadWASMBlock(path string) (*WASMBlock, error) {
    ctx := context.Background()
    rt := wazero.NewRuntime(ctx)

    // Register host module with functions matching WIT interface
    hostModule := rt.NewHostModuleBuilder("wafer")

    // host::send - generic capability dispatch
    hostModule.NewFunctionBuilder().
        WithFunc(hostSend).
        Export("send")

    // host::capabilities - list available capabilities
    hostModule.NewFunctionBuilder().
        WithFunc(hostCapabilities).
        Export("capabilities")

    // host::is-cancelled - check context cancellation
    hostModule.NewFunctionBuilder().
        WithFunc(hostIsCancelled).
        Export("is_cancelled")

    if _, err := hostModule.Instantiate(ctx); err != nil {
        return nil, err
    }

    // Load and instantiate WASM module
    wasmBytes, err := os.ReadFile(path)
    if err != nil {
        return nil, err
    }

    module, err := rt.Instantiate(ctx, wasmBytes)
    if err != nil {
        return nil, err
    }

    return &WASMBlock{runtime: rt, module: module}, nil
}

// Host function implementations

func hostSend(ctx context.Context, msgPtr, msgLen uint32) (resultPtr uint32) {
    // 1. Read message from WASM memory
    // 2. Dispatch based on msg.Kind ("log", "config.get", "http.request", etc.)
    // 3. Write result back to WASM memory
    // 4. Return pointer to result
    return handleCapability(ctx, msgPtr, msgLen)
}

func hostCapabilities(ctx context.Context) (listPtr uint32) {
    // Return pointer to list of CapabilityInfo in WASM memory
    return writeCapabilitiesToMemory(ctx)
}

func hostIsCancelled(ctx context.Context) uint32 {
    select {
    case <-ctx.Done():
        return 1
    default:
        return 0
    }
}
```

### Building WASM Blocks

**TinyGo:**
```bash
tinygo build -o block.wasm -target wasi ./block.go
```

**Standard Go (wasip1):**
```bash
GOOS=wasip1 GOARCH=wasm go build -o block.wasm ./block.go
```

---

## WIT Interface (WebAssembly)

WAFER-Go uses WIT (WebAssembly Interface Types) to define the contract between the runtime and WASM blocks.

### Block Interface Definition

```wit
// wafer-block.wit

package wafer:block@0.0.1-draft;

interface types {
    record message {
        kind: string,
        data: list<u8>,
        meta: list<tuple<string, string>>,
    }

    enum action {
        continue,
        respond,
        drop,
        error,
    }

    record response {
        data: list<u8>,
        meta: list<tuple<string, string>>,
    }

    record block-error {
        code: string,
        message: string,
        meta: list<tuple<string, string>>,
    }

    record result {
        action: action,
        response: option<response>,
        error: option<block-error>,
    }

    enum instance-mode {
        per-node,       // One instance per chain node (default)
        singleton,      // One instance shared across all chains
        per-chain,      // One instance per chain
        per-execution,  // New instance for every message
    }

    record block-info {
        name: string,
        version: string,
        interface: string,       // e.g., "database@v1" (required)
        summary: string,         // Brief description of this implementation
        instance-mode: instance-mode,           // Default instance lifecycle
        allowed-modes: list<instance-mode>,     // Modes this block supports
    }

    enum lifecycle-type {
        init,
        start,
        stop,
    }

    record lifecycle-event {
        %type: lifecycle-type,
        data: list<u8>,
    }

    record capability-info {
        kind: string,       // e.g., "log", "config.get"
        summary: string,    // What this capability does
        input: list<u8>,    // JSON Schema
        output: list<u8>,   // JSON Schema
    }
}

// Host interface provides Context.Send() for WASM blocks
// Generic message-based interface - extensible without WIT changes
interface host {
    use types.{message, result, capability-info};

    /// Send a message to a runtime capability (maps to ctx.Send)
    /// Capabilities: "log", "config.get", etc.
    send: func(msg: message) -> result;

    /// Get available capabilities (maps to ctx.Capabilities)
    capabilities: func() -> list<capability-info>;

    /// Check if context is cancelled (maps to ctx.Done)
    /// Returns true if the context has been cancelled
    is-cancelled: func() -> bool;
}

world block {
    import host;

    use types.{message, result, block-info, lifecycle-event};

    /// Returns block identity and interface contract
    export info: func() -> block-info;

    /// Process a message and return result
    /// Use host::send() for Context capabilities
    export handle: func(msg: message) -> result;

    /// Optional lifecycle event handler
    export lifecycle: func(event: lifecycle-event) -> result<_, string>;
}
```

### Generating Bindings

```bash
# Generate Go bindings from WIT
wit-bindgen go --world block ./wafer-block.wit

# Generate Rust bindings
wit-bindgen rust --world block ./wafer-block.wit
```

### WASM Block in Rust (using WIT)

```rust
// src/lib.rs
wit_bindgen::generate!({
    world: "block",
});

struct MyBlock;

// Helper to send log messages via host::send
fn log(level: &str, message: &str) {
    let mut meta = Vec::new();
    meta.push(("level".into(), level.into()));

    host::send(Message {
        kind: "log".into(),
        data: message.as_bytes().to_vec(),
        meta,
    });
}

// Helper to get config via host::send
fn config_get(key: &str) -> Option<String> {
    let mut meta = Vec::new();
    meta.push(("key".into(), key.into()));

    let result = host::send(Message {
        kind: "config.get".into(),
        data: vec![],
        meta,
    });

    match result.action {
        Action::Respond => result.response.map(|r| String::from_utf8_lossy(&r.data).into()),
        _ => None,
    }
}

impl Guest for MyBlock {
    fn info() -> BlockInfo {
        BlockInfo {
            name: "@app/my-block".into(),
            version: "1.0.0".into(),
            interface: "processor@v1".into(),
            summary: "Custom message processor. Transforms and enriches data.".into(),
        }
    }

    fn handle(msg: Message) -> Result_ {
        // Access Context capabilities via host::send
        log("info", "handling message");

        // Read config (equivalent to wafer.ConfigGet in Go)
        if let Some(api_key) = config_get("api_key") {
            log("debug", &format!("using api key: {}...", &api_key[..4]));
        }

        Result_ {
            action: Action::Continue,
            response: None,
            error: None,
        }
    }

    fn lifecycle(event: LifecycleEvent) -> Result<(), String> {
        match event.type_ {
            LifecycleType::Init => {
                log("info", "initializing block");
            }
            LifecycleType::Start => {}
            LifecycleType::Stop => {}
        }
        Ok(())
    }
}

export!(MyBlock);
```

---

## Interfaces

Interfaces are defined as JSON files using JSON Schema. This makes them:
- Language-agnostic
- AI-agent friendly (standard format models understand)
- Easy to validate and generate documentation from

### Loading Interfaces

```go
// Load interface definition from JSON file
func LoadInterface(path string) (*wafer.InterfaceDefinition, error) {
    data, err := os.ReadFile(path)
    if err != nil {
        return nil, err
    }

    var def wafer.InterfaceDefinition
    if err := json.Unmarshal(data, &def); err != nil {
        return nil, err
    }

    return &def, nil
}

// Usage
def, _ := LoadInterface("interfaces/database@v1.json")
fmt.Println(def.Summary) // "Standard database operations..."
```

### Optional: Go Types for Convenience

You can still define Go types for type-safe handling within blocks:

```go
// interfaces/database/v1/types.go
package databasev1

type QueryRequest struct {
    Table   string         `json:"table"`
    Where   map[string]any `json:"where,omitempty"`
    Limit   int            `json:"limit,omitempty"`
    Offset  int            `json:"offset,omitempty"`
}

type QueryResponse struct {
    Rows  []map[string]any `json:"rows"`
    Count int              `json:"count"`
}

type InsertRequest struct {
    Table string         `json:"table"`
    Data  map[string]any `json:"data"`
}

type InsertResponse struct {
    ID string `json:"id"`
}
```

These are used internally by blocks but the source of truth for documentation is the JSON Schema interface file.

---

## CLI

### Commands

```bash
# Run a WAFER application
wafer run [config.json]

# Create a new block
wafer new block my-cache --interface cache@v1

# Create a new block with custom methods
wafer new block my-processor --methods process,validate,transform

# Create a new interface
wafer new interface payments --methods charge,refund,subscribe

# Create a new app from template
wafer new app my-api --template api-with-auth

# List available templates
wafer templates list

# Validate configuration
wafer validate [config.json]

# Show block info
wafer info ./blocks/my-block.go
```

### Generated Block Structure

```
my-cache/
├── block.go          # Block implementation
├── block_test.go     # Test file
├── block.json        # Block manifest for registry
└── README.md         # Usage documentation
```

---

## Directory Structure

```
wafer-go/
├── wafer.go           # Wafer runtime (New, Load, Start, Execute)
├── types.go            # Message, Result, Action, Context, etc.
├── helpers.go          # Convenience wrappers (Log, ConfigGet)
├── config.go           # Config loading
├── wasm.go             # WASM block loader (wazero)
├── registry.go         # Block type registry
│
├── interfaces/
│   ├── interface.go    # Base types
│   ├── database/v1/
│   ├── auth/v1/
│   ├── storage/v1/
│   ├── http/v1/
│   └── custom/v1/
│
├── blocks/             # Official blocks
│   ├── http/
│   ├── router/
│   ├── sqlite/
│   ├── auth/
│   └── logger/
│
├── cmd/
│   └── wafer/
│       └── main.go     # CLI entry point
│
└── wafer.json         # Example configuration
```

---

## Testing Blocks

```go
package main

import (
    "testing"

    "github.com/suppers-ai/wafer-go"
)

// Mock context for testing - implements wafer.Context
type mockContext struct {
    messages []wafer.Message  // Captured Send() calls
    config   map[string]string
    done     chan struct{}
}

func newMockContext() *mockContext {
    return &mockContext{
        config: make(map[string]string),
        done:   make(chan struct{}),
    }
}

func (m *mockContext) Send(msg *wafer.Message) wafer.Result {
    m.messages = append(m.messages, *msg)

    // Handle config.get requests
    if msg.Kind == "config.get" {
        key := msg.Meta["key"]
        if val, ok := m.config[key]; ok {
            return wafer.Result{
                Action:   wafer.Respond,
                Response: &wafer.Response{Data: []byte(val)},
            }
        }
        return wafer.Result{Action: wafer.Error, Error: &wafer.Error{Code: "not_found"}}
    }

    // Log and other capabilities just succeed
    return wafer.Result{Action: wafer.Continue}
}

func (m *mockContext) Capabilities() []wafer.CapabilityInfo {
    return []wafer.CapabilityInfo{
        {Kind: "log", Summary: "Write log message"},
        {Kind: "config.get", Summary: "Get config value"},
    }
}

func (m *mockContext) Done() <-chan struct{} {
    return m.done
}

func TestValidateBlock(t *testing.T) {
    ctx := newMockContext()

    // Initialize block with context
    Lifecycle(ctx, wafer.LifecycleEvent{Type: wafer.Init})

    // Test valid input
    msg := &wafer.Message{
        Kind: "user.create",
        Data: []byte(`{"email": "test@example.com", "password": "12345678"}`),
    }

    result := Handle(ctx, msg)

    if result.Action != wafer.Continue {
        t.Errorf("expected Continue, got %v", result.Action)
    }

    // Verify logging happened
    var logCount int
    for _, m := range ctx.messages {
        if m.Kind == "log" {
            logCount++
        }
    }
    if logCount == 0 {
        t.Error("expected at least one log message")
    }

    // Test invalid input
    msg = &wafer.Message{
        Kind: "user.create",
        Data: []byte(`{"password": "12345678"}`),
    }

    result = Handle(ctx, msg)

    if result.Action != wafer.Error {
        t.Errorf("expected Error, got %v", result.Action)
    }
    if result.Error.Code != "validation_error" {
        t.Errorf("expected validation_error, got %s", result.Error.Code)
    }
}
```

---

## Configuration Example

Chains can also be defined as JSON configuration. Match patterns on `next` nodes handle routing by comparing against `msg.Kind`.

```json
{
  "version": "0.0.1-draft",
  "blocks": [
    {
      "type": "http",
      "source": "github.com/suppers-ai/wafer-go/blocks/http"
    },
    {
      "type": "validate",
      "source": "./blocks/validate.go"
    },
    {
      "type": "auth",
      "source": "github.com/suppers-ai/wafer-go/blocks/auth-jwt@v2.0.0"
    },
    {
      "type": "db",
      "source": "github.com/suppers-ai/wafer-go/blocks/sqlite"
    },
    {
      "type": "log",
      "source": "./blocks/logger.wasm"
    }
  ],
  "chains": [
    {
      "id": "api",
      "summary": "HTTP API entrypoint that routes requests to handler chains",
      "root": {
        "block": "http",
        "config": { "port": 8080 },
        "next": [
          { "match": "POST:/users", "chain": "create-user" },
          { "match": "GET:/users/*", "chain": "get-user" }
        ]
      }
    },
    {
      "id": "create-user",
      "summary": "Creates a new user with validation, authentication, and database storage",
      "config": { "on_error": "stop", "timeout": "30s" },
      "root": {
        "block": "log",
        "next": [
          {
            "block": "validate",
            "config": { "schema": "user-schema" },
            "next": [
              {
                "block": "auth",
                "config": { "jwt_secret": "${JWT_SECRET}" },
                "next": [
                  {
                    "block": "db",
                    "config": { "path": "./data.db", "table": "users" }
                  }
                ]
              }
            ]
          }
        ]
      }
    }
  ]
}
```

In this configuration:
- The `api` chain has an HTTP block at its root that processes incoming requests
- Match patterns route `POST:/users` to the `create-user` chain reference
- The `create-user` chain processes messages through log, validate, auth, and db blocks sequentially

---

## Related Documents

- **[WAFER Spec](./WAFER_SPEC.md)** — The specification that WAFER-Go implements (blocks, chains, interfaces, registry)
- **Solobase** — BaaS platform built on WAFER-Go (block-based architecture with Preact UIs)

---

## License

MIT License
