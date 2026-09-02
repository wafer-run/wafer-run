//! Discovery document generation — OpenAPI 3.1 and A2A AgentCard.

use serde_json::{json, Value};
use wafer_block::types::{AgentTool, AuthLevel, BlockEndpoint, BlockInfo, HttpMethod};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn method_key(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
    }
}

/// Extract properties from a JSON Schema object and turn them into OpenAPI
/// parameter objects with the given `in` value.
fn extract_params(schema: &Value, location: &str) -> Vec<Value> {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    props
        .iter()
        .map(|(name, prop_schema)| {
            let is_required = required.contains(&name.as_str());
            json!({
                "name": name,
                "in": location,
                "required": is_required,
                "schema": prop_schema,
            })
        })
        .collect()
}

/// Build a slug from a URL path suitable for use in a skill id.
/// Strips leading `/`, removes braces, and replaces `/` with `_`.
fn path_to_slug(path: &str) -> String {
    path.trim_start_matches('/')
        .replace(['{', '}'], "")
        .replace('/', "_")
}

/// JSON Schema keywords whose value is *instance data*, not a subschema.
///
/// `default`, `const`, and each entry of `examples` and `enum` are literal
/// JSON values that a validator compares against the instance; they are
/// never interpreted as schemas. So the `$ref` rewriting below must not walk
/// into them, in either direction:
///
/// * A literal `{"$ref": "https://example.com/x"}` sitting in a `default` is
///   perfectly legal user data — an object with one key that happens to be
///   spelled `$ref`. Treating it as a reference reports the whole endpoint
///   as carrying an unresolvable `$ref` and deletes a working tool.
/// * Worse, and silent: `{"$ref": "#/$defs/D", "note": "..."}` in a
///   `default` would be *rewritten* through the sibling-merge path, so the
///   published schema hands the agent a default value the endpoint never
///   declared.
///
/// `$defs` stripping is suspended inside them for the same reason: a literal
/// value may legitimately have a `$defs` key, and deleting it would corrupt
/// the data.
///
/// These names are keywords only in a position where the surrounding
/// object's keys are *read* as keywords. Inside one of the
/// [`SCHEMA_MAP_KEYWORDS`] maps the keys are names the author chose, and the
/// rule must not apply — see that constant.
const LITERAL_VALUE_KEYWORDS: &[&str] = &["default", "const", "examples", "enum"];

/// JSON Schema keywords whose value is a *map from author-chosen names to
/// schemas*, rather than a schema object.
///
/// Which of the two a member sits in decides whether its key is a keyword at
/// all. In a schema object, `default` is the default-value keyword. In a
/// `properties` map, `default` is a field literally named `default` —
/// `struct S { default: Status }` emits
/// `{"properties": {"default": {"$ref": "#/$defs/Status"}}}` — and its value
/// is a schema that must be resolved like any other.
///
/// Deciding by key name alone copied that `$ref` through verbatim, stripped
/// `$defs` out from under it, and set no flag, so the tool shipped an
/// argument pointing at a definition no longer in the document. That is the
/// silent lie about a tool's own arguments the rest of this module exists to
/// refuse, so the walk tracks position instead of guessing from the name.
///
/// `$defs` is deliberately absent: it never reaches the member walk, because
/// a schema object drops it outright as the reference table. A *property*
/// named `$defs` is a member of a `properties` map and is never read as that
/// table.
const SCHEMA_MAP_KEYWORDS: &[&str] = &["properties", "patternProperties", "dependentSchemas"];

/// The most JSON nodes — objects, arrays, and scalars counted individually —
/// one schema source may expand into while its `$ref`s are inlined.
///
/// Cycle detection bounds the *depth* of an expansion but not its *size*.
/// Inlining copies a definition in full at every place it is referenced, so a
/// finite, acyclic type whose definitions each reference the next one twice
/// doubles the output per level: 22 such levels turn a 2 KB schema into
/// 247 MB. Nothing about that schema is malformed — every reference resolves
/// and no reference closes a cycle — so the unresolved-reference verdict does
/// not describe it, and with no size bound the generator runs until it
/// exhausts memory while the manifest request hangs.
///
/// 100 000 is picked to be unreachable by a real type tree and unmissable by
/// a runaway one. An inlined property costs roughly three to six nodes (its
/// schema object, its `type`, its `description`), so the budget is on the
/// order of twenty thousand inlined properties — a few megabytes of
/// `inputSchema`, orders of magnitude past the largest schema any block here
/// declares and past what an MCP client would accept in a manifest. The
/// doubling case crosses it before level twenty, in milliseconds, long
/// before any memory is at risk.
///
/// # What the budget bounds, exactly
///
/// It bounds the nodes the *walk emits*, which is not quite the size of the
/// returned document. [`inline_refs`] keeps a cyclic definition by cloning
/// the body its frame already produced, so a kept definition's nodes are
/// charged once (when they were walked) and appear twice (in the output tree
/// and in the `$defs` table). With `d` definitions kept — `d` is the number
/// of *distinct* definitions some back-edge names, one for almost every real
/// recursive type — the returned document holds at most `(d + 1) ×
/// MAX_INLINED_NODES` nodes.
///
/// The clone is deliberately not charged. Charging it would push a
/// legitimately-sized recursive schema over a budget that exists for runaway
/// *expansion*, and there is no runaway here: `d` is bounded by the number of
/// definitions the source declares, and each kept body is a subtree of output
/// the walk already paid for. A factor of `d + 1` on a bound chosen to be
/// orders of magnitude larger than any real schema is not the failure this
/// constant guards against.
const MAX_INLINED_NODES: usize = 100_000;

/// Decode a `#/$defs/` pointer segment back into the key it names in the
/// `$defs` table.
///
/// schemars writes reference *names* through its `encode_ref_name`: `~`
/// becomes `~0` and `/` becomes `~1` (RFC 6901 JSON-Pointer escaping), and
/// every other byte outside the URI-fragment safe set is percent-encoded
/// (space, `"`, `#`, `%`, `<`, `>`, `[`, `\`, `]`, `^`, `` ` ``, `{`, `|`,
/// `}`, and anything non-ASCII). The `$defs` *keys* are left unencoded, so
/// `#[schemars(rename = "Product Status")]` emits a reference to
/// `#/$defs/Product%20Status` against a table keyed `Product Status`.
/// Looking the raw segment up would miss and silently degrade the property
/// to `{}`.
///
/// Unescaping order is load-bearing, and is the order RFC 6901 §4 requires:
/// `~1` first, then `~0`. Doing `~0` first would turn the encoding of the
/// literal name `~1` (which is `~01`) into `~1` and then into `/`.
///
/// Returns `None` when percent-decoding does not yield valid UTF-8 — a
/// segment that cannot name any key, and so must be reported as unresolvable
/// rather than guessed at.
fn decode_ref_name(encoded: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()?;
    Some(decoded.replace("~1", "/").replace("~0", "~"))
}

/// The bytes a `#/$defs/` pointer segment may carry unencoded: the RFC 3986
/// unreserved set (`ALPHA / DIGIT / "-" / "." / "_" / "~"`). Everything else
/// is percent-encoded, including every non-ASCII byte —
/// `percent_encoding` escapes those regardless of the set.
const REF_NAME_ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Encode a `$defs` key into the pointer segment that names it — the exact
/// inverse of [`decode_ref_name`], and the encoding schemars itself writes.
///
/// Used for the back-edges [`inline_refs`] emits for a cycle. The `$defs`
/// keys it writes are the *unencoded* names, exactly as schemars leaves them,
/// so a definition named `Product Status` is keyed `Product Status` and
/// referred to as `#/$defs/Product%20Status`.
///
/// Order is load-bearing and mirrors the decoder's. JSON-Pointer escaping
/// comes first (`~` → `~0`, then `/` → `~1`, in that order, so the `~` that
/// `~1` introduces is not escaped a second time), then percent-encoding —
/// which leaves the escapes alone, since `~`, `0` and `1` are all unreserved.
fn encode_ref_name(name: &str) -> String {
    let escaped = name.replace('~', "~0").replace('/', "~1");
    percent_encoding::utf8_percent_encode(&escaped, REF_NAME_ESCAPE).to_string()
}

/// Count the JSON nodes in a value.
///
/// Used to charge a verbatim-copied literal ([`LITERAL_VALUE_KEYWORDS`]) to
/// the same [`MAX_INLINED_NODES`] budget the resolved nodes pay into. A
/// literal is small on its own, but it is copied again at every place its
/// enclosing definition is inlined, so leaving it uncharged would leave a
/// hole in the bound.
fn count_nodes(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(count_nodes).sum::<usize>(),
        Value::Array(items) => 1 + items.iter().map(count_nodes).sum::<usize>(),
        _ => 1,
    }
}

/// Rewrite a schemars-generated schema into a *self-contained* one: every
/// `#/$defs/*` reference is replaced by its target, and the incoming `$defs`
/// block is dropped.
///
/// OpenAPI clients resolve `$ref` fine, which is why `generate_openapi` does
/// not do this. Many MCP-style clients do not, so the WebMCP projection must
/// hand over schemas that stand alone.
///
/// # Self-contained is not reference-free
///
/// A recursive type — a `Condition` that nests `Condition`s, a
/// `struct Node { children: Vec<Node> }` — has no finite inlining, and for a
/// long time this function said so and the endpoint was refused. That threw
/// away a legal, describable schema: JSON Schema 2020-12 expresses recursion
/// with a reference to a definition the *same document* carries, which is
/// self-contained in the only sense that matters here — a client needs
/// nothing but the document it was handed to resolve it.
///
/// So the cycle is not cut; it is rebased. The first reference to a
/// definition is still inlined in full, and only the reference that closes
/// the cycle becomes a `$ref` back to that definition, which the output then
/// carries under its own `$defs`. Exactly the definitions some back-edge
/// names are kept — every other one is inlined and named by nothing.
/// schemars' root-recursion marker `{"$ref": "#"}` is rebased the same way,
/// onto a definition named after the document's `title` (see
/// [`root_definition_name`]), because a bare `#` would point at whatever
/// document the schema is later embedded in — the merged `inputSchema`,
/// which is a different document.
///
/// Returns the rewritten schema together with the ways the rewrite could not
/// be done honestly — see [`RefIssues`]. Both leave `{}` behind: an
/// unconstrained schema standing where the server requires a concrete type.
/// At the top level that shows up as a missing `properties` object and is
/// caught by the `unrepresentable` check, but below the top level nothing
/// else would notice. So the report travels out of here and callers refuse
/// to build a tool from a schema that sets either of them.
fn inline_refs(schema: &Value) -> (Value, RefIssues) {
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    let root_name = root_definition_name(schema, &defs);
    let mut walk = RefWalk {
        defs: &defs,
        active: Vec::new(),
        issues: RefIssues::default(),
        emitted: 0,
        kept: std::collections::BTreeMap::new(),
        cyclic: std::collections::BTreeSet::new(),
        root_name: root_name.clone(),
        root_recursive: false,
    };
    let mut resolved = walk.resolve(schema);
    let mut issues = walk.issues;
    let mut kept = walk.kept;

    // The root's definition *is* the finished document, so it can only be
    // taken once the walk is over — and with the table this is about to add
    // stripped back off, because a kept definition never nests one. (`resolve`
    // has already dropped the incoming table, so the removal only guards the
    // invariant rather than doing work.)
    if walk.root_recursive {
        let mut body = resolved.clone();
        if let Some(map) = body.as_object_mut() {
            map.remove("$defs");
        }
        kept.insert(root_name, body);
    }

    if !kept.is_empty() {
        match resolved.as_object_mut() {
            Some(map) => {
                map.insert("$defs".into(), Value::Object(kept.into_iter().collect()));
            }
            // Only a hand-written source reaches this: a document whose root
            // is not a schema object at all — a bare JSON array — that still
            // reached a cycle through one of its members. There is nowhere to
            // put the table, so the back-edges would dangle. Saying so is the
            // same verdict a `$ref` with no referent gets, and for the same
            // reason. (Such a source cannot be published anyway: it is not
            // object-shaped, so `source_is_flattenable` refuses it.)
            None => issues.unresolved = true,
        }
    }

    (resolved, issues)
}

/// The name a document's root is published under when it references itself.
///
/// schemars closes a cycle on the *root* type with the marker
/// `{"$ref": "#"}` and puts nothing in `$defs` for it, so the retained form
/// has to name it something. The root's `title` is that name — on a derived
/// schema it is the source Rust type's name, which is precisely what the
/// definition is — and `Root` stands in when there is no title.
///
/// The name must not be one the incoming `$defs` table already uses, or the
/// kept table would have to hold two different bodies under one key and half
/// the back-edges would resolve to the wrong one. A trailing `_` is appended
/// until the name is free. That only ever fires for a hand-written schema:
/// schemars never puts the root type it is inlining into its own `$defs`.
fn root_definition_name(schema: &Value, defs: &Value) -> String {
    let mut name = schema
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty())
        .unwrap_or("Root")
        .to_string();
    while defs.get(name.as_str()).is_some() {
        name.push('_');
    }
    name
}

/// The two distinct ways [`inline_refs`] can fail to produce a
/// self-contained schema. They are reported separately because the fix is
/// different and the author is sent to a different place: a dangling
/// reference means a missing or misspelled `$defs` entry, and an
/// over-budget expansion means a well-formed type whose definitions
/// multiply out. Collapsing the two into one flag — as a depth cap did —
/// sends the author of a large type hunting for a `$defs` entry that is not
/// missing.
///
/// A cycle is not on this list. It used to be, on the reasoning that a
/// recursive type has no finite inlining; it does have a finite
/// *self-contained* form, and [`inline_refs`] now emits it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RefIssues {
    /// A `$ref` named a target that does not exist, or took a form this
    /// function cannot follow (anything but `#/$defs/*`, including an
    /// external URL).
    unresolved: bool,
    /// The inlining passed [`MAX_INLINED_NODES`], so the walk stopped and
    /// the rest of the schema is missing. Not a defect in any one reference:
    /// the type is finite and every reference resolves, but definitions
    /// referenced from several levels multiply out.
    oversized: bool,
}

/// One pass of [`inline_refs`]: the reference table it resolves against, the
/// chain of definitions currently open, what went wrong, and how much output
/// has been produced so far.
struct RefWalk<'a> {
    /// The document's `$defs` table — the only thing a `#/$defs/*` pointer
    /// can name.
    defs: &'a Value,
    /// The definitions currently being expanded, innermost last.
    ///
    /// A stack, not a set of everything ever seen: a definition referenced
    /// twice from *different* branches of a finite type tree is not a cycle,
    /// and must inline both times.
    active: Vec<String>,
    issues: RefIssues,
    /// JSON nodes emitted so far, charged against [`MAX_INLINED_NODES`].
    emitted: usize,
    /// Definitions that closed a cycle and are therefore kept under the
    /// output's `$defs` instead of inlined. Filled when a frame that is
    /// `active` is referenced again; the body is stored when that frame
    /// finishes resolving.
    kept: std::collections::BTreeMap<String, Value>,
    /// Names whose bodies must be captured when their frame completes.
    ///
    /// Separate from `kept` because the two are known at different moments:
    /// a cycle is discovered at the back-edge, deep inside the frame, and
    /// the body only exists once that frame unwinds.
    cyclic: std::collections::BTreeSet<String>,
    /// The name the document root is known by when it references itself —
    /// see [`root_definition_name`].
    root_name: String,
    /// Whether the root-recursion marker `{"$ref": "#"}` was seen, and so
    /// whether `root_name` names a definition the output has to carry.
    root_recursive: bool,
}

impl RefWalk<'_> {
    /// Charge `nodes` to the output budget.
    ///
    /// Returns `false` once the budget is spent, at which point the caller
    /// emits `{}` and stops descending. Every frame still on the stack then
    /// returns immediately, so a runaway expansion costs the budget and not
    /// much more.
    fn spend(&mut self, nodes: usize) -> bool {
        self.emitted = self.emitted.saturating_add(nodes);
        if self.emitted > MAX_INLINED_NODES {
            self.issues.oversized = true;
            return false;
        }
        true
    }

    /// Resolve one `$ref` target, tracking the chain of definitions currently
    /// being expanded so a cycle is rebased where it closes rather than after
    /// an arbitrary number of hops.
    fn resolve_ref_target(&mut self, reference: &str) -> Value {
        // schemars' root-recursion marker: the document root contains
        // itself. There is no finite inlining of that, but there is a finite
        // self-contained document — the root under a name of its own, with
        // the marker pointing at it. A bare `#` cannot survive into the
        // output: the schema is embedded in a larger document downstream, and
        // `#` would then name that document instead.
        if reference == "#" {
            self.root_recursive = true;
            return json!({ "$ref": format!("#/$defs/{}", encode_ref_name(&self.root_name)) });
        }

        let Some(name) = reference.strip_prefix("#/$defs/").and_then(decode_ref_name) else {
            self.issues.unresolved = true;
            return json!({});
        };
        let Some(target) = self.defs.get(name.as_str()) else {
            self.issues.unresolved = true;
            return json!({});
        };
        if self.active.contains(&name) {
            // The cycle closes here. Point back at the definition instead of
            // expanding it a second time, and mark it so the frame that owns
            // it leaves a body behind for this reference to resolve against.
            let back_edge = json!({ "$ref": format!("#/$defs/{}", encode_ref_name(&name)) });
            self.cyclic.insert(name);
            return back_edge;
        }

        let target = target.clone();
        self.active.push(name.clone());
        let resolved = self.resolve(&target);
        self.active.pop();
        // Only a definition something referred *back* to is kept. A
        // definition that merely appears twice in a finite tree is inlined at
        // both sites and named by nothing, so putting it in the table would
        // ship a definition no reference resolves against.
        if self.cyclic.contains(&name) {
            self.kept.entry(name).or_insert_with(|| resolved.clone());
        }
        resolved
    }

    /// Walk one member of a *schema object*, whose keys are JSON Schema
    /// keywords.
    ///
    /// Only here is a key read as a keyword: a literal-value keyword's data
    /// is copied verbatim ([`LITERAL_VALUE_KEYWORDS`]), and a schema-map
    /// keyword's members are walked as author-named schemas
    /// ([`SCHEMA_MAP_KEYWORDS`]). Everything else is a subschema.
    fn resolve_member(&mut self, key: &str, value: &Value) -> Value {
        if LITERAL_VALUE_KEYWORDS.contains(&key) {
            if !self.spend(count_nodes(value)) {
                return json!({});
            }
            value.clone()
        } else if SCHEMA_MAP_KEYWORDS.contains(&key) {
            self.resolve_schema_map(value)
        } else {
            self.resolve(value)
        }
    }

    /// Walk a map whose keys are names the author chose and whose values are
    /// schemas — the value of a [`SCHEMA_MAP_KEYWORDS`] keyword.
    ///
    /// No key here is a keyword. A body field really can be named `default`,
    /// `const`, `enum`, `examples`, or `$defs`, and every one of those names
    /// carries an ordinary schema that must be resolved.
    fn resolve_schema_map(&mut self, node: &Value) -> Value {
        let Value::Object(map) = node else {
            // Not a map at all. Only a hand-written schema can produce this,
            // and reading it as a subschema is the one interpretation that
            // does not invent structure that is not there.
            return self.resolve(node);
        };
        if !self.spend(1) {
            return json!({});
        }
        let mut out = serde_json::Map::new();
        for (key, value) in map {
            let resolved = self.resolve(value);
            out.insert(key.clone(), resolved);
        }
        Value::Object(out)
    }

    /// Walk one node as a schema.
    fn resolve(&mut self, node: &Value) -> Value {
        if !self.spend(1) {
            return json!({});
        }
        match node {
            Value::Object(map) => {
                if let Some(Value::String(reference)) = map.get("$ref") {
                    let mut resolved = self.resolve_ref_target(reference);

                    // JSON Schema 2020-12 allows keywords ALONGSIDE `$ref`,
                    // and schemars uses exactly that: a doc-commented field
                    // of a named type emits
                    // `{"description": "...", "$ref": "#/$defs/Status"}`.
                    // Returning only the resolved target would silently
                    // delete every such field description. Siblings win over
                    // the target's own keys, since they are the more specific
                    // annotation. `$defs` is excluded here too — it is the
                    // reference table itself, not a schema keyword, and must
                    // never survive into the output (it can appear as a
                    // literal sibling of `$ref` when the ref sits at the
                    // schema root). The siblings are members of a schema
                    // object, so they go through exactly the keyword-position
                    // rules the plain-object walk below uses.
                    if let Some(out) = resolved.as_object_mut() {
                        for (key, value) in map {
                            if key == "$ref" || key == "$defs" {
                                continue;
                            }
                            let member = self.resolve_member(key, value);
                            out.insert(key.clone(), member);
                        }
                    }
                    return resolved;
                }

                let mut out = serde_json::Map::new();
                for (key, value) in map {
                    // `$defs` is the reference table itself, never part of
                    // the resulting schema. This is a schema object, so the
                    // key means that table; a *property* named `$defs` is a
                    // member of a `properties` map and never reaches here.
                    if key == "$defs" {
                        continue;
                    }
                    let member = self.resolve_member(key, value);
                    out.insert(key.clone(), member);
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.resolve(item));
                }
                Value::Array(out)
            }
            other => other.clone(),
        }
    }
}

/// What one schema source contributed to the merged agent input schema, and
/// the ways it can be unusable.
#[derive(Debug, Default)]
struct MergedSource {
    /// Top-level property names this source contributed, sorted.
    contributed: Vec<String>,
    /// The endpoint declared something here (not `None`, not `{}`, not
    /// `null`). A source that declared nothing contributes nothing and
    /// constrains nothing.
    present: bool,
    /// The source is present but cannot be honestly flattened into the
    /// merged object schema — see [`source_is_flattenable`].
    unrepresentable: bool,
    /// Inlining hit a `$ref` it could not resolve, at any depth — see
    /// [`inline_refs`].
    unresolved_ref: bool,
    /// Definitions this source kept (see [`inline_refs`]) whose name is
    /// already in the merged table with a *different* body. One flat schema
    /// has one `$defs` table, so the two cannot both be described by it.
    colliding_defs: Vec<String>,
    /// Inlining ran past [`MAX_INLINED_NODES`] and stopped, so the schema
    /// below is truncated — see [`inline_refs`].
    oversized: bool,
    /// The source declared `additionalProperties: false`. The merged object
    /// can only repeat that claim when *every* present source makes it —
    /// see [`agent_input_schema`].
    closed: bool,
}

/// Schema keywords the flattening in [`merge_schema_source`] knows how to
/// either carry across or drop without changing what the endpoint accepts.
///
/// This is an allow-list on purpose. JSON Schema keeps growing keywords, and
/// the failure mode of guessing wrong is a tool whose `inputSchema` silently
/// omits a constraint the server still enforces. Refusing an endpoint is
/// visible and fixable; publishing a tool that lies about its own arguments
/// is neither.
///
/// The structural entries — `type`, `properties`, `required` — are what the
/// merge reads, plus `additionalProperties`, handled explicitly in
/// [`source_is_flattenable`], and `$defs`, which is neither structure nor
/// annotation:
///
/// * `$defs` — the definitions [`inline_refs`] kept because a cycle closes
///   on them. Dropping it would strand every back-edge in `properties` on a
///   pointer with no referent, which is exactly the unconstrained-`{}` lie
///   the rest of this wall exists to prevent. So it is neither dropped nor
///   carried in place: [`merge_schema_source`] *hoists* it into the one
///   `$defs` table the merged schema has, since the three sources are being
///   folded into a single document.
///
/// The rest are pure annotations with no effect on validation:
///
/// * `title` — on a derived schema this is the *Rust type name* of the
///   source struct (or the author's `#[schemars(title = "...")]`). It names
///   the source type, not the merged argument object an agent fills in, so
///   the WebMCP projection deliberately drops it here. It is *kept* in the
///   stored schema, because `/openapi.json` embeds these verbatim and
///   OpenAPI client generators use `title` to name the types they generate —
///   see `wafer_block::types::endpoint`'s `self_contained_schema`.
/// * `description`, `$comment`, `examples`, `default`, `deprecated`,
///   `readOnly`, `writeOnly`, `$id`, `$schema` — documentation and metadata;
///   dropping them loses prose, not constraints.
const FLATTENABLE_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "$defs",
    "title",
    "description",
    "$comment",
    "examples",
    "default",
    "deprecated",
    "readOnly",
    "writeOnly",
    "$id",
    "$schema",
];

/// Whether a schema object describes a plain JSON object.
///
/// Shared by [`source_is_flattenable`] and [`agent_output_schema`] so the
/// two never drift on what "object-shaped" means — one is deciding whether a
/// source can be folded into the merged `inputSchema`, the other whether a
/// declaration can be published as an `outputSchema`, and both questions
/// start here.
fn schema_is_object_shaped(map: &serde_json::Map<String, Value>) -> bool {
    match map.get("type") {
        Some(Value::String(t)) if t == "object" => true,
        // No `type` at all is object-shaped only if it says so some other
        // way; `properties` is the only such signal the merge can act on.
        None => map.contains_key("properties"),
        // Anything else, including a non-string `type` — the keyword is only
        // ever a string or an array of strings, and neither an array-typed
        // nor a malformed `type` names a plain object.
        _ => false,
    }
}

/// Whether `inlined` — one already-`$ref`-flattened schema source — can be
/// honestly folded into the merged agent input schema.
///
/// The question deliberately is *not* "does it have a `properties` object?".
/// That test is wrong in both directions, and both directions ship a bad
/// tool:
///
/// * **It misses lies.** Composition keywords sit *beside* `properties` at
///   least as often as they replace it. A body struct with
///   `#[serde(flatten)] kind: SomeEnum` emits the enum's `oneOf` as a
///   sibling of the merged `properties`, so a properties-based test sees a
///   healthy schema, publishes the tool, and every field and `required`
///   entry inside those branches is missing from `inputSchema`. The agent
///   400s forever. `allOf`, `anyOf`, `if`/`then`, `not`,
///   `patternProperties`, `dependentRequired`, `minProperties`, `const`, a
///   `$ref` surviving at the *top level* — anything outside
///   [`FLATTENABLE_KEYWORDS`] — has the same shape of failure.
/// * **It over-refuses.** A fieldless struct derives `{"type": "object"}`
///   with no `properties` at all, and genuinely takes no arguments.
///   Contributing nothing for it is the truth, not a lie, so it is
///   representable.
///
/// So the question asked here is "is the entire content of this source
/// expressible as `properties` + `required` in a single flat object whose
/// values the name-based `invocation` provenance can route?" Anything else
/// is refused.
///
/// Non-object shapes fail outright: a tagged-enum body (`{"oneOf": [...]}`),
/// an array body (`Vec<T>`), a nullable body (`{"type": ["object",
/// "null"]}`), a boolean schema (`serde_json::Value` derives `true`), or the
/// universal schema `{}` that inlining leaves behind for a reference it
/// could not resolve. None of them is an object with named members, so a
/// flat object schema cannot describe them at all.
///
/// The structural keywords are checked by *type*, not just by name. A
/// keyword the merge reads with a typed accessor — `properties` as an
/// object, `required` as an array of strings, `type` as a string — that
/// holds something else makes that accessor return `None`, and the merge
/// would read "contributed nothing" / "required nothing" from what is really
/// a malformed declaration. That publishes a tool advertising the wrong
/// argument set, so it is refused instead.
///
/// `additionalProperties` is the one keyword that is both carried and
/// restricted:
///
/// * `false` closes the object. Representable, and reported through
///   [`MergedSource::closed`] so the merged schema can repeat the claim when
///   every present source agrees.
/// * Absent is the serde default — unknown fields are ignored, so there is
///   nothing to carry.
/// * A *schema* (a `HashMap<String, T>` body) or `true` means the server
///   accepts arbitrary extra keys that carry meaning. `invocation` routes
///   arguments by name — `path_params` / `query_params` / `body_params` are
///   fixed name lists — so a key the agent invents has nowhere to go and is
///   dropped on the way to the server, which then rejects the request for
///   the missing data. Advertising an open object the client cannot actually
///   transmit fails on every single invocation, so it is refused rather than
///   published.
fn source_is_flattenable(inlined: &Value) -> bool {
    let Some(map) = inlined.as_object() else {
        // A boolean schema (`true` / `false`) or any non-object node.
        return false;
    };

    // `{}` accepts literally anything, including non-objects. It is also
    // what `inline_refs` leaves behind for a reference it could not resolve.
    if map.is_empty() {
        return false;
    }

    if !schema_is_object_shaped(map) {
        return false;
    }

    if map
        .keys()
        .any(|key| !FLATTENABLE_KEYWORDS.contains(&key.as_str()))
    {
        return false;
    }

    // The keyword *names* being flattenable is not enough: the merge reads
    // `properties` as an object and `required` as an array of strings, and
    // `serde_json`'s accessors return `None` for anything else — which would
    // silently mean "contributed nothing" for a `"properties": "oops"` and
    // "required nothing" for a `"required": "a"`. Both publish a tool that
    // misdescribes its own arguments, which is precisely what this wall
    // exists to prevent. Only a hand-written schema can reach here; schemars
    // never emits these shapes.
    if map
        .get("properties")
        .is_some_and(|props| !props.is_object())
    {
        return false;
    }
    if let Some(required) = map.get("required") {
        match required.as_array() {
            Some(names) if names.iter().all(Value::is_string) => {}
            _ => return false,
        }
    }

    matches!(
        map.get("additionalProperties"),
        None | Some(Value::Bool(false))
    )
}

/// Collect `properties` and `required` from one schema source into the
/// merged accumulators, reporting the property names it contributed and
/// whether the source could be flattened into the merged object at all.
///
/// Names come back sorted so the generated manifest is byte-stable across
/// runs — `serde_json::Map`'s default backing is already a `BTreeMap`
/// (`serde_json`'s `preserve_order` feature, which would make it
/// insertion-ordered instead, is not enabled anywhere in this workspace),
/// but the upstream schema's own key order is not something we control, so
/// the sort here makes that intent explicit and keeps it correct if that
/// feature is ever flipped on.
///
/// Representability is decided by [`source_is_flattenable`], which documents
/// the shapes that fail and why. An unrepresentable source contributes
/// nothing at all — the caller refuses the endpoint outright, so there is no
/// half-merged schema to keep consistent.
///
/// That check is top-level only, by construction. An unresolvable `$ref`
/// *below* the top level leaves the top level intact and hides a `{}` inside
/// one property, so [`inline_refs`]'s own report is carried out separately
/// in `unresolved_ref` rather than folded into `unrepresentable`.
///
/// # `$defs` is hoisted, not carried
///
/// A source that kept a cyclic definition (see [`inline_refs`]) arrives with
/// a `$defs` table of its own, and the merged schema is one document with
/// one such table. Its entries are therefore folded into the shared `defs`
/// accumulator rather than left on the source, so that the back-edges in the
/// properties this source contributes still resolve inside the merged
/// document. Two sources naming the same definition *identically* is not a
/// conflict — they are the same type, and one entry describes both. Two
/// sources naming it *differently* is, and is reported in `colliding_defs`
/// for the caller to refuse: there is no second table to put the loser in,
/// and silently keeping the first would misdescribe the second source's
/// arguments.
///
/// Only a flattenable source contributes definitions, for the same reason it
/// contributes no properties: the caller refuses the endpoint outright, so
/// there is no half-merged document to keep consistent — and a definition
/// hoisted out of a source that was then refused could collide with a later
/// source and report the wrong defect.
fn merge_schema_source(
    source: Option<&Value>,
    properties: &mut serde_json::Map<String, Value>,
    required: &mut Vec<String>,
    defs: &mut serde_json::Map<String, Value>,
) -> MergedSource {
    let Some(source) = source else {
        return MergedSource::default();
    };
    // `None`, `null`, and `{}` all mean "this endpoint declares nothing
    // here". Reading a hand-written `{}` as "nothing declared" matches the
    // endpoint that omits the builder call entirely; read as a schema it
    // would mean "accepts anything", which is not what an author writing
    // `{}` intends and which `source_is_flattenable` refuses anyway.
    let present = match source {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    };
    if !present {
        return MergedSource::default();
    }

    let (inlined, ref_issues) = inline_refs(source);

    if !source_is_flattenable(&inlined) {
        return MergedSource {
            contributed: Vec::new(),
            present,
            unrepresentable: true,
            unresolved_ref: ref_issues.unresolved,
            colliding_defs: Vec::new(),
            oversized: ref_issues.oversized,
            closed: false,
        };
    }

    let mut colliding_defs = Vec::new();
    if let Some(table) = inlined.get("$defs").and_then(Value::as_object) {
        for (name, body) in table {
            match defs.get(name) {
                Some(existing) if existing != body => colliding_defs.push(name.clone()),
                Some(_) => {}
                None => {
                    defs.insert(name.clone(), body.clone());
                }
            }
        }
    }

    let mut contributed = Vec::new();
    if let Some(props) = inlined.get("properties").and_then(Value::as_object) {
        for (name, schema) in props {
            properties.insert(name.clone(), schema.clone());
            contributed.push(name.clone());
        }
    }
    if let Some(reqs) = inlined.get("required").and_then(Value::as_array) {
        for name in reqs.iter().filter_map(Value::as_str) {
            let owned = name.to_string();
            if !required.contains(&owned) {
                required.push(owned);
            }
        }
    }

    contributed.sort();

    let closed = inlined.get("additionalProperties") == Some(&Value::Bool(false));

    MergedSource {
        contributed,
        present,
        unrepresentable: false,
        unresolved_ref: ref_issues.unresolved,
        colliding_defs,
        oversized: ref_issues.oversized,
        closed,
    }
}

/// The flattened agent-facing input schema for one endpoint, plus the
/// provenance a client needs to rebuild a real HTTP request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentInputSchema {
    pub schema: Value,
    pub path_params: Vec<String>,
    pub query_params: Vec<String>,
    pub body_params: Vec<String>,
    /// Property names contributed by more than one of path/query/body.
    /// Non-empty means this endpoint MUST NOT be exposed as a tool: the
    /// merged schema can only describe one of the colliding locations, so
    /// any tool built from it would misdescribe its own arguments.
    pub collisions: Vec<String>,
    /// Source labels (`"path"`, `"query"`, `"body"`) that were present but
    /// could not be honestly flattened into the merged object — see
    /// [`source_is_flattenable`] for the shapes that trigger this and why.
    /// Non-empty means this endpoint MUST NOT be exposed as a tool: the
    /// manifest would describe arguments the server does not accept, or omit
    /// ones it requires, and a tool that can lie about its own arguments is
    /// worse than no tool.
    pub unrepresentable: Vec<String>,
    /// Source labels (`"path"`, `"query"`, `"body"`) whose inlining hit a
    /// `$ref` it could not resolve — see [`inline_refs`]. Non-empty means
    /// this endpoint MUST NOT be exposed as a tool: somewhere in the schema
    /// sits a bare `{}` that accepts anything while the server requires a
    /// specific type. Unlike [`Self::unrepresentable`] this catches the case
    /// at any depth, including the nested one that leaves the top-level
    /// `properties` object looking perfectly healthy.
    pub unresolved_refs: Vec<String>,
    /// Definition names two of path/query/body both kept, with different
    /// bodies, sorted — see [`merge_schema_source`]. Non-empty means this
    /// endpoint MUST NOT be exposed as a tool: the merged schema has one
    /// `$defs` table, so it can describe only one of the two, and every
    /// back-edge in the other source's properties would then resolve to the
    /// wrong type.
    ///
    /// Names, not source labels, because the name is the thing to rename —
    /// the sources are both fine on their own.
    pub colliding_defs: Vec<String>,
    /// Source labels (`"path"`, `"query"`, `"body"`) whose inlining ran past
    /// [`MAX_INLINED_NODES`] — see [`inline_refs`]. Non-empty means this
    /// endpoint MUST NOT be exposed as a tool, and that nothing else read
    /// off the schema can be trusted either: the walk stopped partway, so
    /// every verdict below is drawn from a truncated document. Reported
    /// separately from [`Self::unresolved_refs`] because nothing is missing
    /// from `$defs` — the definitions simply multiply out.
    pub oversized_schemas: Vec<String>,
    /// Names present in the merged `required` list that no source
    /// contributed a property for, sorted. Non-empty means this endpoint
    /// MUST NOT be exposed as a tool: `invocation` routes arguments by name
    /// from the three provenance lists, so an argument the agent supplies
    /// for a name with no property behind it has nowhere to travel — while
    /// the schema still tells a strict client the name is mandatory. Every
    /// call is then rejected by the server for missing data.
    pub undeclared_required: Vec<String>,
    /// Path or query parameters whose schema is not scalar-valued, each
    /// rendered as `"{source}.{name}"` and sorted. Non-empty means this
    /// endpoint MUST NOT be exposed as a tool — see
    /// [`param_is_scalar_valued`].
    pub non_scalar_params: Vec<String>,
}

/// The JSON Schema `type` names whose instances are single scalar values —
/// the ones that survive a trip through a URL path segment or a query-string
/// value unambiguously.
///
/// `null` is included because it only ever appears here as one member of a
/// nullable union (`["string", "null"]`, what schemars emits for an
/// `Option<T>` field): the value is either the scalar or absent, and the
/// WebMCP client skips `null`/`undefined` arguments rather than serializing
/// them.
const SCALAR_TYPE_NAMES: &[&str] = &["string", "number", "integer", "boolean", "null"];

/// Whether one path or query parameter's schema describes a value the client
/// can actually put in a URL.
///
/// A path or query parameter travels as text: one segment of the path, or
/// one `?name=value` pair. `invocation` carries no OpenAPI-style
/// serialization `style`/`explode`, and deliberately so — there is exactly
/// one honest reading of a scalar and no honest reading of anything else. An
/// array-typed `tags` param could mean `?tags=a&tags=b`, `?tags=a,b`, or
/// `?tags[]=a`, and the client has no way to pick; in practice it stringifies
/// the array and the server receives one comma-joined value it never asked
/// for. An object-typed param stringifies to `[object Object]`. Both fail on
/// every invocation while the manifest claims they work.
///
/// The check is deliberately generous about the *shapes* a scalar arrives
/// in, because refusing a legal one deletes a working tool:
///
/// * A plain scalar `type`.
/// * A union of scalars, which is how `Option<T>` reaches here
///   (`{"type": ["string", "null"]}`).
/// * `enum` or `const` with no `type` at all — a schemars unit-variant enum
///   emits exactly that, and every member being a scalar settles the
///   question without a `type` keyword.
/// * `anyOf` / `oneOf` / `allOf` whose every branch is itself scalar-valued —
///   `Option<SomeEnum>` inlines to `{"anyOf": [{"enum": [...]}, {"type": "null"}]}`,
///   which is a string or nothing.
///
/// What is refused: `array`, `object`, and a schema that constrains the value
/// no other way — a bare `{}`, or composition keywords this cannot read.
/// Those either provably cannot be serialized or provably cannot be shown to
/// be serializable, and the projection does not publish what it cannot vouch
/// for.
fn param_is_scalar_valued(schema: &Value) -> bool {
    let Some(map) = schema.as_object() else {
        // A boolean schema: `true` accepts anything, `false` accepts nothing.
        return false;
    };

    match map.get("type") {
        Some(Value::String(name)) => return SCALAR_TYPE_NAMES.contains(&name.as_str()),
        Some(Value::Array(names)) => {
            return !names.is_empty()
                && names.iter().all(|name| {
                    name.as_str()
                        .is_some_and(|name| SCALAR_TYPE_NAMES.contains(&name))
                });
        }
        // A `type` that is neither is malformed, not scalar.
        Some(_) => return false,
        None => {}
    }

    let scalar_literal = |value: &Value| !value.is_array() && !value.is_object();

    if let Some(Value::Array(members)) = map.get("enum") {
        return !members.is_empty() && members.iter().all(scalar_literal);
    }
    if let Some(value) = map.get("const") {
        return scalar_literal(value);
    }

    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(branches)) = map.get(keyword) {
            return !branches.is_empty() && branches.iter().all(param_is_scalar_valued);
        }
    }

    false
}

/// Flatten an endpoint's path, query, and body schemas into the single
/// `inputSchema` a WebMCP tool exposes, plus the provenance the client needs
/// to rebuild a real HTTP request from the agent's flat argument object.
///
/// A property name contributed by more than one of path/query/body is
/// recorded in `collisions` rather than silently resolved by last-source-wins:
/// the merged schema can only describe one of the colliding locations, so a
/// tool built from it would misdescribe its own arguments to the agent. This
/// function does not panic or pick a winner — it reports the conflict and
/// lets the caller (the WebMCP manifest generator) decide to skip the
/// endpoint rather than publish a tool that can lie about its arguments.
/// Likewise, a source that cannot be flattened at all (see
/// [`source_is_flattenable`]) is recorded in `unrepresentable` rather than
/// silently omitted.
///
/// # Path parameters are forced required
///
/// `invocation.path` is a template the client fills in by name. A
/// placeholder with no argument leaves the client nothing to substitute, so
/// it fetches `/b/products/storefront/undefined`. Every name the path source
/// contributed is therefore forced into the merged `required` list,
/// regardless of what that source said.
///
/// A source that declared a path parameter optional — a
/// `path_params_schema` written without a `required` entry, or a
/// `#[serde(default)]` on the field — is stating a contradiction, because
/// the route cannot match without a value in that segment. Forcing is
/// preferred to refusing because the forced form is simply the truth: the
/// endpoint really does require it, and refusing would delete a working tool
/// over a redundant annotation. The caller separately requires that the path
/// source's names are exactly the template's placeholders, so this can never
/// mark something required that does not appear in the URL.
fn agent_input_schema(ep: &BlockEndpoint) -> AgentInputSchema {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();

    // One table for all three sources: the merged schema is a single
    // document, so the definitions its back-edges name have exactly one
    // place to live — see `merge_schema_source`.
    let mut defs = serde_json::Map::new();

    let path = merge_schema_source(
        ep.path_params.as_ref(),
        &mut properties,
        &mut required,
        &mut defs,
    );
    let query = merge_schema_source(
        ep.query_params.as_ref(),
        &mut properties,
        &mut required,
        &mut defs,
    );
    let body = merge_schema_source(
        ep.input_schema.as_ref(),
        &mut properties,
        &mut required,
        &mut defs,
    );

    // See "Path parameters are forced required" above.
    for name in &path.contributed {
        if !required.contains(name) {
            required.push(name.clone());
        }
    }

    let mut unrepresentable = Vec::new();
    let mut unresolved_refs = Vec::new();
    let mut colliding_defs = Vec::new();
    let mut oversized_schemas = Vec::new();
    for (label, merged) in [("path", &path), ("query", &query), ("body", &body)] {
        if merged.unrepresentable {
            unrepresentable.push(label.to_string());
        }
        if merged.unresolved_ref {
            unresolved_refs.push(label.to_string());
        }
        colliding_defs.extend(merged.colliding_defs.iter().cloned());
        if merged.oversized {
            oversized_schemas.push(label.to_string());
        }
    }
    unrepresentable.sort();
    unresolved_refs.sort();
    colliding_defs.sort();
    colliding_defs.dedup();
    oversized_schemas.sort();

    // A `required` entry naming a property no source contributed. Only a
    // hand-written schema can produce it — schemars never emits a `required`
    // name without the matching property — and the merged schema would
    // repeat the claim while `invocation` has no provenance list to route
    // the argument through. Reported rather than filtered out of `required`:
    // filtering would hand the agent a tool that omits an argument the
    // server still demands, which is the same lie one level quieter.
    let mut undeclared_required: Vec<String> = required
        .iter()
        .filter(|name| !properties.contains_key(name.as_str()))
        .cloned()
        .collect();
    undeclared_required.sort();
    undeclared_required.dedup();

    // Path and query values travel as URL text — see `param_is_scalar_valued`.
    let mut non_scalar_params: Vec<String> = [("path", &path), ("query", &query)]
        .into_iter()
        .flat_map(|(label, merged)| {
            merged
                .contributed
                .iter()
                .filter(|name| {
                    properties
                        .get(name.as_str())
                        .is_none_or(|schema| !param_is_scalar_valued(schema))
                })
                .map(move |name| format!("{label}.{name}"))
        })
        .collect();
    non_scalar_params.sort();

    // `additionalProperties: false` describes the *merged* object, so it can
    // only be repeated when every source that fed that object closed itself.
    // One open source (a struct without `#[serde(deny_unknown_fields)]`)
    // means an unknown key is legal somewhere, and claiming otherwise would
    // make a strict client refuse arguments the server would have accepted.
    let merged_is_closed = {
        let mut present = [&path, &query, &body]
            .into_iter()
            .filter(|merged| merged.present)
            .peekable();
        present.peek().is_some() && present.all(|merged| merged.closed)
    };

    let (path_params, query_params, body_params) =
        (path.contributed, query.contributed, body.contributed);

    let mut counts: std::collections::HashMap<&str, u8> = std::collections::HashMap::new();
    for name in path_params
        .iter()
        .chain(query_params.iter())
        .chain(body_params.iter())
    {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let mut collisions: Vec<String> = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name.to_string())
        .collect();
    collisions.sort();

    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        required.sort();
        schema.insert("required".into(), json!(required));
    }
    if merged_is_closed {
        schema.insert("additionalProperties".into(), json!(false));
    }
    // Emitted only when a source actually kept something, so a schema with
    // no cycle in it is byte-for-byte what it was before definitions were
    // ever retained.
    if !defs.is_empty() {
        schema.insert("$defs".into(), Value::Object(defs));
    }

    AgentInputSchema {
        schema: Value::Object(schema),
        path_params,
        query_params,
        body_params,
        collisions,
        unrepresentable,
        unresolved_refs,
        colliding_defs,
        oversized_schemas,
        undeclared_required,
        non_scalar_params,
    }
}

/// The agent-facing projection of one endpoint's declared `output_schema`,
/// and — when there is none to publish — why.
#[derive(Debug, Clone, PartialEq)]
struct AgentOutputSchema {
    /// The self-contained schema to publish as the tool's `outputSchema`,
    /// or `None` when the endpoint declared nothing or the projection could
    /// not vouch for what it declared.
    schema: Option<Value>,
    /// Set when the endpoint *did* declare an output schema and it was
    /// dropped. `None` both when nothing was declared and when the
    /// projection succeeded.
    dropped: Option<WebMcpRefusal>,
}

/// Project an endpoint's declared response schema into the tool's
/// `outputSchema`: the same self-containment treatment `inputSchema` gets —
/// every `#/$defs/*` reference inlined except the ones a cycle closes on,
/// whose definitions travel in the schema's own `$defs`, and the root
/// `title` dropped — under the same [`MAX_INLINED_NODES`] budget.
///
/// Unlike the input side there is nothing to hoist: an output schema is one
/// source and therefore already one document, so the table [`inline_refs`]
/// built is published exactly as it stands and no two tables can collide.
///
/// # Why a bad output schema drops the field and not the tool
///
/// Every other verdict in this module refuses the whole tool, on the rule
/// that a tool that can lie about its arguments is worse than no tool. That
/// rule still applies here; it just lands somewhere else, because **the unit
/// of refusal is the smallest thing that can carry the lie.**
///
/// For `inputSchema` the smallest such unit *is* the whole tool. The field is
/// mandatory, and there is no way to say "I don't know what this takes": an
/// omitted or empty input schema is itself a claim — "this tool takes no
/// arguments" — which is exactly the lie being avoided. Refusing the field
/// and refusing the tool are the same act.
///
/// `outputSchema` is optional, and its absence claims nothing. That is not
/// merely a matter of description quality, which is the tempting reading:
/// outputs are read rather than supplied, so a missing one "only" costs the
/// agent a guess. But a *wrong* one costs more than a missing one, in two
/// concrete ways. A client that validates a tool's structured result against
/// the schema fails calls that would have succeeded. And a truncated
/// expansion can emit something that is not a schema at all — the budget
/// bailout replaces whatever node it lands on with `{}`, so a `required`
/// array can come out as `"required": {}` — which a client that validates
/// tool *definitions* rejects, taking the whole tool down with it inside the
/// consumer's per-tool `try`/`catch`, silently.
///
/// So: publish a schema this function can vouch for, publish none when it
/// cannot, and never withhold the tool over it. The endpoint's arguments are
/// unaffected by anything wrong with its response schema, and a missing tool
/// helps no one — the same reasoning that keeps a `deprecated` endpoint
/// published.
///
/// The drop is not silent: it travels back as a [`WebMcpRefusal`] and is
/// reported under [`WebMcpRefusalScope::OutputSchema`], so an author who
/// declared `.output::<T>()` and sees no `outputSchema` learns why. The
/// reasons are the `OutputSchema*` variants and nothing else: the input
/// side's `UnresolvedRefs` / `SchemaTooLarge` name `inputSchema` in their
/// own message text, so reusing them here would tell an author who declared
/// only an output type that their arguments are at fault.
///
/// # Why a non-object schema is dropped
///
/// A tool's `outputSchema` describes a *structured result*, and a structured
/// result is a JSON object — MCP types the field as an object schema for
/// that reason. An array-valued or scalar-valued response has no honest slot
/// here: publishing `{"type": "array"}` tells a validating client to expect
/// an object and check an array against it, which fails on every call.
///
/// The object test is [`schema_is_object_shaped`], the same one
/// [`source_is_flattenable`] applies: an explicit `"type": "object"`, or no
/// `type` at all alongside a `properties` map. A composition-keyword
/// response (`oneOf` of several object shapes) is therefore dropped too.
/// That is over-refusal, and it is the right direction: what is lost is
/// prose about a response the endpoint's OpenAPI document still describes in
/// full, and the alternative is vouching for a shape this function cannot
/// actually check.
///
/// # Why the whole flattenability wall is applied, not just the object test
///
/// Being object-*shaped* is not being well-*formed*. `{"type": "object",
/// "properties": "oops", "required": {"a": 1}}` passes the object test and
/// is not a JSON Schema at all: `properties` must be an object and
/// `required` an array of strings. Published verbatim it is exactly the
/// malformed document the budget-bailout argument above is about — a client
/// that validates tool *definitions* rejects it and takes the whole tool
/// down inside the consumer's per-tool `try`/`catch`, silently. Only a
/// hand-written schema reaches these shapes; schemars never emits them.
///
/// So [`source_is_flattenable`] is applied whole rather than re-deriving a
/// second, weaker test here. It is stricter than this path strictly needs —
/// nothing is being merged, so its keyword allow-list and its
/// `additionalProperties` rule are refusing shapes that could have been
/// published verbatim without lying. That is the same over-refusal the
/// object test already accepts, in exchange for one wall instead of two
/// walls that drift. Every schema `wafer-block` derives passes it.
fn agent_output_schema(ep: &BlockEndpoint) -> AgentOutputSchema {
    let nothing = AgentOutputSchema {
        schema: None,
        dropped: None,
    };
    let drop = |reason: WebMcpRefusal| AgentOutputSchema {
        schema: None,
        dropped: Some(reason),
    };

    let Some(source) = ep.output_schema.as_ref() else {
        return nothing;
    };
    // `null` and `{}` mean "declared nothing", exactly as they do for the
    // input sources — see `merge_schema_source`. Nothing was declared, so
    // nothing was dropped and there is nothing to report.
    let declared = match source {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    };
    if !declared {
        return nothing;
    }

    let (inlined, issues) = inline_refs(source);

    // Checked first, and for the reason the input side checks it first: past
    // the budget the walk stopped partway, so every verdict below would be
    // drawn from a document the endpoint never declared.
    if issues.oversized {
        return drop(WebMcpRefusal::OutputSchemaTooLarge);
    }
    if issues.unresolved {
        return drop(WebMcpRefusal::OutputSchemaUnresolvedRef);
    }

    let Some(map) = inlined.as_object() else {
        return drop(WebMcpRefusal::OutputSchemaNotAnObject);
    };
    // The same wall the input sources pass, for the same reason: a top level
    // this function cannot account for is one it cannot vouch for. The
    // object test comes first so the two failures stay distinguishable —
    // "your response is a `Vec<T>`" and "your response object is malformed"
    // send an author to different places.
    if !schema_is_object_shaped(map) {
        return drop(WebMcpRefusal::OutputSchemaNotAnObject);
    }
    if !source_is_flattenable(&inlined) {
        return drop(WebMcpRefusal::OutputSchemaUnrepresentable);
    }

    // The root `title` is the source Rust type's name. `wafer-block` keeps it
    // in the stored schema because `/openapi.json` embeds these verbatim and
    // client generators read it to name generated types; here it is noise
    // for an agent, and the merged `inputSchema` drops it on the same
    // grounds.
    let mut published = map.clone();
    published.remove("title");

    AgentOutputSchema {
        schema: Some(Value::Object(published)),
        dropped: None,
    }
}

/// Why a URL path template cannot be turned into a fillable tool URL.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathTemplateError {
    /// A segment carries braces the router will not read as a placeholder:
    /// unmatched, empty, nested, more than one per segment, or mixed with
    /// literal text.
    Malformed,
    /// A router wildcard segment, which no named argument can fill.
    Wildcard {
        /// The offending segment, verbatim.
        segment: String,
    },
}

/// Extract the `{name}` placeholders from a URL path template, in order of
/// appearance, using exactly the rule the router applies at runtime.
///
/// # The rule is the router's, not a general brace scan
///
/// `wafer_block::executor`'s `match_path` and `extract_path_vars` — the code
/// that actually serves these routes — split the pattern on `/` and treat a
/// segment as a placeholder only when it *is* one:
/// `pp.starts_with('{') && pp.ends_with('}')`, with the name taken as
/// `&pp[1..pp.len() - 1]`. Every other segment is compared literally. A scan
/// that finds braces anywhere therefore disagrees with the router in three
/// ways, each of which publishes a tool that fails on every call:
///
/// * **Infix.** `/b/x/v{version}/items` looks like it has a `version`
///   parameter. The router compares the literal segment `v{version}` against
///   `v1` and 404s.
/// * **Two in one segment.** `/b/x/{a}{b}` looks like two parameters. The
///   router sees one placeholder, named `a}{b`, so neither `req.param.a` nor
///   `req.param.b` is ever set and the handler reads nothing.
/// * **Wildcards.** `/b/x/**` has no braces at all, so a brace scan calls it
///   parameterless and publishes `**` as a literal path. `match_path`
///   special-cases a trailing `/**`, stripping it and matching every path
///   under the prefix, so the published URL *does* reach the handler — which
///   then runs against a garbage `**` subpath, a silent wrong answer rather
///   than a 404. A `**` that is not the final segment (`/b/x/**/y`) misses
///   that suffix rule and is compared literally, so the tool's URL only
///   matches a request whose segment really is `**`: a route that answers
///   nothing. Both are refused. A `*` segment is literal to this router too,
///   but it is the shape every author writes when they mean a wildcard, so
///   it is refused on the same terms rather than published as a path that
///   only works if it was meant literally.
///
/// Refusing these costs nothing worth keeping. Every one of them is already
/// broken at runtime, with the single exception of a lone `*` segment, which
/// this router reads literally: a tool for a route whose path genuinely
/// contains a `*` is the one thing lost, and the refusal is logged with its
/// reason so the author sees exactly why.
fn path_placeholders(path: &str) -> Result<Vec<String>, PathTemplateError> {
    let mut names = Vec::new();
    for segment in path.split('/') {
        if segment == "*" || segment == "**" {
            return Err(PathTemplateError::Wildcard {
                segment: segment.to_string(),
            });
        }
        if !segment.contains('{') && !segment.contains('}') {
            continue;
        }
        // Braces are only a placeholder when they span the whole segment.
        let Some(name) = segment
            .strip_prefix('{')
            .and_then(|inner| inner.strip_suffix('}'))
        else {
            return Err(PathTemplateError::Malformed);
        };
        if name.is_empty() || name.contains('{') || name.contains('}') {
            return Err(PathTemplateError::Malformed);
        }
        names.push(name.to_string());
    }
    Ok(names)
}

// ---------------------------------------------------------------------------
// generate_openapi
// ---------------------------------------------------------------------------

/// Move a schema's `$defs` into `components` and rewrite `#/$defs/X` to
/// `#/components/schemas/X`. Same-named definitions with identical bodies
/// share one entry; different bodies get a content-hash suffix.
///
/// Unlike [`inline_refs`], nothing is inlined: OpenAPI clients resolve
/// `$ref` fine, so each definition is published once under
/// `components.schemas` and every reference to it is rewritten in place —
/// including a cyclic one, which stays a `$ref` rather than needing the
/// back-edge dance `inline_refs` does for the ref-free WebMCP projection.
///
/// Two passes: decide every name first (bodies are compared *unrewritten*,
/// so the decision does not depend on rewrite order), then rewrite the root
/// and every hoisted body with the final rename map.
fn hoist_defs_into_components(
    schema: &Value,
    raw: &mut std::collections::BTreeMap<String, Value>, // unrewritten bodies, for comparison
    components: &mut serde_json::Map<String, Value>,     // rewritten bodies, published
) -> Value {
    let table: Vec<(String, Value)> = schema
        .get("$defs")
        .and_then(Value::as_object)
        .map(|t| t.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    // Pass 1: names. `pending` remembers the unrewritten body we compared
    // against so a later duplicate in the same schema compares equal.
    let mut renames: std::collections::BTreeMap<String, String> = Default::default();
    let mut pending: Vec<(String, Value)> = Vec::new();
    for (name, body) in &table {
        let target = match raw
            .get(name)
            .or_else(|| pending.iter().find(|(n, _)| n == name).map(|(_, b)| b))
        {
            None => name.clone(),
            Some(existing) if existing == body => name.clone(),
            Some(_) => format!("{name}_{}", short_sha256(&body.to_string())),
        };
        renames.insert(name.clone(), target.clone());
        pending.push((target, body.clone()));
    }

    // Pass 2: rewrite with the complete map, then publish. `entry` (rather
    // than a `contains_key` check followed by a separate `insert`) does the
    // vacancy check and the reservation in one lookup.
    for (target, body) in pending {
        if let std::collections::btree_map::Entry::Vacant(entry) = raw.entry(target.clone()) {
            components.insert(target, rewrite_local_refs(&body, &renames));
            entry.insert(body);
        }
    }
    let mut out = rewrite_local_refs(schema, &renames);
    if let Some(map) = out.as_object_mut() {
        map.remove("$defs");
    }
    out
}

/// The first 4 bytes (8 hex chars) of the SHA-256 digest of `text`. Used to
/// disambiguate two different definitions that share a `$defs` name once
/// they are hoisted into the single flat `components.schemas` namespace.
fn short_sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Rewrite every `#/$defs/X` reference in `node` to `#/components/schemas/Y`,
/// where `Y` is `X`'s entry in `renames` (or `X` itself when the name was not
/// renamed). Walks the whole tree, not just `properties`, so a `$ref` nested
/// under `oneOf`, `items`, or any other subschema-bearing keyword is caught
/// too.
fn rewrite_local_refs(node: &Value, renames: &std::collections::BTreeMap<String, String>) -> Value {
    match node {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if k == "$ref" {
                        if let Some(name) = v
                            .as_str()
                            .and_then(|r| r.strip_prefix("#/$defs/"))
                            .and_then(decode_ref_name)
                        {
                            let target = renames.get(&name).cloned().unwrap_or(name);
                            return (k.clone(), json!(format!("#/components/schemas/{target}")));
                        }
                    }
                    (k.clone(), rewrite_local_refs(v, renames))
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| rewrite_local_refs(v, renames))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Generate a full OpenAPI 3.1 JSON document from the given blocks.
pub fn generate_openapi(
    blocks: &[BlockInfo],
    project_name: &str,
    project_description: &str,
    server_url: &str,
) -> Value {
    let mut paths: serde_json::Map<String, Value> = serde_json::Map::new();

    // `hoist_defs_into_components` runs once per document build: `raw` holds
    // every hoisted definition's unrewritten body, keyed by its published
    // name, so a same-named definition met later in the walk compares
    // against what was actually published rather than re-deciding from
    // scratch; `components` holds the rewritten bodies that go out under
    // `components.schemas`.
    let mut raw: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut components: serde_json::Map<String, Value> = serde_json::Map::new();

    for block in blocks {
        for ep in &block.endpoints {
            if !ep.has_schema() {
                continue;
            }

            let mut operation: serde_json::Map<String, Value> = serde_json::Map::new();

            // summary
            operation.insert("summary".into(), json!(ep.summary));

            // description
            if !ep.description.is_empty() {
                operation.insert("description".into(), json!(ep.description));
            }

            // tags
            if !ep.tags.is_empty() {
                operation.insert("tags".into(), json!(ep.tags));
            }

            // deprecated
            if ep.deprecated {
                operation.insert("deprecated".into(), json!(true));
            }

            // requestBody from input_schema
            if let Some(input) = &ep.input_schema {
                let input = hoist_defs_into_components(input, &mut raw, &mut components);
                operation.insert(
                    "requestBody".into(),
                    json!({
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": input
                            }
                        }
                    }),
                );
            }

            // parameters from path_params and query_params
            let mut parameters: Vec<Value> = Vec::new();
            if let Some(pp) = &ep.path_params {
                let pp = hoist_defs_into_components(pp, &mut raw, &mut components);
                parameters.extend(extract_params(&pp, "path"));
            }
            if let Some(qp) = &ep.query_params {
                let qp = hoist_defs_into_components(qp, &mut raw, &mut components);
                parameters.extend(extract_params(&qp, "query"));
            }
            if !parameters.is_empty() {
                operation.insert("parameters".into(), json!(parameters));
            }

            // responses
            let response_200 = ep.output_schema.as_ref().map_or_else(
                || json!({ "description": "Successful response" }),
                |output| {
                    let output = hoist_defs_into_components(output, &mut raw, &mut components);
                    json!({
                        "description": "Successful response",
                        "content": {
                            "application/json": {
                                "schema": output
                            }
                        }
                    })
                },
            );
            operation.insert("responses".into(), json!({ "200": response_200 }));

            // security
            match ep.auth {
                AuthLevel::Authenticated | AuthLevel::Admin => {
                    operation.insert("security".into(), json!([{ "bearerAuth": [] }]));
                }
                AuthLevel::Public => {
                    // no security field
                }
            }

            let method = method_key(ep.method);
            let path_entry = paths.entry(ep.path.clone()).or_insert_with(|| json!({}));
            path_entry
                .as_object_mut()
                .unwrap()
                .insert(method.into(), Value::Object(operation));
        }
    }

    // A document with no `$defs` anywhere hoists nothing, and must come out
    // byte-identical to before this hoist existed — so `schemas` is only
    // added when there is something to publish under it.
    let mut components_obj: serde_json::Map<String, Value> = serde_json::Map::new();
    if !components.is_empty() {
        components_obj.insert("schemas".into(), Value::Object(components));
    }
    components_obj.insert(
        "securitySchemes".into(),
        json!({
            "bearerAuth": {
                "type": "http",
                "scheme": "bearer",
                "bearerFormat": "JWT"
            }
        }),
    );

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": project_name,
            "description": project_description,
            "version": "1.0.0"
        },
        "servers": [
            { "url": server_url }
        ],
        "paths": paths,
        "components": components_obj
    })
}

// ---------------------------------------------------------------------------
// generate_agent_card
// ---------------------------------------------------------------------------

/// Generate an A2A AgentCard JSON document from the given blocks.
pub fn generate_agent_card(
    blocks: &[BlockInfo],
    project_name: &str,
    project_description: &str,
    server_url: &str,
) -> Value {
    let mut skills: Vec<Value> = Vec::new();

    for block in blocks {
        for ep in &block.endpoints {
            if !ep.has_schema() {
                continue;
            }

            let path_slug = path_to_slug(&ep.path);
            let method = method_key(ep.method);
            let skill_id = format!("{}/{}_{}", block.name, method, path_slug);

            let description = if !ep.description.is_empty() {
                ep.description.clone()
            } else {
                ep.summary.clone()
            };

            let has_input = ep.input_schema.is_some();
            let has_output = ep.output_schema.is_some();

            let input_modes: Vec<&str> = if has_input {
                vec!["application/json"]
            } else {
                vec![]
            };
            let output_modes: Vec<&str> = if has_output {
                vec!["application/json"]
            } else {
                vec![]
            };

            let skill = json!({
                "id": skill_id,
                "name": ep.summary,
                "description": description,
                "tags": ep.tags,
                "input_modes": input_modes,
                "output_modes": output_modes,
            });

            skills.push(skill);
        }
    }

    json!({
        "name": project_name,
        "description": project_description,
        "version": "1.0.0",
        "supported_interfaces": [
            {
                "url": format!("{}/a2a", server_url),
                "protocol_binding": "JSONRPC",
                "protocol_version": "1.0"
            }
        ],
        "capabilities": {
            "streaming": true,
            "pushNotifications": false
        },
        "security_schemes": {
            "bearerAuth": {
                "type": "http",
                "scheme": "bearer",
                "bearerFormat": "JWT"
            }
        },
        "default_input_modes": ["application/json"],
        "default_output_modes": ["application/json"],
        "skills": skills,
    })
}

// ---------------------------------------------------------------------------
// generate_webmcp
// ---------------------------------------------------------------------------

/// `Public < Authenticated < Admin`, expressed explicitly because
/// `AuthLevel` deliberately does not derive `Ord`.
fn auth_rank(level: AuthLevel) -> u8 {
    match level {
        AuthLevel::Public => 0,
        AuthLevel::Authenticated => 1,
        AuthLevel::Admin => 2,
    }
}

/// Whether a request with this method can carry a body an agent's arguments
/// could travel in.
///
/// `GET` cannot: the Fetch standard makes constructing a `Request` with a
/// body on a `GET` or `HEAD` a `TypeError`, so a tool that declares body
/// arguments on a `GET` throws before it ever reaches the network — it fails
/// on 100% of invocations.
///
/// `DELETE` is treated as body-less too. `fetch` does not object to it, but
/// RFC 9110 §9.3.5 gives a `DELETE` payload no defined semantics,
/// intermediaries are permitted to drop it, and no endpoint in this workspace
/// declares one — so the shape has no established meaning to preserve. The
/// refusal is not silent (see [`WebMcpRefusal`]), so an author who genuinely
/// needs a `DELETE` body learns exactly why the tool is missing rather than
/// shipping one that works in one deployment and not the next.
fn method_can_carry_body(method: HttpMethod) -> bool {
    match method {
        HttpMethod::Post | HttpMethod::Patch => true,
        HttpMethod::Get | HttpMethod::Delete => false,
    }
}

/// Why one opted-in endpoint did not become a WebMCP tool.
///
/// Every variant except [`Self::DuplicateToolName`] is a defect in the
/// endpoint's own declarations, not a property of the caller. That one is
/// caller-scoped, because tool-name uniqueness is a property of a manifest
/// and a manifest is auth-filtered — see its own docs and the census in
/// [`generate_webmcp_report`].
///
/// The auth filter itself is not represented here, because hiding a tool
/// from a caller who may not invoke it is the projection working, not
/// failing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WebMcpRefusal {
    /// The tool name is not a legal MCP tool name — see
    /// `AgentTool::is_valid_name`. `BlockInfo::validate` rejects this at
    /// registration; reaching the generator means something bypassed it.
    InvalidToolName,
    /// More than one endpoint *this caller may invoke* declares this tool
    /// name, so none of them can claim it in this caller's manifest.
    ///
    /// The only caller-scoped reason there is: tool-name uniqueness is a
    /// property of a manifest rather than of the deployment, so the same
    /// endpoint can be refused here for an admin and published for an
    /// anonymous visitor. See the census in `generate_webmcp_report` for
    /// why, and for why an `Admin` ceiling still sees every collision that
    /// exists anywhere.
    ///
    /// **A runtime that went through `Wafer::seal()` never produces this.**
    /// `seal()` refuses to boot on a tool name two endpoints claim
    /// (`RuntimeError::DuplicateToolNames`), precisely because the
    /// per-manifest resolution above is caller-dependent: an agent below
    /// admin would bind the name to whichever endpoint shadows the other
    /// with no diagnostic reaching it. This variant survives as the safety
    /// net for a consumer that projects `BlockInfo`s that gate never saw.
    DuplicateToolName {
        /// How many endpoints visible to this caller declare this name.
        count: usize,
    },
    /// Property names arriving from more than one of path/query/body.
    CollidingParameterNames {
        /// The colliding names, sorted.
        names: Vec<String>,
    },
    /// Schema sources (`"path"`, `"query"`, `"body"`) that could not be
    /// honestly flattened — see `source_is_flattenable`.
    UnrepresentableSources {
        /// The offending source labels, sorted.
        sources: Vec<String>,
    },
    /// Schema sources whose inlining hit a `$ref` it could not resolve.
    UnresolvedRefs {
        /// The offending source labels, sorted.
        sources: Vec<String>,
    },
    /// Two of path/query/body carry a `$defs` entry of the same name with
    /// different bodies. One flat schema has one `$defs` table, so the tool
    /// could describe only one of them.
    CollidingDefinitions {
        /// The colliding definition names, sorted.
        names: Vec<String>,
    },
    /// Schema sources whose inlining expanded past [`MAX_INLINED_NODES`].
    /// The type is finite and every reference resolves; its definitions
    /// simply multiply out — see [`MAX_INLINED_NODES`].
    SchemaTooLarge {
        /// The offending source labels, sorted.
        sources: Vec<String>,
    },
    /// The merged schema marks names required that no source declared a
    /// property for, so the client has no way to send them.
    RequiredNotDeclared {
        /// The required names with no matching property, sorted.
        names: Vec<String>,
    },
    /// A path or query parameter is not scalar-valued, so the client cannot
    /// know how to serialize it into the URL.
    NonScalarPathOrQueryParams {
        /// The offending parameters as `"{source}.{name}"`, sorted.
        params: Vec<String>,
    },
    /// The URL path template carries braces the router will not read as a
    /// placeholder — unmatched, empty, nested, more than one in a segment,
    /// or mixed with literal text in a segment.
    MalformedPathTemplate,
    /// The URL path template contains a router wildcard segment, which no
    /// named tool argument can fill in.
    WildcardPathSegment {
        /// The offending segment, verbatim.
        segment: String,
    },
    /// The declared path parameters are not exactly the template's
    /// placeholders, so the client cannot build the URL.
    PathParamsDisagreeWithTemplate {
        /// Placeholder names found in the path template, sorted and deduped.
        placeholders: Vec<String>,
        /// Property names the path schema declared, sorted.
        declared: Vec<String>,
    },
    /// The endpoint declares body arguments on a method that cannot carry a
    /// body — see [`method_can_carry_body`].
    BodyOnBodylessMethod {
        /// The body property names that have nowhere to travel, sorted.
        body_params: Vec<String>,
    },
    /// The declared response schema does not describe a JSON *object*, so it
    /// cannot be published as an `outputSchema` — see
    /// [`agent_output_schema`].
    ///
    /// One of the four `OutputSchema*` reasons. They exist separately from
    /// the input side's otherwise-identical verdicts because the input
    /// side's message text names `inputSchema`, and telling an author who
    /// declared `.output::<T>()` that their *arguments* cannot be inlined
    /// sends them to the wrong declaration. Every one of them is only ever
    /// reported with [`WebMcpRefusalScope::OutputSchema`]: the tool is still
    /// published, without the field.
    OutputSchemaNotAnObject,
    /// The declared response schema is object-shaped but not something this
    /// projection can vouch for — a `properties` that is not an object, a
    /// `required` that is not an array of strings, an `additionalProperties`
    /// other than `false`, or a top-level keyword outside
    /// [`FLATTENABLE_KEYWORDS`]. See [`agent_output_schema`].
    OutputSchemaUnrepresentable,
    /// Inlining the declared response schema hit a `$ref` it could not
    /// resolve, which would leave an unconstrained `{}` where the endpoint
    /// returns a specific type.
    OutputSchemaUnresolvedRef,
    /// Inlining the declared response schema expanded past
    /// [`MAX_INLINED_NODES`] and stopped partway, so what is in hand is a
    /// truncated document rather than a weaker one.
    OutputSchemaTooLarge,
}

/// What a [`WebMcpRefusalReport`] is about: the whole tool, or one optional
/// part of it that was dropped while the tool was published anyway.
///
/// The distinction exists because the two are not equally costly, and the
/// projection should refuse the *smallest* thing that can carry the lie. See
/// [`agent_output_schema`] for the argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WebMcpRefusalScope {
    /// No tool was published for this endpoint.
    Tool,
    /// The tool was published, but without its `outputSchema`.
    OutputSchema,
}

impl std::fmt::Display for WebMcpRefusalScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool => f.write_str("tool"),
            Self::OutputSchema => f.write_str("outputSchema"),
        }
    }
}

impl std::fmt::Display for WebMcpRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidToolName => write!(
                f,
                "tool name is not a legal MCP tool name (1-{max} characters of [A-Za-z0-9_-])",
                max = AgentTool::MAX_NAME_LEN
            ),
            Self::DuplicateToolName { count } => write!(
                f,
                "tool name is claimed by {count} endpoints this caller may invoke, so none of them can claim it in this caller's manifest"
            ),
            Self::CollidingParameterNames { names } => write!(
                f,
                "parameter name(s) {} arrive from more than one of path/query/body and cannot be described by one flat schema",
                names.join(", ")
            ),
            Self::UnrepresentableSources { sources } => write!(
                f,
                "schema source(s) {} cannot be flattened into the tool's object-shaped inputSchema without losing constraints",
                sources.join(", ")
            ),
            Self::UnresolvedRefs { sources } => write!(
                f,
                "schema source(s) {} contain a $ref that could not be resolved, leaving an unconstrained {{}} where the server requires a type",
                sources.join(", ")
            ),
            Self::CollidingDefinitions { names } => write!(
                f,
                "definition name(s) {} are defined differently by more than one of path/query/body, and the merged inputSchema has one $defs table, so it could describe only one of them",
                names.join(", ")
            ),
            Self::SchemaTooLarge { sources } => write!(
                f,
                "schema source(s) {} expand past {MAX_INLINED_NODES} JSON nodes when their $refs are inlined — nothing is missing and every reference resolves, but a definition reached from several levels is copied out once per path, so the self-contained inputSchema has no workable size",
                sources.join(", ")
            ),
            Self::RequiredNotDeclared { names } => write!(
                f,
                "required name(s) {} have no matching property in any of path/query/body, so the client has nowhere to send them",
                names.join(", ")
            ),
            Self::NonScalarPathOrQueryParams { params } => write!(
                f,
                "path/query parameter(s) {} are not scalar-valued, and the invocation carries no serialization style the client could use to put them in a URL",
                params.join(", ")
            ),
            Self::MalformedPathTemplate => f.write_str(
                "path template has a {} placeholder that is unmatched, empty, nested, or does not span a whole '/'-separated segment — the router only substitutes a placeholder that is an entire segment",
            ),
            Self::WildcardPathSegment { segment } => write!(
                f,
                "path template contains the router wildcard segment '{segment}', which no named tool argument can fill in"
            ),
            Self::PathParamsDisagreeWithTemplate {
                placeholders,
                declared,
            } => write!(
                f,
                "path template placeholders [{}] do not match the declared path params [{}], so the request URL cannot be built",
                placeholders.join(", "),
                declared.join(", ")
            ),
            Self::BodyOnBodylessMethod { body_params } => write!(
                f,
                "body parameter(s) {} are declared on a method that cannot carry a request body",
                body_params.join(", ")
            ),
            Self::OutputSchemaNotAnObject => f.write_str(
                "the declared output schema does not describe a JSON object, and a tool's outputSchema describes a structured result, which is always an object",
            ),
            Self::OutputSchemaUnrepresentable => f.write_str(
                "the declared output schema's top level carries a keyword this projection cannot account for, or a malformed 'properties'/'required'/'additionalProperties' value, so it cannot be published as an outputSchema this tool vouches for",
            ),
            Self::OutputSchemaUnresolvedRef => f.write_str(
                "the declared output schema contains a $ref that could not be resolved, leaving an unconstrained {} in the outputSchema where the endpoint returns a specific type",
            ),
            Self::OutputSchemaTooLarge => write!(
                f,
                "the declared output schema expands past {MAX_INLINED_NODES} JSON nodes when its $refs are inlined — nothing is missing and every reference resolves, but a definition reached from several levels is copied out once per path, so the self-contained outputSchema has no workable size"
            ),
        }
    }
}

/// One thing the projection refused to publish for an endpoint that opted in
/// to agent-tool exposure, named precisely enough to find in source.
///
/// [`Self::scope`] says what was refused. Most entries refuse the whole tool;
/// an entry scoped to [`WebMcpRefusalScope::OutputSchema`] means the tool was
/// published and only its `outputSchema` was dropped.
///
/// `#[non_exhaustive]`: the fields are a diagnostic record whose shape is
/// expected to grow, and a consumer must never be able to exhaustively
/// destructure one — a new field would then be a silent behaviour change at
/// the consumer rather than a compile error here.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WebMcpRefusalReport {
    /// Name of the block declaring the endpoint.
    pub block: String,
    /// HTTP method of the refused endpoint.
    pub method: HttpMethod,
    /// URL path of the refused endpoint.
    pub path: String,
    /// The tool name the endpoint asked for.
    pub tool_name: String,
    /// What was refused: the whole tool, or one optional part of it.
    pub scope: WebMcpRefusalScope,
    /// Why it was refused.
    pub reason: WebMcpRefusal,
    /// Whether the caller this report was generated for may invoke the
    /// endpoint this entry is about.
    ///
    /// Structural refusals are deliberately reported to *every* caller — a
    /// malformed path template is a defect whether an anonymous visitor or
    /// an admin asked, and an author debugging one should not have to
    /// authenticate to see it. That is right for a log, and wrong for any
    /// consumer that renders a refusal list *as one caller sees it*: naming
    /// an admin-only endpoint's block, method, path and tool name under a
    /// "Public" heading discloses across the tier the auth filter exists to
    /// separate, and misdescribes what that caller receives.
    ///
    /// Such a consumer filters on this flag rather than re-deriving the auth
    /// decision, which would be a second implementation of a
    /// security-critical filter. Entries scoped to
    /// [`WebMcpRefusalScope::OutputSchema`] and
    /// [`WebMcpRefusal::DuplicateToolName`] are always `true`: both are
    /// decided after the auth filter.
    pub visible_to_caller: bool,
}

impl std::fmt::Display for WebMcpRefusalReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "block '{}' endpoint {} {} (tool '{}'): ",
            self.block, self.method, self.path, self.tool_name
        )?;
        match self.scope {
            WebMcpRefusalScope::Tool => write!(f, "{}", self.reason),
            WebMcpRefusalScope::OutputSchema => {
                write!(f, "published without outputSchema — {}", self.reason)
            }
        }
    }
}

/// Project the blocks' endpoint declarations into a WebMCP tool manifest,
/// filtered to what `caller` is allowed to invoke.
///
/// This is the third projection of `BlockInfo::endpoints`, alongside
/// [`generate_openapi`] and [`generate_agent_card`]. Two things make it
/// different from those:
///
/// * **Opt-in.** Only endpoints carrying `AgentTool` metadata appear.
///   Carrying a schema is not consent to being called by an agent.
/// * **Auth-filtered.** Tools above `caller`'s level are omitted entirely —
///   not marked unavailable. A name an agent cannot use is recon surface, so
///   it never reaches the page. This mirrors the SEC-073 posture applied to
///   the discovery documents.
///
/// Each emitted tool carries `name`, `description`, `inputSchema`, and
/// `invocation`, plus `outputSchema` when the endpoint declares a response
/// schema this projection can vouch for (see [`agent_output_schema`]) and
/// `deprecated` when the endpoint is.
///
/// # `effective_auth` is required because declared auth is not enforced auth
///
/// `effective_auth` is asked, for one block and one of its endpoints, what
/// access level the consumer's router will actually enforce on that route.
/// For a router that mounts blocks under access-tiered prefixes that is
/// `max(prefix_tier, ep.auth)`, not `ep.auth` alone.
///
/// It is an argument rather than a default because only the consumer knows
/// its prefix table, and getting it wrong is not a cosmetic error in either
/// direction. A `Public` endpoint mounted under an admin-only prefix, judged
/// by `ep.auth`, is advertised to anonymous callers and rejected on every
/// call — the recon surface the auth filter exists to remove. A stricter
/// `ep.auth` on a route the consumer serves publicly hides a tool that is
/// genuinely reachable.
///
/// A consumer whose routing cannot raise an endpoint's declared level may
/// use [`generate_webmcp_declared_auth`], which is this function with
/// `|_, ep| ep.auth`. Its name says at the call site which claim is being
/// made, so a reviewer can check it without opening the docs.
///
/// Every endpoint this refuses is logged at `warn!` with the block, method,
/// path, tool name, scope, and reason. Use [`generate_webmcp_report`] to
/// receive the refusals as data instead.
pub fn generate_webmcp(
    blocks: &[BlockInfo],
    caller: AuthLevel,
    effective_auth: impl Fn(&BlockInfo, &BlockEndpoint) -> AuthLevel,
) -> Value {
    let (manifest, refused) = generate_webmcp_report(blocks, caller, effective_auth);
    for refusal in &refused {
        tracing::warn!(
            block = %refusal.block,
            method = %refusal.method,
            path = %refusal.path,
            tool = %refusal.tool_name,
            scope = %refusal.scope,
            reason = %refusal.reason,
            "webmcp: endpoint opted in to agent-tool exposure but was refused"
        );
    }
    manifest
}

/// [`generate_webmcp`] for a consumer whose routing never raises an
/// endpoint's declared access level: `ep.auth` is taken as the level the
/// router will enforce.
///
/// # Read this as a claim, not a default
///
/// This function is the whole of `generate_webmcp(blocks, caller, |_, ep|
/// ep.auth)`, and it exists only so that the claim it makes is written at
/// the call site. It used to be the one *called* `generate_webmcp` — the
/// short, autocomplete-first name — while the resolver-taking form was
/// `generate_webmcp_with`. That put the looser filter on the obvious name,
/// so a consumer that reached for it got weaker auth filtering silently, and
/// the one in-tree consumer with a prefix-tiered router needed a ten-line
/// comment to stop a future editor doing exactly that. The names are now the
/// other way round: the safe call is the short one, and this one says what
/// it assumes.
///
/// Do not use it if anything between the request and the block can raise the
/// enforced level — an access-tiered mount prefix, a middleware that gates a
/// path family, a route table that re-declares auth. In any of those cases
/// the declared level is not the enforced one and this will publish tools
/// that fail on every call, or hide tools that would have worked. See
/// [`generate_webmcp`].
pub fn generate_webmcp_declared_auth(blocks: &[BlockInfo], caller: AuthLevel) -> Value {
    generate_webmcp(blocks, caller, |_block, ep| ep.auth)
}

/// [`generate_webmcp`], returning the refused endpoints alongside the
/// manifest instead of logging them.
///
/// # Why refusals are not in the manifest
///
/// The refusal list is deliberately a *second* return value rather than a
/// section of the served document. A caller who cannot see a tool must not be
/// told that a tool they cannot see exists, let alone what is wrong with it;
/// that is the same existence-oracle the auth filter is there to close. The
/// refusals are for the operator's logs and for tests, and the manifest is
/// for the agent.
///
/// # Refusals are the same for every caller — with two exceptions
///
/// Every *tool-scoped* refusal reason except
/// [`WebMcpRefusal::DuplicateToolName`] is a defect in the endpoint's own
/// declarations, so the structural checks run *before* the auth filter and
/// that part of the returned list does not depend on `caller`. A broken
/// admin endpoint is broken when an anonymous visitor loads the manifest
/// too, and an author debugging it should not have to guess which caller
/// makes the diagnostic appear. The auth filter itself is not a refusal —
/// omitting a tool the caller may not invoke is the projection working — so
/// it is reported nowhere.
///
/// Those entries do carry [`WebMcpRefusalReport::visible_to_caller`], and a
/// consumer that renders a refusal list as one caller's own view of the
/// deployment must filter on it. Reporting to every caller is right for a
/// log and wrong for a page that claims to show what a given tier receives.
///
/// The two exceptions run *after* the auth filter, so they only ever
/// describe an endpoint this caller can see:
///
/// * A **duplicate name** is not a defect in one endpoint; it is a property
///   of the manifest the two endpoints land in, and that manifest is
///   auth-filtered. Counting names across endpoints the caller cannot see
///   would make a missing tool an oracle for a higher-privilege endpoint's
///   existence — the same recon surface the auth filter exists to close. So
///   the census is scoped to the caller, and collisions are reported for the
///   callers in whose manifest they actually occur. Because the auth filter
///   is monotone, a report taken at `AuthLevel::Admin` still contains every
///   collision that exists anywhere, which is what a boot-time diagnostic
///   pass should ask for. (A runtime sealed by `Wafer::seal()` has none —
///   see [`WebMcpRefusal::DuplicateToolName`].)
/// * A **dropped `outputSchema`** is a defect in the endpoint's declaration,
///   but the entry says "published without outputSchema", and that is only
///   true where the tool was in fact published. Recorded ahead of the two
///   gates above it would claim a field is missing from a tool that is not
///   in the manifest at all.
///
/// # Not every refusal costs the tool
///
/// A report entry carries a [`WebMcpRefusalScope`]. Most say the endpoint
/// produced no tool at all; one — [`WebMcpRefusalScope::OutputSchema`] —
/// says the tool *was* published and only its `outputSchema` was dropped.
/// A consumer that counts refusals as missing tools must filter on the
/// scope. See [`agent_output_schema`] for why that case refuses a field
/// rather than a tool.
pub fn generate_webmcp_report(
    blocks: &[BlockInfo],
    caller: AuthLevel,
    effective_auth: impl Fn(&BlockInfo, &BlockEndpoint) -> AuthLevel,
) -> (Value, Vec<WebMcpRefusalReport>) {
    use WebMcpRefusalScope as Scope;

    let ceiling = auth_rank(caller);

    let mut refused: Vec<WebMcpRefusalReport> = Vec::new();

    // A WebMCP client registers tools by name, so two endpoints sharing a
    // name are ambiguous no matter which one "wins", and neither may claim
    // it.
    //
    // # This is a safety net that should never fire
    //
    // Everything below describes what happens *if* a duplicate name reaches
    // this function, and in a runtime that went through `Wafer::seal()` none
    // ever does: `seal()` counts tool names across every registered block
    // and refuses to boot on a collision
    // (`RuntimeError::DuplicateToolNames`). The reason the gate is there
    // rather than here is the whole "what it does cost" section further
    // down — every runtime resolution of an ambiguous name is bad for
    // somebody, and the per-manifest one is bad for the caller least able to
    // notice. A name that cannot be deployed twice needs no runtime
    // resolution at all.
    //
    // The census stays because this function is a pure projection of
    // `BlockInfo`s and does not know where they came from. A consumer can
    // hand it declarations that never passed through `seal()` — a
    // hand-assembled `Vec<BlockInfo>`, an inspector view, a test — and in
    // that case an ambiguous name must still not be published as though it
    // were unique.
    //
    // # Uniqueness is a property of a manifest, not of the deployment
    //
    // The question this census answers is which of two readings of
    // "unique" the projection enforces:
    //
    // * **Global** — a name claimed anywhere is claimed everywhere, so
    //   counting runs over every opted-in endpoint regardless of who asked.
    // * **Per-manifest** — a name is unique if it is unambiguous *in the
    //   document this caller receives*, so counting runs over the endpoints
    //   this caller may invoke.
    //
    // This counts per manifest, because global counting leaks. An
    // admin-only endpoint that collides with a public one suppresses the
    // public tool for everyone, so an anonymous caller who knows the public
    // block — its source is not a secret — fetches the manifest, finds
    // `get_thing` missing, and has learned that some endpoint they cannot
    // reach claims that name. The auth filter four lines below exists
    // precisely so that a name an agent cannot use never reaches the page;
    // under global counting the *absence* of a name leaks the same fact.
    // Nothing about a caller's manifest may depend on an endpoint that
    // caller cannot see, and this is the last place it did.
    //
    // # What per-manifest counting does not cost
    //
    // The review that introduced global counting worried that a name could
    // then mean different things to different callers. It cannot, and the
    // reason is that the auth filter is monotone in the caller's rank: a
    // higher tier sees a superset of a lower tier's endpoints. If a name is
    // unique at some tier, its one claimant is visible there; at any lower
    // tier the candidate set only shrinks, so that name is either still
    // claimed by the same endpoint or claimed by nobody. A name therefore
    // never denotes two different endpoints across two manifests — it is
    // present or absent, never ambiguous.
    //
    // Order-independence is unchanged: counting first and emitting only what
    // turned out unique cannot depend on declaration order, unlike dropping
    // the later duplicate.
    //
    // Laundering is unchanged too, and is the reason this census is scoped
    // by auth *only*. It runs before every structural verdict below, so an
    // endpoint that will be refused for a malformed path or an
    // unrepresentable body still spends its claim on the name. Otherwise
    // dropping the broken side would let the survivor silently inherit the
    // name as though it had always been unique, and repairing the broken
    // endpoint later would silently change what the name means.
    //
    // # What it does cost
    //
    // Cross-tier monotonicity of the tool set. A public caller can now
    // receive a `get_thing` that an admin — who sees both claimants — does
    // not. That reads backwards, and it is the honest answer: at the admin's
    // tier the name really is ambiguous, and at the anonymous visitor's tier
    // it really is not. The cost also lands in the right place, on the
    // operator who can see the refusal log and fix the collision, rather
    // than on the visitor who can do neither. Because the filter is
    // monotone, an `Admin` census sees every collision that exists anywhere,
    // so boot-time diagnostics generated at that ceiling lose nothing.
    //
    // Names that are not legal MCP tool names are excluded from the count.
    // They are refused on their own terms below, and counting them would let
    // one defect cause another: two endpoints that both forgot a name would
    // share the empty-string bucket, and every future endpoint that also
    // forgot one would keep that bucket above 1 — a `DuplicateToolName`
    // verdict that says nothing true about either endpoint.
    let mut name_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for block in blocks {
        for ep in &block.endpoints {
            if let Some(tool) = ep.agent_tool.as_ref() {
                if AgentTool::is_valid_name(&tool.name)
                    && auth_rank(effective_auth(block, ep)) <= ceiling
                {
                    *name_counts.entry(tool.name.as_str()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut tools: Vec<Value> = Vec::new();

    for block in blocks {
        for ep in &block.endpoints {
            let Some(tool) = ep.agent_tool.as_ref() else {
                continue;
            };

            // Resolved here rather than at the filter below because every
            // refusal recorded on the way there has to carry it: a consumer
            // rendering "what this caller receives" must be able to drop the
            // entries about endpoints this caller cannot see, and it must
            // not re-derive that decision itself. The filter below still
            // decides the manifest; this only records what it will decide.
            let visible_to_caller = auth_rank(effective_auth(block, ep)) <= ceiling;

            let mut refuse = |scope: Scope, reason: WebMcpRefusal| {
                refused.push(WebMcpRefusalReport {
                    block: block.name.clone(),
                    method: ep.method,
                    path: ep.path.clone(),
                    tool_name: tool.name.clone(),
                    scope,
                    reason,
                    visible_to_caller,
                });
            };

            // `BlockInfo::validate` rejects an illegal name at registration,
            // so reaching here means a block was constructed without going
            // through it. Skipping defensively keeps a name an MCP client
            // would reject — and which the consumer's per-tool try/catch
            // would swallow — out of the manifest.
            if !AgentTool::is_valid_name(&tool.name) {
                refuse(Scope::Tool, WebMcpRefusal::InvalidToolName);
                continue;
            }

            let input = agent_input_schema(ep);

            // A property name arriving from two of path/query/body cannot be
            // honestly described by one flat schema, and the client would put
            // the value in both places. Emitting no tool is the safe, visible
            // failure; emitting a lying one is neither.
            if !input.collisions.is_empty() {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::CollidingParameterNames {
                        names: input.collisions.clone(),
                    },
                );
                continue;
            }

            // Inlining ran past its node budget and stopped partway, so the
            // schema in hand is a truncated document. This is checked before
            // every schema-shaped verdict below because those all read the
            // inlined schema, and a truncated one produces verdicts about a
            // schema the endpoint never declared — `{}` where a whole
            // subtree was cut, which `source_is_flattenable` would report as
            // an unrepresentable source and send the author looking at the
            // wrong thing. See `MAX_INLINED_NODES`.
            if !input.oversized_schemas.is_empty() {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::SchemaTooLarge {
                        sources: input.oversized_schemas.clone(),
                    },
                );
                continue;
            }

            // A source that cannot be honestly flattened into the
            // object-shaped `inputSchema` a tool exposes — a tagged-enum
            // body, an array body, a composition keyword sitting beside
            // `properties`, an open map the invocation cannot route. A tool
            // that misdescribes the arguments its source really takes is
            // exactly the kind of lie `collisions` above already refuses to
            // tell. See `source_is_flattenable`.
            if !input.unrepresentable.is_empty() {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::UnrepresentableSources {
                        sources: input.unrepresentable.clone(),
                    },
                );
                continue;
            }

            // A `$ref` that did not resolve leaves `{}` — "send anything" —
            // where the server requires a specific type. Below the top level
            // that is invisible to the check above: the schema still has its
            // `properties` object, one entry of which now accepts garbage.
            // Same lie, quieter.
            if !input.unresolved_refs.is_empty() {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::UnresolvedRefs {
                        sources: input.unresolved_refs.clone(),
                    },
                );
                continue;
            }

            // Two sources that each kept a cyclic definition of the same
            // name, with different bodies. The merged schema is one document
            // with one `$defs` table, so describing one of them means every
            // back-edge the other contributed resolves to the wrong type —
            // the same lie as a colliding parameter name, one level down.
            //
            // Checked here rather than beside `CollidingParameterNames`
            // because it is read off the *inlined* schema: past the node
            // budget the walk stopped partway, and a truncated document's
            // partial tables would report a collision that the endpoint does
            // not have.
            if !input.colliding_defs.is_empty() {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::CollidingDefinitions {
                        names: input.colliding_defs.clone(),
                    },
                );
                continue;
            }

            // `required` names with no property behind them. The client
            // builds its arguments from the three provenance lists, so it
            // has no slot to put such a name in — while a strict MCP client
            // reads the schema and makes the model supply it anyway. Every
            // call then reaches the server missing the data it demands.
            // Filtering the names out of `required` instead would swap one
            // lie for another: the server's own schema still requires them.
            if !input.undeclared_required.is_empty() {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::RequiredNotDeclared {
                        names: input.undeclared_required.clone(),
                    },
                );
                continue;
            }

            // A path or query parameter that is not a single scalar has no
            // agreed URL serialization, and `invocation` carries no
            // `style`/`explode` to settle it. See `param_is_scalar_valued`.
            if !input.non_scalar_params.is_empty() {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::NonScalarPathOrQueryParams {
                        params: input.non_scalar_params.clone(),
                    },
                );
                continue;
            }

            // `invocation.path` is handed to the client verbatim, so every
            // `{placeholder}` in it must have a declared path param to fill
            // it, and every declared path param must have a placeholder to
            // go into. An unfilled placeholder means the client GETs a
            // literal `{product_id}` and 404s forever; a declared param with
            // nowhere to go means an argument the agent supplies is silently
            // dropped. A tool whose URL can never be built is a lie about
            // what it does, so it is not published at all.
            let mut placeholders = match path_placeholders(&ep.path) {
                Ok(placeholders) => placeholders,
                Err(PathTemplateError::Malformed) => {
                    refuse(Scope::Tool, WebMcpRefusal::MalformedPathTemplate);
                    continue;
                }
                Err(PathTemplateError::Wildcard { segment }) => {
                    refuse(Scope::Tool, WebMcpRefusal::WildcardPathSegment { segment });
                    continue;
                }
            };
            placeholders.sort();
            placeholders.dedup();
            if placeholders != input.path_params {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::PathParamsDisagreeWithTemplate {
                        placeholders,
                        declared: input.path_params.clone(),
                    },
                );
                continue;
            }

            // Body arguments on a method that cannot carry a body have
            // nowhere to travel: a client that honours `body_params` builds a
            // request the Fetch standard refuses to construct, and one that
            // quietly drops them sends a request the server rejects for
            // missing data. Either way the tool fails on every invocation, so
            // it is not published. See `method_can_carry_body`.
            if !input.body_params.is_empty() && !method_can_carry_body(ep.method) {
                refuse(
                    Scope::Tool,
                    WebMcpRefusal::BodyOnBodylessMethod {
                        body_params: input.body_params.clone(),
                    },
                );
                continue;
            }

            // Everything above is a defect in the endpoint. This is not: a
            // tool the caller may not invoke is omitted, silently and by
            // design. See the "Refusals are the same for every caller"
            // section.
            if !visible_to_caller {
                continue;
            }

            // Uniqueness is scoped to this manifest, so the check belongs
            // after the auth filter that defines it — see the census above.
            // The count is over endpoints this caller may invoke, so a
            // filtered-out endpoint has no count and must not be asked for
            // one; and running the structural checks first means an endpoint
            // that is both broken and duplicated is reported by the defect
            // its author can actually fix, keeping every caller-independent
            // verdict ahead of the one caller-scoped verdict there is.
            let count = name_counts.get(tool.name.as_str()).copied().unwrap_or(0);
            if count != 1 {
                refuse(Scope::Tool, WebMcpRefusal::DuplicateToolName { count });
                continue;
            }

            // The one verdict that does not take the tool down with it: a
            // response schema this cannot vouch for costs the `outputSchema`
            // field and nothing else. See `agent_output_schema` for why the
            // unit of refusal is smaller here than everywhere above.
            //
            // Last, and specifically *after* both caller-scoped gates above,
            // because a field-scoped refusal only means anything when the
            // field's tool actually ships. "Published without outputSchema"
            // is a false statement about an endpoint the caller cannot see,
            // and about one whose name collided — in both cases no tool was
            // published at all, and a report that says otherwise sends its
            // reader looking for a tool that is not in the manifest. The
            // verdict itself is still a property of the declaration alone,
            // so it reads the same for every caller who receives the tool.
            let output = agent_output_schema(ep);
            if let Some(reason) = output.dropped {
                refuse(Scope::OutputSchema, reason);
            }

            // A deprecated endpoint still works, so it is still published —
            // a missing tool helps no one — but the signal travels with it.
            // The machine-readable `deprecated` flag is what a client should
            // key off; the description prefix is what actually reaches a
            // model, since clients routinely forward only name, description,
            // and inputSchema.
            let description = if ep.deprecated {
                format!("[Deprecated] {}", tool.description)
            } else {
                tool.description.clone()
            };

            let mut emitted = serde_json::Map::new();
            emitted.insert("name".into(), json!(tool.name));
            emitted.insert("description".into(), json!(description));
            if ep.deprecated {
                emitted.insert("deprecated".into(), json!(true));
            }
            emitted.insert("inputSchema".into(), input.schema);
            if let Some(schema) = output.schema {
                emitted.insert("outputSchema".into(), schema);
            }
            emitted.insert(
                "invocation".into(),
                json!({
                    "method": method_key(ep.method),
                    "path": ep.path,
                    "path_params": input.path_params,
                    "query_params": input.query_params,
                    "body_params": input.body_params,
                }),
            );
            tools.push(Value::Object(emitted));
        }
    }

    (
        json!({
            "schema_version": 1,
            "tools": tools,
        }),
        refused,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn test_block() -> BlockInfo {
        BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "A test block")
            .description("Test block for unit tests")
            .endpoints(vec![
                BlockEndpoint::post("/b/test/api/login")
                    .summary("Login")
                    .description("Authenticate with credentials")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({"type": "object", "properties": {"email": {"type": "string"}, "password": {"type": "string"}}, "required": ["email", "password"]}))
                    .output_schema(json!({"type": "object", "properties": {"token": {"type": "string"}}}))
                    .tags(&["auth"]),
                BlockEndpoint::get("/b/test/api/me")
                    .summary("Get current user")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(json!({"type": "object", "properties": {"id": {"type": "string"}, "email": {"type": "string"}}}))
                    .tags(&["auth", "users"]),
                BlockEndpoint::get("/b/test/health")
                    .summary("Health check"),
            ])
    }

    // 1. openapi_basic_structure
    #[test]
    fn openapi_basic_structure() {
        let block = test_block();
        let doc = generate_openapi(
            &[block],
            "My Project",
            "A test project",
            "https://example.com",
        );

        assert_eq!(doc["openapi"], "3.1.0");
        assert_eq!(doc["info"]["title"], "My Project");
        assert_eq!(doc["info"]["description"], "A test project");
        assert_eq!(doc["info"]["version"], "1.0.0");
        assert_eq!(doc["servers"][0]["url"], "https://example.com");
    }

    // 2. openapi_includes_schema_endpoints_only
    #[test]
    fn openapi_includes_schema_endpoints_only() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        // /b/test/health has no schema — should not appear
        assert!(
            doc["paths"].get("/b/test/health").is_none(),
            "health endpoint (no schema) should be excluded"
        );
        // /b/test/api/login has input + output schema
        assert!(
            doc["paths"].get("/b/test/api/login").is_some(),
            "login endpoint should be included"
        );
        // /b/test/api/me has output schema
        assert!(
            doc["paths"].get("/b/test/api/me").is_some(),
            "me endpoint should be included"
        );
    }

    // 3. openapi_post_has_request_body
    #[test]
    fn openapi_post_has_request_body() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let op = &doc["paths"]["/b/test/api/login"]["post"];
        assert!(
            !op["requestBody"].is_null(),
            "POST with input_schema should have requestBody"
        );
        assert_eq!(
            op["requestBody"]["content"]["application/json"]["schema"]["type"],
            "object"
        );
    }

    // 4. openapi_get_has_response_schema
    #[test]
    fn openapi_get_has_response_schema() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let op = &doc["paths"]["/b/test/api/me"]["get"];
        let schema = &op["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(schema["type"], "object");
    }

    // 5. openapi_auth_sets_security
    #[test]
    fn openapi_auth_sets_security() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        // Public endpoint: no security field
        let login_op = &doc["paths"]["/b/test/api/login"]["post"];
        assert!(
            login_op.get("security").is_none(),
            "Public endpoint should not have a security field"
        );

        // Authenticated endpoint: bearerAuth
        let me_op = &doc["paths"]["/b/test/api/me"]["get"];
        assert_eq!(
            me_op["security"][0]["bearerAuth"],
            json!([]),
            "Authenticated endpoint should have bearerAuth security"
        );
    }

    // 6. openapi_tags_propagated
    #[test]
    fn openapi_tags_propagated() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let login_tags = &doc["paths"]["/b/test/api/login"]["post"]["tags"];
        assert_eq!(*login_tags, json!(["auth"]));

        let me_tags = &doc["paths"]["/b/test/api/me"]["get"]["tags"];
        assert_eq!(*me_tags, json!(["auth", "users"]));
    }

    // 7. openapi_security_scheme_present
    #[test]
    fn openapi_security_scheme_present() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let bearer = &doc["components"]["securitySchemes"]["bearerAuth"];
        assert_eq!(bearer["type"], "http");
        assert_eq!(bearer["scheme"], "bearer");
    }

    // 7b. openapi_hoists_defs_into_components_and_rewrites_refs
    #[test]
    fn openapi_hoists_defs_into_components_and_rewrites_refs() {
        let blocks = vec![
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
            BlockEndpoint::post("/b/test/offers").auth(AuthLevel::Public).input_schema(json!({
                "type": "object",
                "properties": { "condition": { "$ref": "#/$defs/Condition" } },
                "$defs": { "Condition": { "type": "object", "properties": {
                    "all": { "type": "array", "items": { "$ref": "#/$defs/Condition" } } } } }
            })),
        ]),
        ];
        let doc = generate_openapi(&blocks, "t", "t", "https://x.test");
        let schema = &doc["paths"]["/b/test/offers"]["post"]["requestBody"]["content"]
            ["application/json"]["schema"];
        assert!(schema.get("$defs").is_none(), "{schema}");
        assert_eq!(
            schema["properties"]["condition"],
            json!({ "$ref": "#/components/schemas/Condition" })
        );
        assert_eq!(
            doc["components"]["schemas"]["Condition"]["properties"]["all"]["items"],
            json!({ "$ref": "#/components/schemas/Condition" })
        );
        let text = doc.to_string();
        assert!(!text.contains("#/$defs/"), "no dangling local refs: {text}");
    }

    // 7c. openapi_without_defs_is_byte_identical_to_before_the_hoist
    #[test]
    fn openapi_without_defs_is_byte_identical_to_before_the_hoist() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "d", "https://x.com");

        assert_eq!(
            doc,
            json!({
                "openapi": "3.1.0",
                "info": {
                    "title": "P",
                    "description": "d",
                    "version": "1.0.0"
                },
                "servers": [
                    { "url": "https://x.com" }
                ],
                "paths": {
                    "/b/test/api/login": {
                        "post": {
                            "summary": "Login",
                            "description": "Authenticate with credentials",
                            "tags": ["auth"],
                            "requestBody": {
                                "required": true,
                                "content": {
                                    "application/json": {
                                        "schema": {
                                            "type": "object",
                                            "properties": {
                                                "email": { "type": "string" },
                                                "password": { "type": "string" }
                                            },
                                            "required": ["email", "password"]
                                        }
                                    }
                                }
                            },
                            "responses": {
                                "200": {
                                    "description": "Successful response",
                                    "content": {
                                        "application/json": {
                                            "schema": {
                                                "type": "object",
                                                "properties": {
                                                    "token": { "type": "string" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "/b/test/api/me": {
                        "get": {
                            "summary": "Get current user",
                            "tags": ["auth", "users"],
                            "responses": {
                                "200": {
                                    "description": "Successful response",
                                    "content": {
                                        "application/json": {
                                            "schema": {
                                                "type": "object",
                                                "properties": {
                                                    "id": { "type": "string" },
                                                    "email": { "type": "string" }
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            "security": [{ "bearerAuth": [] }]
                        }
                    }
                },
                "components": {
                    "securitySchemes": {
                        "bearerAuth": {
                            "type": "http",
                            "scheme": "bearer",
                            "bearerFormat": "JWT"
                        }
                    }
                }
            }),
            "hoisting $defs into components is additive: a document with no \
             $defs anywhere must come out byte-identical to before: {doc}"
        );
    }

    // 7d. openapi_disambiguates_same_named_defs_with_different_bodies
    #[test]
    fn openapi_disambiguates_same_named_defs_with_different_bodies() {
        let blocks = vec![
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/test/a")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "c": { "$ref": "#/$defs/Condition" } },
                        "$defs": { "Condition": { "type": "string" } }
                    })),
                BlockEndpoint::post("/b/test/b")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "c": { "$ref": "#/$defs/Condition" } },
                        "$defs": { "Condition": { "type": "integer" } }
                    })),
            ]),
        ];
        let doc = generate_openapi(&blocks, "t", "t", "https://x.test");

        // The first-seen body keeps the bare name.
        assert_eq!(
            doc["components"]["schemas"]["Condition"],
            json!({ "type": "string" })
        );
        let a_ref = &doc["paths"]["/b/test/a"]["post"]["requestBody"]["content"]
            ["application/json"]["schema"]["properties"]["c"];
        assert_eq!(a_ref, &json!({ "$ref": "#/components/schemas/Condition" }));

        // The differently-shaped second definition gets a content-hash suffix
        // rather than clobbering or being dropped.
        let b_ref = &doc["paths"]["/b/test/b"]["post"]["requestBody"]["content"]
            ["application/json"]["schema"]["properties"]["c"];
        let b_ref_target = b_ref["$ref"].as_str().expect("a $ref string");
        assert_ne!(b_ref_target, "#/components/schemas/Condition");
        let prefix = "#/components/schemas/Condition_";
        assert!(b_ref_target.starts_with(prefix), "{b_ref_target}");
        let suffix = &b_ref_target[prefix.len()..];
        assert_eq!(suffix.len(), 8, "8 hex chars: {b_ref_target}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "{b_ref_target}"
        );
        let hashed_name = &b_ref_target["#/components/schemas/".len()..];
        assert_eq!(
            doc["components"]["schemas"][hashed_name],
            json!({ "type": "integer" })
        );

        assert_eq!(doc["components"]["schemas"].as_object().unwrap().len(), 2);
    }

    // 7e. openapi_hoists_defs_from_path_query_and_output_schemas_too
    #[test]
    fn openapi_hoists_defs_from_path_query_and_output_schemas_too() {
        let blocks = vec![
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/test/items/{id}")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "id": { "$ref": "#/$defs/Id" } },
                        "required": ["id"],
                        "$defs": { "Id": { "type": "string" } }
                    }))
                    .query_params_schema(json!({
                        "type": "object",
                        "properties": { "filter": { "$ref": "#/$defs/Filter" } },
                        "$defs": { "Filter": { "type": "string" } }
                    }))
                    .output_schema(json!({
                        "type": "object",
                        "properties": { "item": { "$ref": "#/$defs/Item" } },
                        "$defs": { "Item": { "type": "object" } }
                    })),
            ]),
        ];
        let doc = generate_openapi(&blocks, "t", "t", "https://x.test");
        let op = &doc["paths"]["/b/test/items/{id}"]["get"];

        let id_param = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "id")
            .expect("id path param");
        assert_eq!(
            id_param["schema"],
            json!({ "$ref": "#/components/schemas/Id" })
        );

        let filter_param = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "filter")
            .expect("filter query param");
        assert_eq!(
            filter_param["schema"],
            json!({ "$ref": "#/components/schemas/Filter" })
        );

        let output_schema = &op["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(
            output_schema["properties"]["item"],
            json!({ "$ref": "#/components/schemas/Item" })
        );

        assert_eq!(
            doc["components"]["schemas"]["Id"],
            json!({ "type": "string" })
        );
        assert_eq!(
            doc["components"]["schemas"]["Filter"],
            json!({ "type": "string" })
        );
        assert_eq!(
            doc["components"]["schemas"]["Item"],
            json!({ "type": "object" })
        );
        let text = doc.to_string();
        assert!(!text.contains("#/$defs/"), "no dangling local refs: {text}");
    }

    // 8. agent_card_basic_structure
    #[test]
    fn agent_card_basic_structure() {
        let block = test_block();
        let card = generate_agent_card(
            &[block],
            "My Project",
            "A test project",
            "https://example.com",
        );

        assert_eq!(card["name"], "My Project");
        assert_eq!(card["description"], "A test project");
        assert_eq!(card["version"], "1.0.0");
        assert_eq!(
            card["supported_interfaces"][0]["url"],
            "https://example.com/a2a"
        );
        assert_eq!(
            card["supported_interfaces"][0]["protocol_binding"],
            "JSONRPC"
        );
    }

    // 9. agent_card_skills_from_schema_endpoints
    #[test]
    fn agent_card_skills_from_schema_endpoints() {
        let block = test_block();
        let card = generate_agent_card(&[block], "P", "", "https://x.com");

        let skills = card["skills"].as_array().unwrap();
        // Only 2 schema endpoints (login + me); health has no schema
        assert_eq!(
            skills.len(),
            2,
            "should have 2 skills (schema endpoints only)"
        );

        let login_skill = skills.iter().find(|s| s["name"] == "Login").unwrap();
        assert_eq!(login_skill["tags"], json!(["auth"]));

        let me_skill = skills
            .iter()
            .find(|s| s["name"] == "Get current user")
            .unwrap();
        assert_eq!(me_skill["tags"], json!(["auth", "users"]));
    }

    // 10. agent_card_skill_ids_include_block_name
    #[test]
    fn agent_card_skill_ids_include_block_name() {
        let block = test_block();
        let card = generate_agent_card(&[block], "P", "", "https://x.com");

        let skills = card["skills"].as_array().unwrap();
        for skill in skills {
            let id = skill["id"].as_str().unwrap();
            assert!(
                id.starts_with("test/block/"),
                "skill id '{id}' should start with block name 'test/block/'"
            );
        }
    }

    // 11. agent_card_capabilities_defaults
    #[test]
    fn agent_card_capabilities_defaults() {
        let block = test_block();
        let card = generate_agent_card(&[block], "P", "", "https://x.com");

        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(card["capabilities"]["pushNotifications"], false);
    }

    // 12. inline_refs_leaves_flat_schema_unchanged
    #[test]
    fn inline_refs_leaves_flat_schema_unchanged() {
        let schema = json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert_eq!(out, schema);
        assert!(!unresolved, "a schema with no $ref resolves cleanly");
    }

    // 13. inline_refs_replaces_ref_with_target_and_drops_defs
    #[test]
    fn inline_refs_replaces_ref_with_target_and_drops_defs() {
        let schema = json!({
            "type": "object",
            "properties": { "status": { "$ref": "#/$defs/ProductStatus" } },
            "$defs": {
                "ProductStatus": { "type": "string", "enum": ["draft", "active"] }
            }
        });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["status"],
            json!({ "type": "string", "enum": ["draft", "active"] })
        );
        assert!(out.get("$defs").is_none(), "$defs must be stripped: {out}");
        assert!(!unresolved, "a resolvable ref must not be reported: {out}");
    }

    // 14. inline_refs_resolves_nested_refs
    #[test]
    fn inline_refs_resolves_nested_refs() {
        let schema = json!({
            "type": "object",
            "properties": { "tier": { "$ref": "#/$defs/PricingTier" } },
            "$defs": {
                "PricingTier": {
                    "type": "object",
                    "properties": { "scheme": { "$ref": "#/$defs/BillingScheme" } }
                },
                "BillingScheme": { "type": "string", "enum": ["per_unit", "tiered"] }
            }
        });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["tier"]["properties"]["scheme"],
            json!({ "type": "string", "enum": ["per_unit", "tiered"] })
        );
        assert!(!unresolved, "both hops resolve: {out}");
    }

    // 15. inline_refs_resolves_refs_inside_arrays
    #[test]
    fn inline_refs_resolves_refs_inside_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "offers": { "type": "array", "items": { "$ref": "#/$defs/Offer" } }
            },
            "$defs": { "Offer": { "type": "object" } }
        });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["offers"]["items"],
            json!({ "type": "object" })
        );
        assert!(!unresolved, "a ref inside an array resolves: {out}");
    }

    // 16. inline_refs_keeps_a_self_referential_definition_under_defs
    #[test]
    fn inline_refs_keeps_a_self_referential_definition_under_defs() {
        let schema = json!({
            "$defs": { "Node": { "type": "object", "properties": {
                "children": { "type": "array", "items": { "$ref": "#/$defs/Node" } } } } },
            "type": "object",
            "properties": { "root": { "$ref": "#/$defs/Node" } }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(issues, RefIssues::default());
        // The first reference is inlined; the cycle inside it is a `$ref`
        // back to the kept definition.
        assert_eq!(out["properties"]["root"]["type"], "object");
        assert_eq!(
            out["properties"]["root"]["properties"]["children"]["items"],
            json!({ "$ref": "#/$defs/Node" })
        );
        assert_eq!(
            out["$defs"]["Node"]["properties"]["children"]["items"],
            json!({ "$ref": "#/$defs/Node" })
        );
        assert!(
            out["$defs"].as_object().unwrap().len() == 1,
            "only the cyclic def is kept: {out}"
        );
    }

    /// A `Condition` that can contain child `Condition`s is a real shape in
    /// products/contracts.rs, and it arrives with the `$ref` at the schema
    /// root. Inlining must bottom out rather than recurse forever, and the
    /// way it bottoms out is a `$ref` back to a definition the output
    /// carries — not a `{}` that accepts anything where the server requires
    /// a `Condition`. Nothing is missing from `$defs`, so nothing is
    /// unresolvable either.
    #[test]
    fn inline_refs_terminates_on_self_referential_schema() {
        let schema = json!({
            "$ref": "#/$defs/Condition",
            "$defs": {
                "Condition": {
                    "type": "object",
                    "properties": { "all_of": { "type": "array", "items": { "$ref": "#/$defs/Condition" } } }
                }
            }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(issues, RefIssues::default());
        assert_eq!(out["type"], json!("object"));
        assert_eq!(
            out["properties"]["all_of"]["items"],
            json!({ "$ref": "#/$defs/Condition" }),
            "the cycle closes on the kept definition: {out}"
        );
        assert_eq!(
            out["$defs"]["Condition"]["properties"]["all_of"]["items"],
            json!({ "$ref": "#/$defs/Condition" }),
            "and the definition it names travels with the schema: {out}"
        );
    }

    /// The bug a depth cap has that cycle detection does not: `depth` never
    /// resets between nesting levels, so a cap on "ref hops" is really a cap
    /// on *cumulative* ref nesting. A finite, fully resolvable chain deeper
    /// than the old `MAX_REF_DEPTH = 8` was refused outright — a working tool
    /// deleted over an implementation detail.
    #[test]
    fn inline_refs_resolves_a_chain_deeper_than_the_old_depth_cap() {
        const DEPTH: usize = 20;

        let mut defs = serde_json::Map::new();
        for level in 0..DEPTH {
            let target = json!({ "$ref": format!("#/$defs/L{}", level + 1) });
            defs.insert(
                format!("L{level}"),
                json!({ "type": "object", "properties": { "next": target } }),
            );
        }
        defs.insert(format!("L{DEPTH}"), json!({ "type": "string" }));

        let schema = json!({
            "type": "object",
            "properties": { "root": { "$ref": "#/$defs/L0" } },
            "$defs": defs
        });

        let (out, issues) = inline_refs(&schema);
        assert!(
            !issues.unresolved,
            "a finite {DEPTH}-level chain resolves fully: {issues:?}"
        );

        let mut cursor = &out["properties"]["root"];
        for _ in 0..DEPTH {
            cursor = &cursor["properties"]["next"];
        }
        assert_eq!(
            cursor,
            &json!({ "type": "string" }),
            "every level must be inlined, not truncated: {out}"
        );
    }

    /// A definition referenced twice from *different* branches is a diamond,
    /// not a cycle: the visited stack must unwind, or the second branch is
    /// falsely read as closing a cycle — inlined once and then left as a
    /// back-edge to a definition that is in no way recursive.
    #[test]
    fn inline_refs_resolves_a_definition_referenced_from_two_branches() {
        let schema = json!({
            "type": "object",
            "properties": {
                "left": { "$ref": "#/$defs/Shared" },
                "right": { "$ref": "#/$defs/Shared" }
            },
            "$defs": { "Shared": { "type": "string", "enum": ["a", "b"] } }
        });
        let (out, issues) = inline_refs(&schema);
        assert!(
            !issues.unresolved,
            "a shared definition is not a cycle: {issues:?}"
        );
        assert_eq!(
            out["properties"]["left"], out["properties"]["right"],
            "both branches inline the same target: {out}"
        );
        assert_eq!(
            out["properties"]["left"],
            json!({ "type": "string", "enum": ["a", "b"] })
        );
        assert!(
            out.get("$defs").is_none(),
            "nothing refers back to `Shared`, so keeping it would ship a \
             definition no reference resolves against: {out}"
        );
    }

    /// A cycle that closes through an intermediate definition keeps exactly
    /// the definition the back-edge names — `A`, whose body carries `B`
    /// inlined — and nothing else. `B` is not part of any cycle on its own,
    /// so keeping it too would put a definition in the table that nothing
    /// refers to.
    #[test]
    fn inline_refs_keeps_the_definition_an_indirect_cycle_closes_on() {
        let indirect = json!({
            "$ref": "#/$defs/A",
            "$defs": {
                "A": { "type": "object", "properties": { "b": { "$ref": "#/$defs/B" } } },
                "B": { "type": "object", "properties": { "a": { "$ref": "#/$defs/A" } } }
            }
        });
        let (out, issues) = inline_refs(&indirect);
        assert_eq!(issues, RefIssues::default());
        assert_eq!(
            out["properties"]["b"]["properties"]["a"],
            json!({ "$ref": "#/$defs/A" }),
            "A -> B -> A closes on A: {out}"
        );
        assert_eq!(
            out["$defs"]["A"]["properties"]["b"]["properties"]["a"],
            json!({ "$ref": "#/$defs/A" }),
            "and the kept body is the one the back-edge names: {out}"
        );
        assert_eq!(
            out["$defs"].as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["A"],
            "B is inlined inside A and is referred to by nothing: {out}"
        );
    }

    #[test]
    fn inline_refs_rebases_root_recursion_to_a_named_definition() {
        let schema = json!({
            "title": "Condition",
            "type": "object",
            "properties": { "all": { "type": "array", "items": { "$ref": "#" } } }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(issues, RefIssues::default());
        assert_eq!(
            out["properties"]["all"]["items"],
            json!({ "$ref": "#/$defs/Condition" })
        );
        assert_eq!(
            out["$defs"]["Condition"]["properties"]["all"]["items"],
            json!({ "$ref": "#/$defs/Condition" })
        );
        assert!(
            out["$defs"]["Condition"].get("$defs").is_none(),
            "kept defs carry no nested table"
        );
    }

    /// The back-edge is a JSON pointer, so it carries the *encoded* name
    /// while the `$defs` key stays raw — the same split `decode_ref_name`
    /// exists for, now written in the other direction. Getting this wrong is
    /// invisible until a client tries to resolve the pointer and finds
    /// nothing, or finds a fragment that is not a legal URI at all.
    #[test]
    fn inline_refs_encodes_the_back_edge_of_an_awkwardly_named_definition() {
        let schema = json!({
            "type": "object",
            "properties": { "status": { "$ref": "#/$defs/Product%20Status" } },
            "$defs": {
                "Product Status": {
                    "type": "object",
                    "properties": { "next": { "$ref": "#/$defs/Product%20Status" } }
                }
            }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(issues, RefIssues::default());
        assert_eq!(
            out["properties"]["status"]["properties"]["next"],
            json!({ "$ref": "#/$defs/Product%20Status" }),
            "the pointer is encoded: {out}"
        );
        assert!(
            out["$defs"].get("Product Status").is_some(),
            "the table key is not: {out}"
        );
    }

    /// A hand-written schema whose root `title` is also a `$defs` key. Both
    /// are cyclic, so both have to be kept, and one table cannot hold two
    /// different bodies under one name — the root takes a free name instead
    /// of overwriting the definition every other back-edge resolves to.
    #[test]
    fn inline_refs_does_not_let_a_recursive_root_overwrite_a_kept_definition() {
        let schema = json!({
            "title": "Node",
            "type": "object",
            "properties": {
                "parent": { "$ref": "#" },
                "child": { "$ref": "#/$defs/Node" }
            },
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": { "next": { "$ref": "#/$defs/Node" } }
                }
            }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(issues, RefIssues::default());
        assert_eq!(
            out["properties"]["parent"],
            json!({ "$ref": "#/$defs/Node_" }),
            "the root cannot claim a name the table already uses: {out}"
        );
        assert_eq!(
            out["$defs"]["Node"],
            json!({ "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } }),
            "and the definition it would have overwritten is intact: {out}"
        );
        assert_eq!(
            out["$defs"]["Node_"]["properties"]["parent"],
            json!({ "$ref": "#/$defs/Node_" }),
            "{out}"
        );
    }

    /// A cycle reached from a document whose root is not a schema object has
    /// nowhere to put the definitions table, so the back-edges it emitted
    /// would name nothing. That is the same defect as a `$ref` with no
    /// referent and is reported the same way, rather than the table being
    /// dropped and the dangling pointers shipped.
    ///
    /// Only a hand-written source reaches this — a JSON array where a schema
    /// belongs — and it is refused as unrepresentable on top, since an array
    /// is not object-shaped. Both verdicts are asserted: the point is that
    /// neither depends on the other noticing.
    #[test]
    fn inline_refs_reports_a_cycle_it_cannot_attach_a_table_to() {
        let schema = json!([{ "$ref": "#" }]);
        let (out, issues) = inline_refs(&schema);
        assert!(
            issues.unresolved,
            "the back-edge names a table that could not be written: {out}"
        );
        assert!(!issues.oversized, "{issues:?}");
        assert_eq!(
            out,
            json!([{ "$ref": "#/$defs/Root" }]),
            "no table is invented on a non-object root: {out}"
        );

        let ep = BlockEndpoint::post("/b/x/array").input_schema(schema);
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
        assert_eq!(result.unresolved_refs, vec!["body".to_string()]);
    }

    /// With no `title` to name it by, the rebased root is `Root`. The name
    /// matters: it is what every back-edge in the published schema points
    /// at, and it is the name a second source's table can collide with.
    #[test]
    fn inline_refs_names_an_untitled_recursive_root_root() {
        let schema = json!({
            "type": "object",
            "properties": { "self": { "$ref": "#" } }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(issues, RefIssues::default());
        assert_eq!(
            out["properties"]["self"],
            json!({ "$ref": "#/$defs/Root" }),
            "the root exists, so it is named rather than cut: {out}"
        );
        assert_eq!(
            out["$defs"]["Root"]["properties"]["self"],
            json!({ "$ref": "#/$defs/Root" })
        );
    }

    /// `default`, `const`, `examples`, and `enum` hold *instance data*, not
    /// subschemas. Walking into them corrupts legal user data in two
    /// directions: a literal `{"$ref": ...}` is reported as an unresolvable
    /// reference (deleting a working endpoint), and a literal that happens to
    /// name a real `$defs` entry is silently rewritten into something the
    /// author never wrote.
    #[test]
    fn inline_refs_copies_literal_keyword_values_verbatim() {
        let schema = json!({
            "type": "object",
            "properties": {
                "external": {
                    "type": "object",
                    "default": { "$ref": "https://example.com/x" }
                },
                "rewritten": {
                    "type": "object",
                    "default": { "$ref": "#/$defs/D", "note": "literal data" }
                },
                "listed": {
                    "type": "string",
                    "examples": [{ "$ref": "#/$defs/D" }],
                    "enum": ["a", "b"]
                },
                "fixed": {
                    "const": { "$defs": { "not a table": true } }
                }
            },
            "$defs": { "D": { "type": "string" } }
        });

        let (out, issues) = inline_refs(&schema);
        assert!(
            !issues.unresolved,
            "no schema position holds a $ref here, so nothing is reported: {issues:?}"
        );
        assert_eq!(
            out["properties"]["external"]["default"],
            json!({ "$ref": "https://example.com/x" }),
            "a literal default is data, not a reference: {out}"
        );
        assert_eq!(
            out["properties"]["rewritten"]["default"],
            json!({ "$ref": "#/$defs/D", "note": "literal data" }),
            "a literal default must not be rewritten through the sibling merge: {out}"
        );
        assert_eq!(
            out["properties"]["listed"]["examples"],
            json!([{ "$ref": "#/$defs/D" }]),
            "examples entries are instance data: {out}"
        );
        assert_eq!(
            out["properties"]["fixed"]["const"],
            json!({ "$defs": { "not a table": true } }),
            "`$defs` stripping must not reach inside a literal value: {out}"
        );
    }

    /// The same rule on the `$ref`-sibling copy path: a `default` sitting
    /// beside a `$ref` is merged onto the resolved target, and must arrive
    /// untouched.
    #[test]
    fn inline_refs_copies_a_literal_sibling_of_a_ref_verbatim() {
        let schema = json!({
            "type": "object",
            "properties": {
                "status": {
                    "$ref": "#/$defs/Status",
                    "default": { "$ref": "https://example.com/x" }
                }
            },
            "$defs": { "Status": { "type": "string" } }
        });
        let (out, issues) = inline_refs(&schema);
        assert!(
            !issues.unresolved,
            "the only real reference resolves: {issues:?}"
        );
        assert_eq!(
            out["properties"]["status"],
            json!({ "type": "string", "default": { "$ref": "https://example.com/x" } }),
            "a literal sibling of $ref must survive verbatim: {out}"
        );
    }

    /// A schema whose definitions each reference the next one twice is
    /// finite, acyclic, and fully resolvable — and doubles in size per level.
    /// `levels` of it expand to `2^levels` copies of the leaf.
    fn doubling_schema(levels: usize) -> Value {
        let mut defs = serde_json::Map::new();
        for level in 0..levels {
            let next = json!({ "$ref": format!("#/$defs/L{}", level + 1) });
            defs.insert(
                format!("L{level}"),
                json!({
                    "type": "object",
                    "properties": { "a": next.clone(), "b": next }
                }),
            );
        }
        defs.insert(
            format!("L{levels}"),
            json!({ "type": "string", "description": "leaf" }),
        );
        json!({
            "type": "object",
            "properties": { "root": { "$ref": "#/$defs/L0" } },
            "$defs": defs
        })
    }

    /// The keyword rule is about *position*, not spelling. A body field
    /// literally named `default` is a member of the `properties` map, so its
    /// value is a schema and its `$ref` must resolve. Reading the name as the
    /// `default` keyword copied the reference through verbatim, stripped
    /// `$defs` out from under it, and set no flag — a published tool whose
    /// argument points at a definition that is no longer in the document.
    #[test]
    fn inline_refs_resolves_a_property_named_like_a_literal_keyword() {
        let schema = json!({
            "type": "object",
            "properties": {
                "default": { "$ref": "#/$defs/Status" },
                "const": { "$ref": "#/$defs/Status" },
                "enum": { "$ref": "#/$defs/Status" },
                "examples": { "$ref": "#/$defs/Status" },
                "normal": { "$ref": "#/$defs/Status" }
            },
            "$defs": { "Status": { "type": "string", "enum": ["draft", "active"] } }
        });

        let (out, issues) = inline_refs(&schema);
        assert!(
            !issues.unresolved && !issues.oversized,
            "every reference here resolves: {issues:?}"
        );
        let status = json!({ "type": "string", "enum": ["draft", "active"] });
        for name in ["default", "const", "enum", "examples", "normal"] {
            assert_eq!(
                out["properties"][name], status,
                "a property named `{name}` carries a schema, not instance data: {out}"
            );
        }
        assert!(
            !out.to_string().contains("$ref"),
            "no reference may survive into the published schema: {out}"
        );
    }

    /// The same property, pointing at a definition that does not exist, must
    /// be *reported*. The name-based rule published it silently, which is the
    /// exact failure the refusal wall exists to prevent.
    #[test]
    fn inline_refs_reports_a_dangling_ref_under_a_keyword_named_property() {
        let schema = json!({
            "type": "object",
            "properties": { "default": { "$ref": "#/$defs/Missing" } }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(out["properties"]["default"], json!({}));
        assert!(
            issues.unresolved,
            "a dangling ref under a keyword-named property is still dangling: {out}"
        );
    }

    /// Position decides in both directions, inside one schema: the `default`
    /// *keyword* of the `settings` property holds instance data and is copied
    /// verbatim, while the `default` *property* beside it holds a schema and
    /// is resolved.
    #[test]
    fn inline_refs_separates_a_default_keyword_from_a_default_property() {
        let schema = json!({
            "type": "object",
            "properties": {
                "default": { "$ref": "#/$defs/Status" },
                "settings": {
                    "type": "object",
                    "default": { "$ref": "#/$defs/Status", "note": "literal data" }
                }
            },
            "$defs": { "Status": { "type": "string" } }
        });
        let (out, issues) = inline_refs(&schema);
        assert!(!issues.unresolved && !issues.oversized, "{issues:?}");
        assert_eq!(
            out["properties"]["default"],
            json!({ "type": "string" }),
            "the property is a schema and resolves: {out}"
        );
        assert_eq!(
            out["properties"]["settings"]["default"],
            json!({ "$ref": "#/$defs/Status", "note": "literal data" }),
            "the keyword is instance data and is copied verbatim: {out}"
        );
    }

    /// The `$ref`-sibling copy path walks a schema object too, so it applies
    /// the same position rule: a `properties` map merged in beside a `$ref`
    /// carries author-chosen names, while a `default` sibling of that `$ref`
    /// is instance data.
    #[test]
    fn inline_refs_applies_the_position_rule_on_the_ref_sibling_path() {
        let schema = json!({
            "$ref": "#/$defs/Base",
            "default": { "$ref": "#/$defs/Status", "note": "literal data" },
            "properties": {
                "default": { "$ref": "#/$defs/Status" },
                "missing": { "$ref": "#/$defs/Absent" }
            },
            "$defs": {
                "Base": { "type": "object" },
                "Status": { "type": "string" }
            }
        });
        let (out, issues) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["default"],
            json!({ "type": "string" }),
            "a sibling `properties` map holds schemas, keyword-spelled or not: {out}"
        );
        assert_eq!(
            out["default"],
            json!({ "$ref": "#/$defs/Status", "note": "literal data" }),
            "a `default` sibling of `$ref` is still instance data: {out}"
        );
        assert!(
            issues.unresolved,
            "the dangling ref under the keyword-named property must be reported: {out}"
        );
        assert!(!issues.oversized, "{issues:?}");
    }

    /// Names the author chose also appear under `patternProperties` and
    /// `dependentSchemas`, and are schemas there for the same reason.
    #[test]
    fn inline_refs_resolves_schemas_under_the_other_author_named_maps() {
        let schema = json!({
            "type": "object",
            "patternProperties": { "^const$": { "$ref": "#/$defs/Status" } },
            "dependentSchemas": {
                "enum": {
                    "type": "object",
                    "properties": { "examples": { "$ref": "#/$defs/Status" } }
                }
            },
            "$defs": { "Status": { "type": "string" } }
        });
        let (out, issues) = inline_refs(&schema);
        assert!(!issues.unresolved && !issues.oversized, "{issues:?}");
        assert_eq!(
            out["patternProperties"]["^const$"],
            json!({ "type": "string" }),
            "a pattern is an author-chosen key: {out}"
        );
        assert_eq!(
            out["dependentSchemas"]["enum"]["properties"]["examples"],
            json!({ "type": "string" }),
            "a dependent schema is keyed by property name: {out}"
        );
    }

    /// `$defs` is the reference table only where a schema object's keys are
    /// read as keywords. A *property* named `$defs` is an author-chosen key
    /// in a `properties` map, and dropping it deletes a field the endpoint
    /// really accepts. Tracking position rather than the spelling of the key
    /// is what tells the two apart.
    #[test]
    fn inline_refs_keeps_a_property_named_defs() {
        let schema = json!({
            "type": "object",
            "properties": { "$defs": { "$ref": "#/$defs/Status" } },
            "$defs": { "Status": { "type": "string" } }
        });
        let (out, issues) = inline_refs(&schema);
        assert!(!issues.unresolved && !issues.oversized, "{issues:?}");
        assert_eq!(
            out["properties"]["$defs"],
            json!({ "type": "string" }),
            "a field named `$defs` is a field, not the reference table: {out}"
        );
        assert!(
            out.get("$defs").is_none(),
            "the real table is still stripped: {out}"
        );
    }

    /// Cycle detection bounds an expansion's depth, not its size. A finite,
    /// acyclic type whose definitions multiply out has no honest inlining
    /// either, and must be reported as its own defect: nothing is missing
    /// from `$defs`, so the unresolvable-reference verdict would send the
    /// author hunting for a defect that is not there.
    #[test]
    fn inline_refs_stops_and_reports_an_expansion_past_the_node_budget() {
        let (_, issues) = inline_refs(&doubling_schema(22));
        assert!(
            issues.oversized,
            "2^22 copies of the leaf must break the budget: {issues:?}"
        );
        assert!(
            !issues.unresolved,
            "every `$defs` entry it names exists: {issues:?}"
        );
    }

    /// The budget is a bound on runaway expansion, not on real schemas. A
    /// type far larger than anything a block declares — 150 fields of a
    /// 12-field record — inlines completely and reports nothing.
    #[test]
    fn inline_refs_publishes_a_large_but_realistic_type_tree() {
        let mut record = serde_json::Map::new();
        for field in 0..12 {
            record.insert(
                format!("f{field}"),
                json!({ "type": "string", "description": "a field" }),
            );
        }
        let mut properties = serde_json::Map::new();
        for field in 0..150 {
            properties.insert(format!("p{field}"), json!({ "$ref": "#/$defs/Record" }));
        }
        let schema = json!({
            "type": "object",
            "properties": properties,
            "$defs": { "Record": { "type": "object", "properties": record } }
        });

        let (out, issues) = inline_refs(&schema);
        assert!(
            !issues.oversized && !issues.unresolved,
            "1800 inlined properties is well inside the budget: {issues:?}"
        );
        assert_eq!(
            out["properties"]["p149"]["properties"]["f11"],
            json!({ "type": "string", "description": "a field" }),
            "every reference is inlined in full: {out}"
        );
    }

    // 17. inline_refs_strips_defs_even_when_ref_is_schema_root
    #[test]
    fn inline_refs_strips_defs_even_when_ref_is_schema_root() {
        // When `$ref` sits at the schema root, `$defs` is a literal sibling
        // of it in the same JSON object — the same shape that carries a
        // legitimate sibling like `description` in the test above. Unlike
        // `description`, `$defs` is the reference table itself and must
        // never be merged back into the output. The output can carry a
        // `$defs` table of its own, but only one `inline_refs` built from
        // the definitions a cycle closed on — nothing here is cyclic, so
        // there is none.
        let schema = json!({
            "$ref": "#/$defs/Condition",
            "$defs": {
                "Condition": {
                    "type": "object",
                    "properties": { "all_of": { "type": "array", "items": { "type": "string" } } }
                }
            }
        });
        let (out, _issues) = inline_refs(&schema);
        assert!(out.get("$defs").is_none(), "$defs must be stripped: {out}");
        assert_eq!(
            out,
            json!({
                "type": "object",
                "properties": { "all_of": { "type": "array", "items": { "type": "string" } } }
            }),
            "an acyclic schema inlines to exactly what it did before kept \
             definitions existed: {out}"
        );
    }

    // 18. inline_refs_reports_an_unresolvable_ref_it_had_to_drop
    #[test]
    fn inline_refs_reports_an_unresolvable_ref_it_had_to_drop() {
        let schema = json!({ "properties": { "x": { "$ref": "#/$defs/Missing" } } });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert_eq!(out["properties"]["x"], json!({}));
        assert!(
            unresolved,
            "a ref with no matching $defs entry degrades to `{{}}` and must be \
             reported, not passed off as a schema: {out}"
        );
    }

    // 18b. inline_refs_resolves_a_percent_encoded_ref_name
    #[test]
    fn inline_refs_resolves_a_percent_encoded_ref_name() {
        // schemars percent-encodes reference *names* (`encode_ref_name`) but
        // leaves the `$defs` keys unencoded, so a `#[schemars(rename = "...")]`
        // with a space in it only resolves if the pointer is decoded first.
        let schema = json!({
            "type": "object",
            "properties": { "status": { "$ref": "#/$defs/Product%20Status" } },
            "$defs": {
                "Product Status": { "type": "string", "enum": ["draft", "active"] }
            }
        });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["status"],
            json!({ "type": "string", "enum": ["draft", "active"] }),
            "a percent-encoded ref name must be decoded before lookup: {out}"
        );
        assert!(
            !unresolved,
            "the ref resolved, so nothing is reported: {out}"
        );
    }

    // 18c. inline_refs_resolves_a_json_pointer_escaped_ref_name
    #[test]
    fn inline_refs_resolves_a_json_pointer_escaped_ref_name() {
        // `/` -> `~1` and `~` -> `~0`, unescaped in that order per RFC 6901 §4.
        let schema = json!({
            "type": "object",
            "properties": {
                "slash": { "$ref": "#/$defs/a~1b" },
                "tilde": { "$ref": "#/$defs/c~0d" },
                "literal_tilde_one": { "$ref": "#/$defs/~01" }
            },
            "$defs": {
                "a/b": { "type": "string" },
                "c~d": { "type": "integer" },
                "~1": { "type": "boolean" }
            }
        });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert_eq!(out["properties"]["slash"], json!({ "type": "string" }));
        assert_eq!(out["properties"]["tilde"], json!({ "type": "integer" }));
        assert_eq!(
            out["properties"]["literal_tilde_one"],
            json!({ "type": "boolean" }),
            "unescaping `~0` before `~1` would turn `~01` into `/`: {out}"
        );
        assert!(!unresolved, "all three refs resolved: {out}");
    }

    // 19. inline_refs_preserves_keywords_sitting_beside_a_ref
    #[test]
    fn inline_refs_preserves_keywords_sitting_beside_a_ref() {
        // JSON Schema 2020-12 allows keywords alongside `$ref`, and schemars uses
        // that for field-level docs on a named type. Returning only the target
        // would delete every such description.
        let schema = json!({
            "type": "object",
            "properties": {
                "status": {
                    "description": "Current lifecycle status of the product.",
                    "$ref": "#/$defs/ProductStatus"
                }
            },
            "$defs": {
                "ProductStatus": { "type": "string", "enum": ["draft", "active"] }
            }
        });
        let (out, RefIssues { unresolved, .. }) = inline_refs(&schema);
        assert!(!unresolved, "the ref resolves: {out}");
        let status = &out["properties"]["status"];

        assert_eq!(
            status["description"],
            json!("Current lifecycle status of the product."),
            "a description beside $ref must survive inlining: {status}"
        );
        assert_eq!(status["type"], json!("string"));
        assert_eq!(status["enum"], json!(["draft", "active"]));
    }

    // 20. agent_input_schema_is_empty_object_when_endpoint_has_no_schemas
    #[test]
    fn agent_input_schema_is_empty_object_when_endpoint_has_no_schemas() {
        let ep = BlockEndpoint::get("/b/products/storefront/config");
        let result = agent_input_schema(&ep);
        assert_eq!(result.schema, json!({ "type": "object", "properties": {} }));
        assert!(
            result.path_params.is_empty()
                && result.query_params.is_empty()
                && result.body_params.is_empty()
        );
        assert!(result.collisions.is_empty());
    }

    // 21. agent_input_schema_merges_all_three_sources_and_records_provenance
    #[test]
    fn agent_input_schema_merges_all_three_sources_and_records_provenance() {
        let ep = BlockEndpoint::post("/b/products/products/{product_id}/offers")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "product_id": { "type": "string" } },
                "required": ["product_id"]
            }))
            .query_params_schema(json!({
                "type": "object",
                "properties": { "expand": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }));

        let result = agent_input_schema(&ep);

        assert_eq!(
            result.schema["properties"]["product_id"],
            json!({ "type": "string" })
        );
        assert_eq!(
            result.schema["properties"]["expand"],
            json!({ "type": "string" })
        );
        assert_eq!(
            result.schema["properties"]["name"],
            json!({ "type": "string" })
        );

        assert_eq!(
            result.schema["required"],
            json!(["name", "product_id"]),
            "required must be exactly the sorted union of both sources' required lists, \
             with expand excluded and no duplicates"
        );

        assert_eq!(result.path_params, vec!["product_id".to_string()]);
        assert_eq!(result.query_params, vec!["expand".to_string()]);
        assert_eq!(result.body_params, vec!["name".to_string()]);
        assert!(result.collisions.is_empty());
    }

    // 22. agent_input_schema_inlines_refs_from_each_source
    #[test]
    fn agent_input_schema_inlines_refs_from_each_source() {
        let ep = BlockEndpoint::post("/b/products/checkout").input_schema(json!({
            "type": "object",
            "properties": { "presentation": { "$ref": "#/$defs/CheckoutPresentation" } },
            "$defs": {
                "CheckoutPresentation": { "type": "string", "enum": ["hosted", "embedded"] }
            }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(
            result.schema["properties"]["presentation"],
            json!({ "type": "string", "enum": ["hosted", "embedded"] })
        );
        assert!(result.schema.get("$defs").is_none());
        assert_eq!(
            result.schema,
            json!({
                "type": "object",
                "properties": {
                    "presentation": { "type": "string", "enum": ["hosted", "embedded"] }
                }
            }),
            "keeping cyclic definitions is additive: a source with no cycle \
             merges to exactly the document it did before: {:?}",
            result.schema
        );
        assert_eq!(result.body_params, vec!["presentation".to_string()]);
    }

    // 23. agent_input_schema_omits_required_key_when_nothing_is_required
    #[test]
    fn agent_input_schema_omits_required_key_when_nothing_is_required() {
        let ep = BlockEndpoint::get("/b/products/list").query_params_schema(json!({
            "type": "object",
            "properties": { "page": { "type": "integer" } }
        }));
        let result = agent_input_schema(&ep);
        assert!(
            result.schema.get("required").is_none(),
            "an all-optional schema must not carry an empty required array: {}",
            result.schema
        );
        assert_eq!(result.query_params, vec!["page".to_string()]);
    }

    // 24. agent_input_schema_provenance_is_sorted_for_deterministic_output
    #[test]
    fn agent_input_schema_provenance_is_sorted_for_deterministic_output() {
        let ep = BlockEndpoint::get("/b/x/{b}/{a}").path_params_schema(json!({
            "type": "object",
            "properties": { "b": { "type": "string" }, "a": { "type": "string" } }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.path_params, vec!["a".to_string(), "b".to_string()]);
    }

    // 25. agent_input_schema_no_collision_across_distinct_names
    #[test]
    fn agent_input_schema_no_collision_across_distinct_names() {
        // Same multi-source shape as test 21: three distinct property names,
        // one per source — nothing should be flagged as colliding.
        let ep = BlockEndpoint::post("/b/products/products/{product_id}/offers")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "product_id": { "type": "string" } }
            }))
            .query_params_schema(json!({
                "type": "object",
                "properties": { "expand": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }));

        let result = agent_input_schema(&ep);
        assert!(result.collisions.is_empty());
    }

    // 26. agent_input_schema_records_collision_between_path_and_body
    #[test]
    fn agent_input_schema_records_collision_between_path_and_body() {
        let ep = BlockEndpoint::post("/b/products/products/{id}")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "integer" } }
            }));

        let result = agent_input_schema(&ep);
        assert_eq!(
            result.collisions,
            vec!["id".to_string()],
            "a name contributed by both path and body must be flagged, not silently \
             resolved by last-source-wins"
        );
        // Provenance still records the name on both sides — the caller
        // needs that to see exactly which locations collided.
        assert_eq!(result.path_params, vec!["id".to_string()]);
        assert_eq!(result.body_params, vec!["id".to_string()]);
    }

    // 27. agent_input_schema_collects_every_colliding_name_not_just_the_first
    #[test]
    fn agent_input_schema_collects_every_colliding_name_not_just_the_first() {
        // "id" collides between path and body; "owner" collides between
        // path and query. Both must come back, sorted.
        let ep = BlockEndpoint::post("/b/products/products/{id}/{owner}")
            .path_params_schema(json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "owner": { "type": "string" }
                }
            }))
            .query_params_schema(json!({
                "type": "object",
                "properties": { "owner": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "name": { "type": "string" }
                }
            }));

        let result = agent_input_schema(&ep);
        assert_eq!(
            result.collisions,
            vec!["id".to_string(), "owner".to_string()],
            "every colliding name must be collected and sorted, not just the first found"
        );
    }

    // 28. agent_input_schema_flags_tagged_enum_body_as_unrepresentable
    #[test]
    fn agent_input_schema_flags_tagged_enum_body_as_unrepresentable() {
        // A `#[serde(tag = "...")]` enum body has no top-level `properties`
        // object at all — schemars renders it as `oneOf`. The merge must
        // not silently contribute nothing; it must flag the body as
        // unrepresentable so the caller refuses to build a tool from it.
        let ep = BlockEndpoint::post("/b/products/conditions").input_schema(json!({
            "oneOf": [
                { "type": "object", "properties": { "kind": { "const": "always" } } },
                { "type": "object", "properties": { "kind": { "const": "never" } } }
            ]
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
        assert!(result.body_params.is_empty());
    }

    // 29. agent_input_schema_flags_array_body_as_unrepresentable
    #[test]
    fn agent_input_schema_flags_array_body_as_unrepresentable() {
        let ep = BlockEndpoint::post("/b/products/bulk").input_schema(json!({
            "type": "array",
            "items": { "type": "string" }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
        assert!(result.body_params.is_empty());
    }

    // 30. agent_input_schema_flags_root_recursive_ref_as_unrepresentable
    #[test]
    fn agent_input_schema_flags_root_recursive_ref_as_unrepresentable() {
        // A body that is *only* the root-recursion marker describes nothing
        // an agent could fill in: inlining rebases it to
        // `{"$ref": "#/$defs/Root"}` and keeps a `Root` definition whose
        // whole body is that same back-edge. The top level is then a bare
        // `$ref`, which is not an object with named members, so no flat
        // `inputSchema` can describe it. Keeping cyclic definitions changes
        // how this is refused, not whether it is.
        let ep = BlockEndpoint::post("/b/products/condition").input_schema(json!({ "$ref": "#" }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
        assert!(result.body_params.is_empty());
    }

    // 31. agent_input_schema_ordinary_object_source_is_representable
    #[test]
    fn agent_input_schema_ordinary_object_source_is_representable() {
        let ep = BlockEndpoint::post("/b/products/rename").input_schema(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        }));
        let result = agent_input_schema(&ep);
        assert!(
            result.unrepresentable.is_empty(),
            "an ordinary object source must not be flagged: {:?}",
            result.unrepresentable
        );
        assert_eq!(result.body_params, vec!["name".to_string()]);
    }

    // -----------------------------------------------------------------
    // representability: composition keywords beside `properties`
    // -----------------------------------------------------------------

    /// `#[serde(flatten)] kind: SomeEnum` on a body struct: schemars emits
    /// the enum's `oneOf` as a *sibling* of the merged `properties`. A
    /// "does it have `properties`?" test sees a healthy schema and publishes
    /// a tool whose `inputSchema` is missing every field inside those
    /// branches — and the `required` entries with them.
    ///
    /// The literal below is what `BlockEndpoint::input::<T>()` emits for
    /// `struct Body { name: String, #[serde(flatten)] kind: Kind }` with
    /// `#[serde(tag = "kind")] enum Kind { Percent { percent: f64 },
    /// Amount { amount: i64 } }`, root `title` included.
    #[test]
    fn agent_input_schema_flags_a_oneof_beside_properties_as_unrepresentable() {
        let ep = BlockEndpoint::post("/b/products/rules").input_schema(json!({
            "title": "Body",
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"],
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "kind": { "const": "Percent", "type": "string" },
                        "percent": { "format": "double", "type": "number" }
                    },
                    "required": ["kind", "percent"]
                },
                {
                    "type": "object",
                    "properties": {
                        "amount": { "format": "int64", "type": "integer" },
                        "kind": { "const": "Amount", "type": "string" }
                    },
                    "required": ["kind", "amount"]
                }
            ]
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(
            result.unrepresentable,
            vec!["body".to_string()],
            "a oneOf sitting beside properties must be flagged, not silently \
             dropped: {}",
            result.schema
        );
        assert!(
            result.body_params.is_empty(),
            "an unrepresentable source contributes nothing"
        );
    }

    #[test]
    fn agent_input_schema_flags_an_allof_beside_properties_as_unrepresentable() {
        let ep = BlockEndpoint::post("/b/products/rules").input_schema(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "allOf": [
                { "type": "object", "properties": { "extra": { "type": "string" } } }
            ]
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
    }

    #[test]
    fn agent_input_schema_flags_an_if_then_beside_properties_as_unrepresentable() {
        let ep = BlockEndpoint::post("/b/products/rules").input_schema(json!({
            "type": "object",
            "properties": { "mode": { "type": "string" } },
            "if": { "properties": { "mode": { "const": "advanced" } } },
            "then": { "required": ["threshold"] }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
    }

    #[test]
    fn agent_input_schema_flags_a_nullable_source_as_unrepresentable() {
        // `Option<T>`: schemars emits `"type": [T, "null"]`, and `null` is
        // not something a flat object schema can express.
        let ep = BlockEndpoint::post("/b/products/maybe").input_schema(json!({
            "title": "Nullable_string",
            "type": ["string", "null"]
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
    }

    #[test]
    fn agent_input_schema_flags_a_boolean_schema_source_as_unrepresentable() {
        // The JSON Schema "accept anything" form. It admits arrays and
        // scalars too, which a flat object cannot describe, and the agent
        // has no named argument to put anything into.
        let ep = BlockEndpoint::post("/b/products/raw").input_schema(json!(true));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
    }

    #[test]
    fn agent_input_schema_flags_an_untyped_any_source_as_unrepresentable() {
        // What `BlockEndpoint::input::<serde_json::Value>()` emits: a bare
        // annotation with no `type` and no `properties`, which constrains
        // nothing at all.
        let ep =
            BlockEndpoint::post("/b/products/raw").input_schema(json!({ "title": "AnyValue" }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
    }

    // -----------------------------------------------------------------
    // representability: shapes the old `properties`-based test over-refused
    // -----------------------------------------------------------------

    /// A fieldless struct derives `{"title": "Empty", "type": "object"}` with
    /// no `properties` at all, and genuinely takes no arguments.
    /// Contributing nothing for it is the truth, not a lie, so it must not
    /// be refused.
    #[test]
    fn agent_input_schema_treats_an_empty_object_source_as_representable() {
        let ep = BlockEndpoint::post("/b/products/ping").input_schema(json!({
            "title": "Empty",
            "type": "object"
        }));
        let result = agent_input_schema(&ep);
        assert!(
            result.unrepresentable.is_empty(),
            "a fieldless object body takes no arguments and is representable: {:?}",
            result.unrepresentable
        );
        assert!(result.body_params.is_empty());
        assert_eq!(result.schema, json!({ "type": "object", "properties": {} }));
    }

    /// `#[serde(deny_unknown_fields)]` derives `additionalProperties: false`.
    /// It is representable, and the merged object may repeat the claim
    /// because every source that fed it made the same one.
    #[test]
    fn agent_input_schema_carries_additional_properties_false_when_every_source_closes() {
        let ep = BlockEndpoint::post("/b/products/strict/{id}")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "additionalProperties": false
            }));
        let result = agent_input_schema(&ep);
        assert!(result.unrepresentable.is_empty());
        assert_eq!(
            result.schema["additionalProperties"],
            json!(false),
            "every present source closed itself, so the merged object may \
             say so too: {}",
            result.schema
        );
    }

    /// One open source means an unknown key is legal somewhere. Claiming the
    /// merged object is closed would make a strict client refuse arguments
    /// the server would have accepted.
    #[test]
    fn agent_input_schema_drops_additional_properties_false_when_one_source_is_open() {
        let ep = BlockEndpoint::post("/b/products/strict/{id}")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }));
        let result = agent_input_schema(&ep);
        assert!(
            result.schema.get("additionalProperties").is_none(),
            "one open source means the merged object is not closed: {}",
            result.schema
        );
    }

    /// A `HashMap<String, T>` body derives `{"type": "object",
    /// "additionalProperties": {...}}`. The schema shape alone is
    /// flattenable, but the `invocation` provenance routes arguments by
    /// *name* — `body_params` is a fixed list — so a key the agent invents
    /// has nowhere to go and never reaches the server. Publishing a tool
    /// whose only usable arguments cannot be transmitted fails on every
    /// invocation, so it is refused.
    #[test]
    fn agent_input_schema_flags_an_open_map_body_as_unrepresentable() {
        let ep = BlockEndpoint::post("/b/products/settings").input_schema(json!({
            "title": "MapBody",
            "type": "object",
            "additionalProperties": { "type": "string" }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(
            result.unrepresentable,
            vec!["body".to_string()],
            "an open map body has no named arguments the client can route: {}",
            result.schema
        );
    }

    #[test]
    fn agent_input_schema_flags_an_explicitly_open_object_as_unrepresentable() {
        let ep = BlockEndpoint::post("/b/products/settings").input_schema(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
    }

    // -----------------------------------------------------------------
    // path placeholders are structurally mandatory
    // -----------------------------------------------------------------

    /// A path source with no `required` key at all still yields a `required`
    /// list containing the placeholder. Without this the emitted schema marks
    /// the path param OPTIONAL, the agent legitimately omits it, and the
    /// client fetches `/b/x/undefined`.
    #[test]
    fn agent_input_schema_forces_an_optional_path_param_required() {
        let ep = BlockEndpoint::get("/b/x/{id}").path_params_schema(json!({
            "type": "object",
            "properties": { "id": { "type": "string" } }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(
            result.schema["required"],
            json!(["id"]),
            "a path placeholder is structurally mandatory whatever the source \
             said: {}",
            result.schema
        );
    }

    #[test]
    fn agent_input_schema_keeps_a_path_param_the_source_already_required() {
        let ep = BlockEndpoint::get("/b/x/{id}").path_params_schema(json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(
            result.schema["required"],
            json!(["id"]),
            "no duplicate entry when the source already said so: {}",
            result.schema
        );
    }

    #[test]
    fn agent_input_schema_forces_every_placeholder_on_a_multi_placeholder_path() {
        let ep = BlockEndpoint::get("/b/x/{tenant}/y/{id}").path_params_schema(json!({
            "type": "object",
            "properties": { "tenant": { "type": "string" }, "id": { "type": "string" } }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.schema["required"], json!(["id", "tenant"]));
    }

    /// Forcing applies only to the path source. A query or body property the
    /// author marked optional stays optional.
    #[test]
    fn agent_input_schema_does_not_force_query_or_body_params_required() {
        let ep = BlockEndpoint::post("/b/x/{id}")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "string" } }
            }))
            .query_params_schema(json!({
                "type": "object",
                "properties": { "expand": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.schema["required"], json!(["id"]));
    }

    // -----------------------------------------------------------------
    // generate_webmcp
    // -----------------------------------------------------------------

    /// Two blocks spanning all three auth levels, used by the tests below.
    fn webmcp_fixture_blocks() -> Vec<BlockInfo> {
        let products = BlockInfo::new(
            "impresspress/products",
            "1.0.0",
            "http-handler@v1",
            "Commerce block",
        )
        .endpoints(vec![
            BlockEndpoint::get("/b/products/storefront/{product_id}")
                .summary("Storefront product")
                .auth(AuthLevel::Public)
                .path_params_schema(json!({
                    "type": "object",
                    "properties": { "product_id": { "type": "string" } },
                    "required": ["product_id"]
                }))
                .agent_tool("get_product", "Fetch a product and its offers by id."),
            BlockEndpoint::get("/b/products/purchases")
                .summary("List own purchases")
                .auth(AuthLevel::Authenticated)
                .output_schema(json!({ "type": "object" }))
                .agent_tool("list_my_purchases", "List the signed-in user's purchases."),
            // Carries schemas but never opted in — must never appear.
            BlockEndpoint::post("/b/products/webhooks")
                .summary("Stripe webhook")
                .auth(AuthLevel::Public)
                .input_schema(json!({ "type": "object" })),
        ]);

        let admin = BlockInfo::new(
            "impresspress/admin",
            "1.0.0",
            "http-handler@v1",
            "Admin block",
        )
        .endpoints(vec![BlockEndpoint::get("/b/admin/api/users")
            .summary("List users")
            .auth(AuthLevel::Admin)
            .output_schema(json!({ "type": "object" }))
            .agent_tool("list_users", "List all user accounts.")]);

        vec![products, admin]
    }

    fn tool_names(doc: &Value) -> Vec<String> {
        doc["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name").to_string())
            .collect()
    }

    #[test]
    fn webmcp_public_caller_sees_only_public_tools() {
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Public);
        assert_eq!(tool_names(&doc), vec!["get_product".to_string()]);

        // Name-list assertions above only prove the higher-privilege tools'
        // *names* are absent. Assert on the fully rendered document too, so a
        // future bug that leaks a restricted tool's description or
        // invocation path while omitting only its name would still be
        // caught — auth filtering is the security property this function
        // exists for, and a name an agent cannot use is recon surface.
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("list_my_purchases") && !rendered.contains("/b/products/purchases"),
            "an authenticated-only endpoint must not appear in an anonymous caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("List the signed-in user's purchases."),
            "an authenticated-only tool's description must not leak into an anonymous caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("list_users") && !rendered.contains("/b/admin/api/users"),
            "an admin-only endpoint must not appear in an anonymous caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("List all user accounts."),
            "an admin-only tool's description must not leak into an anonymous caller's manifest: {rendered}"
        );
    }

    #[test]
    fn webmcp_authenticated_caller_sees_public_and_authenticated() {
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Authenticated);
        let names = tool_names(&doc);
        assert!(names.contains(&"get_product".to_string()));
        assert!(names.contains(&"list_my_purchases".to_string()));
        assert!(
            !names.contains(&"list_users".to_string()),
            "admin tool must not leak to an authenticated caller: {names:?}"
        );

        // As above: the name-list check only proves the admin tool's *name*
        // is absent. Check the full rendered document too, so a leak of the
        // admin tool's description or invocation path (with the name
        // suppressed) would still fail this test.
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("list_users") && !rendered.contains("/b/admin/api/users"),
            "an admin-only endpoint must not appear in an authenticated (non-admin) caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("List all user accounts."),
            "an admin-only tool's description must not leak into an authenticated (non-admin) caller's manifest: {rendered}"
        );
    }

    #[test]
    fn webmcp_admin_caller_sees_every_tool() {
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Admin);
        let mut names = tool_names(&doc);
        names.sort();
        assert_eq!(
            names,
            vec![
                "get_product".to_string(),
                "list_my_purchases".to_string(),
                "list_users".to_string(),
            ],
            "an admin caller must see exactly these three tools, by name — a length-only \
             assertion here previously let real bugs through: {doc}"
        );
    }

    #[test]
    fn webmcp_excludes_endpoints_that_did_not_opt_in() {
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Admin);
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("/b/products/webhooks"),
            "a schema-carrying endpoint without agent_tool must be absent: {rendered}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_whose_parameter_names_collide() {
        // `id` arrives from BOTH the path and the body. One flat schema cannot
        // honestly describe both locations, so no tool may be emitted — an
        // absent tool is visible, a lying one is not.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/{id}")
                    .summary("Collides")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "string" } },
                        "required": ["id"]
                    }))
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "integer" } }
                    }))
                    .agent_tool("colliding_tool", "Should never be emitted."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "an endpoint with a cross-location name collision must produce no tool: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_whose_body_is_a_tagged_enum() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/conditions")
                    .summary("Set condition")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "oneOf": [
                            { "type": "object", "properties": { "kind": { "const": "always" } } }
                        ]
                    }))
                    .agent_tool(
                        "set_condition",
                        "Should never be emitted: body is a tagged enum.",
                    ),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a tagged-enum body must produce no tool: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_whose_body_is_an_array() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/bulk")
                    .summary("Bulk import")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({ "type": "array", "items": { "type": "string" } }))
                    .agent_tool("bulk_import", "Should never be emitted: body is an array."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "an array body must produce no tool: {doc}"
        );
    }

    /// A body that is nothing but the root-recursion marker: the rebased
    /// `{"$ref": "#/$defs/Root"}` top level has no named members, so there
    /// is no flat `inputSchema` to publish. Contrast
    /// `webmcp_publishes_an_endpoint_with_a_recursive_body_and_keeps_defs`,
    /// where the recursion sits *inside* an object body and the tool is
    /// published with the definition alongside it.
    #[test]
    fn webmcp_skips_an_endpoint_whose_body_is_a_root_recursive_ref() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/condition")
                    .summary("Set condition")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({ "$ref": "#" }))
                    .agent_tool(
                        "set_condition",
                        "Should never be emitted: body is a root-recursive $ref.",
                    ),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a root-recursive $ref body must produce no tool: {doc}"
        );
    }

    /// The recursion a real block declares: a `Condition` that nests
    /// `Condition`s inside an otherwise ordinary object body. The tool is
    /// published, the first reference is inlined, and the back-edge points
    /// at the definition the schema now carries.
    #[test]
    fn webmcp_publishes_an_endpoint_with_a_recursive_body_and_keeps_defs() {
        let blocks = vec![
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/test/offers")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "name": { "type": "string" }, "condition": { "$ref": "#/$defs/Condition" } },
                        "$defs": { "Condition": { "type": "object", "properties": {
                            "all": { "type": "array", "items": { "$ref": "#/$defs/Condition" } } } } }
                    }))
                    .agent_tool("create_offer", "Create an offer."),
            ]),
        ];
        let (doc, refused) = generate_webmcp_report(&blocks, AuthLevel::Public, |_b, ep| ep.auth);
        assert!(refused.is_empty(), "{refused:?}");
        let tool = &doc["tools"][0];
        assert_eq!(tool["name"], "create_offer");
        assert_eq!(
            tool["inputSchema"]["properties"]["condition"]["type"],
            "object"
        );
        assert_eq!(
            tool["inputSchema"]["$defs"]["Condition"]["properties"]["all"]["items"],
            json!({ "$ref": "#/$defs/Condition" })
        );
        assert_eq!(
            tool["invocation"]["body_params"],
            json!(["condition", "name"])
        );
    }

    /// The other half of the collision rule: two sources that keep a
    /// definition of the same name with the *same* body are describing one
    /// type, and one table entry describes both. Merging them is not a
    /// conflict and must not be reported as one — refusing here would delete
    /// every endpoint that takes the same recursive type in two places.
    ///
    /// Asserted on `agent_input_schema` rather than on a published manifest
    /// because the shape cannot reach one: a cyclic definition is an object,
    /// so whichever of path/query references it is a non-scalar URL
    /// parameter and the tool is refused for *that*. The second half of the
    /// test pins exactly this — the endpoint is refused, and not for a
    /// definition collision.
    #[test]
    fn agent_input_schema_merges_two_sources_that_keep_the_same_definition() {
        let shared = |field: &str| {
            json!({
                "type": "object",
                "properties": { field: { "$ref": "#/$defs/T" } },
                "$defs": {
                    "T": { "type": "object", "properties": { "n": { "$ref": "#/$defs/T" } } }
                }
            })
        };
        let ep = BlockEndpoint::post("/b/x/shared")
            .query_params_schema(shared("q"))
            .input_schema(shared("b"));

        let result = agent_input_schema(&ep);
        assert!(
            result.colliding_defs.is_empty(),
            "the same type declared twice is one type: {:?}",
            result.colliding_defs
        );
        assert_eq!(
            result.schema["$defs"],
            json!({ "T": { "type": "object", "properties": { "n": { "$ref": "#/$defs/T" } } } }),
            "one entry, not two and not a duplicate: {:?}",
            result.schema
        );
        for (source, field) in [("query", "q"), ("body", "b")] {
            assert_eq!(
                result.schema["properties"][field]["properties"]["n"],
                json!({ "$ref": "#/$defs/T" }),
                "the {source} source's back-edge resolves against the merged table: {:?}",
                result.schema
            );
        }

        let blocks = vec![
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![ep
                .auth(AuthLevel::Public)
                .agent_tool("shared", "Shared definition.")]),
        ];
        let (_, refused) = generate_webmcp_report(&blocks, AuthLevel::Public, |_b, e| e.auth);
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::NonScalarPathOrQueryParams {
                params: vec!["query.q".to_string()],
            },
            "refused for the object-valued query param, never for the shared definition: {refused:?}"
        );
    }

    /// One flat `inputSchema` has one `$defs` table, so two sources that
    /// each keep a definition of the same name with a different body cannot
    /// both be described by it. Picking a winner would misdescribe whichever
    /// source lost, so the tool is refused instead.
    #[test]
    fn webmcp_refuses_two_sources_defining_the_same_name_differently() {
        let blocks = vec![
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/test/x")
                    .auth(AuthLevel::Public)
                    .query_params_schema(json!({ "type": "object", "properties": { "q": { "$ref": "#/$defs/T" } },
                        "$defs": { "T": { "type": "object", "properties": { "n": { "$ref": "#/$defs/T" } } } } }))
                    .input_schema(json!({ "type": "object", "properties": { "b": { "$ref": "#/$defs/T" } },
                        "$defs": { "T": { "type": "object", "properties": { "m": { "$ref": "#/$defs/T" } } } } }))
                    .agent_tool("x", "x"),
            ]),
        ];
        let (doc, refused) = generate_webmcp_report(&blocks, AuthLevel::Public, |_b, ep| ep.auth);
        assert!(doc["tools"].as_array().unwrap().is_empty());
        assert!(
            matches!(refused[0].reason, WebMcpRefusal::CollidingDefinitions { ref names } if names == &["T".to_string()]),
            "{refused:?}"
        );
    }

    #[test]
    fn webmcp_drops_all_tools_sharing_a_duplicate_name() {
        // Two endpoints both claim `get_thing`. Neither may appear: keeping
        // either one arbitrarily would make the manifest's meaning depend
        // on which endpoint happened to be declared (or iterated) first.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "First get_thing."),
                BlockEndpoint::get("/b/x/b")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Second get_thing."),
                BlockEndpoint::get("/b/x/c")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_other_thing", "Uniquely named, must still appear."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            tool_names(&doc),
            vec!["get_other_thing".to_string()],
            "both endpoints sharing the duplicated name must be dropped, and the uniquely \
             named endpoint must still appear: {doc}"
        );
    }

    #[test]
    fn webmcp_duplicate_name_drop_is_order_independent() {
        let make_block = |order: [&str; 3]| {
            let by_id = |id: &str| match id {
                "a" => BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "First get_thing."),
                "b" => BlockEndpoint::get("/b/x/b")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Second get_thing."),
                "c" => BlockEndpoint::get("/b/x/c")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_other_thing", "Uniquely named."),
                other => panic!("unknown endpoint id: {other}"),
            };
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test")
                .endpoints(order.iter().map(|id| by_id(id)).collect())
        };

        let forward =
            generate_webmcp_declared_auth(&[make_block(["a", "b", "c"])], AuthLevel::Admin);
        let reversed =
            generate_webmcp_declared_auth(&[make_block(["c", "b", "a"])], AuthLevel::Admin);

        assert_eq!(tool_names(&forward), vec!["get_other_thing".to_string()]);
        assert_eq!(
            tool_names(&forward),
            tool_names(&reversed),
            "dropping duplicate-named tools must not depend on endpoint declaration order"
        );
    }

    #[test]
    fn webmcp_tool_carries_invocation_metadata() {
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Public);
        let tool = &doc["tools"][0];
        assert_eq!(tool["name"], json!("get_product"));
        assert_eq!(
            tool["description"],
            json!("Fetch a product and its offers by id.")
        );
        assert_eq!(tool["invocation"]["method"], json!("get"));
        assert_eq!(
            tool["invocation"]["path"],
            json!("/b/products/storefront/{product_id}")
        );
        assert_eq!(tool["invocation"]["path_params"], json!(["product_id"]));
        assert_eq!(tool["invocation"]["query_params"], json!([]));
        assert_eq!(tool["invocation"]["body_params"], json!([]));
        assert_eq!(
            tool["inputSchema"]["properties"]["product_id"],
            json!({ "type": "string" })
        );
    }

    #[test]
    fn webmcp_emits_schema_version_and_empty_tools_for_no_blocks() {
        let doc = generate_webmcp_declared_auth(&[], AuthLevel::Admin);
        assert_eq!(doc["schema_version"], json!(1));
        assert_eq!(doc["tools"], json!([]));
    }

    #[test]
    fn webmcp_public_manifest_matches_snapshot() {
        // Full-document snapshot at each auth level: exercises the exact
        // output an agent would receive, not just tool names, and is a
        // stronger determinism check than calling the pure function twice
        // in one process (which cannot fail).
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Public);
        assert_eq!(
            doc,
            json!({
                "schema_version": 1,
                "tools": [
                    {
                        "name": "get_product",
                        "description": "Fetch a product and its offers by id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "product_id": { "type": "string" } },
                            "required": ["product_id"]
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/storefront/{product_id}",
                            "path_params": ["product_id"],
                            "query_params": [],
                            "body_params": []
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn webmcp_authenticated_manifest_matches_snapshot() {
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Authenticated);
        assert_eq!(
            doc,
            json!({
                "schema_version": 1,
                "tools": [
                    {
                        "name": "get_product",
                        "description": "Fetch a product and its offers by id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "product_id": { "type": "string" } },
                            "required": ["product_id"]
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/storefront/{product_id}",
                            "path_params": ["product_id"],
                            "query_params": [],
                            "body_params": []
                        }
                    },
                    {
                        "name": "list_my_purchases",
                        "description": "List the signed-in user's purchases.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        },
                        "outputSchema": { "type": "object" },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/purchases",
                            "path_params": [],
                            "query_params": [],
                            "body_params": []
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn webmcp_admin_manifest_matches_snapshot() {
        let doc = generate_webmcp_declared_auth(&webmcp_fixture_blocks(), AuthLevel::Admin);
        assert_eq!(
            doc,
            json!({
                "schema_version": 1,
                "tools": [
                    {
                        "name": "get_product",
                        "description": "Fetch a product and its offers by id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "product_id": { "type": "string" } },
                            "required": ["product_id"]
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/storefront/{product_id}",
                            "path_params": ["product_id"],
                            "query_params": [],
                            "body_params": []
                        }
                    },
                    {
                        "name": "list_my_purchases",
                        "description": "List the signed-in user's purchases.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        },
                        "outputSchema": { "type": "object" },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/purchases",
                            "path_params": [],
                            "query_params": [],
                            "body_params": []
                        }
                    },
                    {
                        "name": "list_users",
                        "description": "List all user accounts.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        },
                        "outputSchema": { "type": "object" },
                        "invocation": {
                            "method": "get",
                            "path": "/b/admin/api/users",
                            "path_params": [],
                            "query_params": [],
                            "body_params": []
                        }
                    }
                ]
            })
        );
    }

    // -----------------------------------------------------------------
    // outputSchema
    // -----------------------------------------------------------------

    /// One block with a single opted-in endpoint carrying `output`, so the
    /// output-schema tests differ only in the schema under test.
    fn block_with_output_schema(output: Value) -> BlockInfo {
        BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
            BlockEndpoint::get("/b/x/thing")
                .auth(AuthLevel::Public)
                .output_schema(output)
                .agent_tool("get_thing", "Fetch a thing."),
        ])
    }

    #[test]
    fn webmcp_publishes_a_self_contained_output_schema() {
        // The same treatment `inputSchema` gets: references inlined, the
        // `$defs` table gone, the root `title` (the source Rust type's name)
        // dropped, and a `$ref` sibling annotation preserved.
        let block = block_with_output_schema(json!({
            "title": "Thing",
            "type": "object",
            "properties": {
                "status": { "description": "Lifecycle state.", "$ref": "#/$defs/Status" }
            },
            "required": ["status"],
            "$defs": { "Status": { "type": "string", "enum": ["draft", "active"] } }
        }));

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert!(refused.is_empty(), "nothing to report: {refused:?}");
        assert_eq!(
            doc["tools"][0]["outputSchema"],
            json!({
                "type": "object",
                "properties": {
                    "status": {
                        "description": "Lifecycle state.",
                        "type": "string",
                        "enum": ["draft", "active"]
                    }
                },
                "required": ["status"]
            }),
            "a client that cannot resolve $ref must still get the whole shape: {doc}"
        );
    }

    #[test]
    fn webmcp_omits_output_schema_when_the_endpoint_declares_none() {
        // No key at all rather than `null` or `{}` — an empty schema would
        // claim "the response can be anything", which is a claim, not a
        // silence. And nothing is reported: nothing was dropped.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/thing")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Fetch a thing."),
            ]);

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert!(refused.is_empty(), "nothing was declared: {refused:?}");
        assert!(
            doc["tools"][0].get("outputSchema").is_none(),
            "an undeclared response shape must leave the key absent: {doc}"
        );
    }

    #[test]
    fn webmcp_declaring_an_empty_output_schema_is_declaring_nothing() {
        for empty in [json!(null), json!({})] {
            let block = block_with_output_schema(empty.clone());
            let (doc, refused) =
                generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
            assert!(refused.is_empty(), "{empty} is not a defect: {refused:?}");
            assert!(
                doc["tools"][0].get("outputSchema").is_none(),
                "{empty} means nothing was declared, exactly as it does for the \
                 input sources: {doc}"
            );
        }
    }

    #[test]
    fn webmcp_publishes_the_tool_without_an_output_schema_it_cannot_vouch_for() {
        // The asymmetry this whole design turns on. The SAME dangling `$ref`
        // costs the tool on the input side and costs only the field on the
        // output side, because `inputSchema` is mandatory — omitting it is
        // itself the false claim "takes no arguments" — while `outputSchema`
        // is optional and its absence claims nothing.
        let dangling = json!({
            "type": "object",
            "properties": { "status": { "$ref": "#/$defs/Missing" } }
        });

        let (doc, refused) = generate_webmcp_report(
            &[block_with_output_schema(dangling.clone())],
            AuthLevel::Admin,
            |_, ep| ep.auth,
        );
        assert_eq!(
            tool_names(&doc),
            vec!["get_thing".to_string()],
            "a defect in the response shape says nothing about the arguments, \
             and a missing tool helps no one: {doc}"
        );
        assert!(
            doc["tools"][0].get("outputSchema").is_none(),
            "the field must be absent, not `{{}}`: {doc}"
        );
        assert_eq!(refused.len(), 1, "the drop must not be silent: {refused:?}");
        assert_eq!(refused[0].scope, WebMcpRefusalScope::OutputSchema);
        assert_eq!(refused[0].reason, WebMcpRefusal::OutputSchemaUnresolvedRef);
        let rendered = refused[0].to_string();
        assert!(
            rendered.contains("published without outputSchema"),
            "the rendered report must not read as a missing tool: {rendered}"
        );
        // And the reason text must name the declaration that is actually at
        // fault. The input side's identical verdict says "inputSchema",
        // which would send an author who declared only `.output::<T>()`
        // looking at their arguments.
        assert!(
            rendered.contains("output schema") && !rendered.contains("inputSchema"),
            "the output drop must not borrow the input side's wording: {rendered}"
        );

        // Same schema, input side: the tool goes.
        let on_input =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/thing")
                    .auth(AuthLevel::Public)
                    .input_schema(dangling)
                    .agent_tool("set_thing", "Set a thing."),
            ]);
        let (doc, refused) = generate_webmcp_report(&[on_input], AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(doc["tools"], json!([]), "{doc}");
        assert_eq!(refused[0].scope, WebMcpRefusalScope::Tool);
    }

    #[test]
    fn webmcp_drops_an_output_schema_that_does_not_describe_an_object() {
        // A `Vec<T>` response. `outputSchema` describes a structured result,
        // which is an object, so there is no honest slot for an array here —
        // and telling a validating client to check an array against an object
        // schema fails every call.
        let (doc, refused) = generate_webmcp_report(
            &[block_with_output_schema(json!({
                "type": "array",
                "items": { "type": "object", "properties": { "id": { "type": "string" } } }
            }))],
            AuthLevel::Admin,
            |_, ep| ep.auth,
        );
        assert_eq!(tool_names(&doc), vec!["get_thing".to_string()], "{doc}");
        assert!(doc["tools"][0].get("outputSchema").is_none(), "{doc}");
        assert_eq!(refused[0].scope, WebMcpRefusalScope::OutputSchema);
        assert_eq!(refused[0].reason, WebMcpRefusal::OutputSchemaNotAnObject);
        assert!(
            refused[0].to_string().contains("output schema"),
            "the reason text must name the output schema: {}",
            refused[0]
        );
    }

    #[test]
    fn webmcp_drops_a_structurally_malformed_output_schema() {
        // Object-shaped is not well-formed. `properties` must be an object
        // and `required` an array of strings; neither is true here, so this
        // is not a JSON Schema at all. Published verbatim it is the same
        // malformed tool *definition* the budget bailout can produce, which
        // a client that validates definitions rejects — taking the whole
        // tool down inside the consumer's per-tool try/catch, silently.
        // Only a hand-written schema reaches this; schemars never emits it.
        let (doc, refused) = generate_webmcp_report(
            &[block_with_output_schema(json!({
                "type": "object",
                "required": { "a": 1 },
                "properties": "oops"
            }))],
            AuthLevel::Admin,
            |_, ep| ep.auth,
        );
        assert_eq!(tool_names(&doc), vec!["get_thing".to_string()], "{doc}");
        assert!(
            doc["tools"][0].get("outputSchema").is_none(),
            "the malformed document must not ship verbatim: {doc}"
        );
        assert_eq!(refused[0].scope, WebMcpRefusalScope::OutputSchema);
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::OutputSchemaUnrepresentable
        );
        assert!(
            refused[0].to_string().contains("output schema"),
            "the reason text must name the output schema: {}",
            refused[0]
        );
    }

    #[test]
    fn webmcp_publishes_an_output_schema_that_only_looks_unusual() {
        // The wall is the input side's, which is stricter than this path
        // needs — so pin the shapes that must still get through: an
        // annotation-carrying object with a well-formed `required`, and a
        // closed one.
        for schema in [
            json!({
                "type": "object",
                "title": "Thing",
                "description": "A thing.",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "additionalProperties": false
            }),
        ] {
            let (doc, refused) = generate_webmcp_report(
                &[block_with_output_schema(schema.clone())],
                AuthLevel::Admin,
                |_, ep| ep.auth,
            );
            assert!(
                doc["tools"][0].get("outputSchema").is_some(),
                "{schema} must publish: {doc}"
            );
            assert!(refused.is_empty(), "{schema}: {refused:?}");
        }
    }

    #[test]
    fn webmcp_publishes_a_recursive_output_schema_with_its_defs() {
        // `struct Node { children: Vec<Node> }` — schemars closes the cycle
        // with `{"$ref": "#"}`. There is no finite *inlining* of it, but
        // there is a finite self-contained document: the recursion is
        // rebased onto a named definition the schema carries with it. An
        // output schema is a single source, so its table needs no hoisting
        // and travels exactly as `inline_refs` built it.
        let (doc, refused) = generate_webmcp_report(
            &[block_with_output_schema(json!({
                "type": "object",
                "properties": {
                    "children": { "type": "array", "items": { "$ref": "#" } }
                }
            }))],
            AuthLevel::Admin,
            |_, ep| ep.auth,
        );
        assert_eq!(tool_names(&doc), vec!["get_thing".to_string()], "{doc}");
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(
            doc["tools"][0]["outputSchema"],
            json!({
                "type": "object",
                "properties": {
                    "children": { "type": "array", "items": { "$ref": "#/$defs/Root" } }
                },
                "$defs": {
                    "Root": {
                        "type": "object",
                        "properties": {
                            "children": { "type": "array", "items": { "$ref": "#/$defs/Root" } }
                        }
                    }
                }
            }),
            "{doc}"
        );
    }

    #[test]
    fn webmcp_drops_an_output_schema_that_expands_past_the_node_budget() {
        // The output side pays into the same `MAX_INLINED_NODES` budget. It
        // matters most here: past the budget the walk emits `{}` wherever it
        // stopped, so a `required` array can come out as `"required": {}` —
        // not a weaker schema but a malformed one, which a client that
        // validates tool definitions rejects, taking the tool with it.
        let (doc, refused) = generate_webmcp_report(
            &[block_with_output_schema(doubling_schema(22))],
            AuthLevel::Admin,
            |_, ep| ep.auth,
        );
        assert_eq!(tool_names(&doc), vec!["get_thing".to_string()], "{doc}");
        assert!(doc["tools"][0].get("outputSchema").is_none(), "{doc}");
        assert_eq!(refused[0].scope, WebMcpRefusalScope::OutputSchema);
        assert_eq!(refused[0].reason, WebMcpRefusal::OutputSchemaTooLarge);
        let rendered = refused[0].to_string();
        assert!(
            rendered.contains("output schema") && !rendered.contains("inputSchema"),
            "the output drop must not borrow the input side's wording: {rendered}"
        );
    }

    #[test]
    fn webmcp_output_schema_drops_are_reported_only_where_the_tool_ships() {
        // A field-scoped refusal renders as "published without outputSchema",
        // which is a claim about a tool that is *in the manifest*. For a
        // caller who cannot see the endpoint no tool was published at all,
        // so the entry would describe something that does not exist and send
        // its reader hunting for it. The verdict is still caller-independent
        // — the same reason text reaches every caller who receives the tool
        // — it is only recorded where it is true.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/admin_thing")
                    .auth(AuthLevel::Admin)
                    .output_schema(json!({ "type": "array" }))
                    .agent_tool("get_admin_thing", "Fetch an admin thing."),
            ]);

        let (public_doc, public_refusals) =
            generate_webmcp_report(std::slice::from_ref(&block), AuthLevel::Public, |_, ep| {
                ep.auth
            });
        let (admin_doc, admin_refusals) =
            generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);

        assert_eq!(
            tool_names(&public_doc),
            Vec::<String>::new(),
            "the tool itself stays hidden: {public_doc}"
        );
        assert_eq!(
            public_refusals,
            Vec::new(),
            "no tool was published to this caller, so nothing was published \
             without an outputSchema: {public_refusals:?}"
        );

        assert_eq!(tool_names(&admin_doc), vec!["get_admin_thing".to_string()]);
        assert_eq!(admin_refusals.len(), 1, "{admin_refusals:?}");
        assert_eq!(admin_refusals[0].scope, WebMcpRefusalScope::OutputSchema);
        assert_eq!(
            admin_refusals[0].reason,
            WebMcpRefusal::OutputSchemaNotAnObject
        );
        assert!(admin_refusals[0].visible_to_caller);
    }

    #[test]
    fn webmcp_a_duplicated_name_is_not_also_reported_as_an_output_schema_drop() {
        // Two claimants, both with an unpublishable response schema. Neither
        // tool ships, so "published without outputSchema" is false for both
        // and only the collision — the thing that actually cost the tool —
        // is reported.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .output_schema(json!({ "type": "array" }))
                    .agent_tool("get_thing", "First."),
                BlockEndpoint::get("/b/x/b")
                    .auth(AuthLevel::Public)
                    .output_schema(json!({ "type": "array" }))
                    .agent_tool("get_thing", "Second."),
            ]);

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(doc["tools"], json!([]), "{doc}");
        assert_eq!(
            refused
                .iter()
                .map(|r| (r.scope, r.reason.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    WebMcpRefusalScope::Tool,
                    WebMcpRefusal::DuplicateToolName { count: 2 }
                ),
                (
                    WebMcpRefusalScope::Tool,
                    WebMcpRefusal::DuplicateToolName { count: 2 }
                ),
            ],
            "{refused:?}"
        );
    }

    // -----------------------------------------------------------------
    // path templates
    // -----------------------------------------------------------------

    #[test]
    fn path_placeholders_extracts_names_and_rejects_malformed_templates() {
        assert_eq!(path_placeholders("/b/x/list"), Ok(Vec::new()));
        assert_eq!(
            path_placeholders("/b/products/storefront/{product_id}"),
            Ok(vec!["product_id".to_string()])
        );
        assert_eq!(
            path_placeholders("/b/x/{a}/y/{b}"),
            Ok(vec!["a".to_string(), "b".to_string()])
        );

        for malformed in [
            "/b/x/{id",   // unclosed
            "/b/x/id}",   // unopened
            "/b/x/}{id}", // closer before opener
            "/b/x/{}",    // empty name
            "/b/x/{a{b}", // nested opener
        ] {
            assert_eq!(
                path_placeholders(malformed),
                Err(PathTemplateError::Malformed),
                "`{malformed}` is not a template this function can fill in"
            );
        }
    }

    /// The three shapes where a brace-anywhere scan disagreed with the
    /// router. Ground truth is `wafer_block::executor`: a segment is a
    /// placeholder only when the braces span the whole segment.
    #[test]
    fn path_placeholders_matches_the_routers_whole_segment_rule() {
        // Infix: the router compares the literal segment `v{version}`
        // against `v1` and 404s, so publishing a `version` param is a lie.
        assert_eq!(
            path_placeholders("/b/x/v{version}/items"),
            Err(PathTemplateError::Malformed),
            "an infix placeholder is not a placeholder to the router"
        );
        assert_eq!(
            path_placeholders("/b/x/{id}.json"),
            Err(PathTemplateError::Malformed),
            "a placeholder with a literal suffix is not a placeholder either"
        );

        // Two in one segment: the router reads ONE placeholder named `a}{b`,
        // so neither `req.param.a` nor `req.param.b` is ever set.
        assert_eq!(
            path_placeholders("/b/x/{a}{b}"),
            Err(PathTemplateError::Malformed),
            "the router cannot split one segment into two parameters"
        );

        // Wildcards: a trailing `/**` publishes as a literal path segment
        // that `match_path`'s trailing-`/**` special case then MATCHES, so
        // the handler runs against a garbage subpath rather than 404ing.
        assert_eq!(
            path_placeholders("/b/x/**"),
            Err(PathTemplateError::Wildcard {
                segment: "**".to_string()
            })
        );
        // A non-final `**` misses that special case and is compared
        // literally, so the published URL only matches a request whose
        // segment really is `**` — a tool whose route answers nothing.
        // Different failure, same refusal.
        assert_eq!(
            path_placeholders("/b/x/**/y"),
            Err(PathTemplateError::Wildcard {
                segment: "**".to_string()
            })
        );
        assert_eq!(
            path_placeholders("/b/x/*/y"),
            Err(PathTemplateError::Wildcard {
                segment: "*".to_string()
            })
        );

        // A literal `*` inside a longer segment is not a wildcard to this
        // router and is not treated as one here either.
        assert_eq!(path_placeholders("/b/x/a*b"), Ok(Vec::new()));
    }

    /// The shape that motivated this check: no production endpoint declares a
    /// `path_params` schema today, so the first annotated templated route
    /// would have shipped a tool whose client GETs a literal `{product_id}`.
    #[test]
    fn webmcp_skips_a_templated_path_with_no_declared_path_params() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/products/storefront/{product_id}")
                    .summary("Storefront product")
                    .auth(AuthLevel::Public)
                    .agent_tool(
                        "get_product",
                        "Should never be emitted: {product_id} unfilled.",
                    ),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a placeholder with no declared path param leaves a URL that can never \
             be built, so no tool may be emitted: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_a_templated_path_whose_param_name_disagrees() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/products/storefront/{product_id}")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "string" } },
                        "required": ["id"]
                    }))
                    .agent_tool("get_product", "Should never be emitted: name mismatch."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a declared param whose name does not match the placeholder still \
             leaves `{{product_id}}` unfilled: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_a_declared_path_param_with_no_placeholder() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/products/list")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "product_id": { "type": "string" } }
                    }))
                    .agent_tool(
                        "list_products",
                        "Should never be emitted: nowhere to put it.",
                    ),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a declared path param with no placeholder to go into would be a \
             required argument the client silently drops: {doc}"
        );
    }

    /// Guard against over-refusing: the check must pass the correct case.
    #[test]
    fn webmcp_emits_a_correctly_matched_templated_path() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/{owner}/items/{item_id}")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string" },
                            "item_id": { "type": "string" }
                        },
                        "required": ["owner", "item_id"]
                    }))
                    .agent_tool("get_item", "Fetch one item."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(tool_names(&doc), vec!["get_item".to_string()]);
        assert_eq!(
            doc["tools"][0]["invocation"]["path_params"],
            json!(["item_id", "owner"])
        );
    }

    #[test]
    fn webmcp_emits_a_non_templated_path_with_no_path_params() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/list")
                    .auth(AuthLevel::Public)
                    .agent_tool("list_things", "Nothing to fill in."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(tool_names(&doc), vec!["list_things".to_string()]);
    }

    // -----------------------------------------------------------------
    // effective auth
    // -----------------------------------------------------------------

    fn tiered_block() -> BlockInfo {
        BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
            // Declared Public, but a consumer may mount it under an
            // admin-only prefix and enforce max(prefix, declared).
            BlockEndpoint::get("/b/admin/api/stats")
                .auth(AuthLevel::Public)
                .agent_tool("get_stats", "Instance statistics."),
        ])
    }

    /// A consumer whose router enforces `max(prefix_tier, ep.auth)` cannot be
    /// described by `ep.auth` alone, and `generate_webmcp_declared_auth` structurally
    /// cannot see the prefix table. The resolver is where that knowledge
    /// belongs.
    #[test]
    fn webmcp_resolver_hides_a_tool_whose_effective_auth_exceeds_the_caller() {
        let blocks = [tiered_block()];
        let with_admin_prefix = |_block: &BlockInfo, ep: &BlockEndpoint| {
            if ep.path.starts_with("/b/admin/") {
                AuthLevel::Admin
            } else {
                ep.auth
            }
        };

        // The declared-auth wrapper trusts `ep.auth` and would publish it.
        assert_eq!(
            tool_names(&generate_webmcp_declared_auth(&blocks, AuthLevel::Public)),
            vec!["get_stats".to_string()],
            "declared-auth-only filtering advertises this to anonymous callers"
        );

        // A resolver that knows about the admin prefix must hide it.
        let doc = generate_webmcp(&blocks, AuthLevel::Public, with_admin_prefix);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a tool the router will 403 must not be advertised, and its name is \
             recon surface either way: {doc}"
        );
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("get_stats") && !rendered.contains("/b/admin/api/stats"),
            "nothing about the endpoint may leak: {rendered}"
        );

        // An admin caller is above the effective level and still sees it.
        let doc = generate_webmcp(&blocks, AuthLevel::Admin, with_admin_prefix);
        assert_eq!(tool_names(&doc), vec!["get_stats".to_string()]);
    }

    #[test]
    fn webmcp_resolver_reveals_a_tool_it_lowers() {
        // The filter runs the other way too: a route the consumer serves
        // without auth must not stay hidden behind a stricter declared level.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/public-mirror")
                    .auth(AuthLevel::Admin)
                    .agent_tool("read_mirror", "Publicly served by this consumer."),
            ]);
        let blocks = [block];

        assert_eq!(
            generate_webmcp_declared_auth(&blocks, AuthLevel::Public)["tools"],
            json!([]),
            "declared-auth-only filtering hides it"
        );
        assert_eq!(
            tool_names(&generate_webmcp(&blocks, AuthLevel::Public, |_, _| {
                AuthLevel::Public
            })),
            vec!["read_mirror".to_string()],
        );
    }

    #[test]
    fn webmcp_passes_the_owning_block_to_the_resolver() {
        // A consumer mounts *blocks* under prefixes, so the resolver has to
        // be able to key off the owning block, not just the endpoint.
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Public, |block, ep| {
            if block.name == "impresspress/products" {
                AuthLevel::Public
            } else {
                ep.auth
            }
        });
        let mut names = tool_names(&doc);
        names.sort();
        assert_eq!(
            names,
            vec!["get_product".to_string(), "list_my_purchases".to_string()],
            "the products block's authenticated endpoint is public under this \
             consumer's routing, and the admin block's is not: {doc}"
        );
    }

    // -----------------------------------------------------------------
    // duplicate names are counted per manifest, before every structural
    // verdict
    // -----------------------------------------------------------------

    #[test]
    fn webmcp_a_duplicate_in_a_higher_tier_does_not_reach_a_lower_manifest() {
        // The existence oracle this census is scoped to close. A public and
        // an admin endpoint both claim `get_thing`. Counting names globally
        // would drop the public tool for the anonymous caller — who, knowing
        // the public block (its source is not a secret), would infer from the
        // gap that an endpoint they cannot reach claims that name. The
        // anonymous manifest must be a function of the anonymous surface and
        // nothing else.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/public")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Public get_thing."),
                BlockEndpoint::get("/b/x/admin")
                    .auth(AuthLevel::Admin)
                    .agent_tool("get_thing", "Admin get_thing."),
                BlockEndpoint::get("/b/x/other")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_other_thing", "Uniquely named."),
            ]);
        let blocks = [block];

        for caller in [AuthLevel::Public, AuthLevel::Authenticated] {
            let doc = generate_webmcp_declared_auth(&blocks, caller);
            assert_eq!(
                tool_names(&doc),
                vec!["get_thing".to_string(), "get_other_thing".to_string()],
                "`get_thing` is unambiguous in {caller}'s manifest, and whether an \
                 admin endpoint also claims the name is not {caller}'s business: {doc}"
            );
            let rendered = doc.to_string();
            assert!(
                !rendered.contains("/b/x/admin") && !rendered.contains("Admin get_thing"),
                "the published tool must be the one this caller can invoke: {rendered}"
            );
        }

        // At the admin's own tier the name really is ambiguous, so neither
        // claimant may have it — and the collision is reported, to the one
        // caller who can do something about it.
        let (doc, refused) = generate_webmcp_report(&blocks, AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(
            tool_names(&doc),
            vec!["get_other_thing".to_string()],
            "{doc}"
        );
        let duplicates = refused
            .iter()
            .filter(|r| {
                r.tool_name == "get_thing"
                    && r.reason == WebMcpRefusal::DuplicateToolName { count: 2 }
            })
            .count();
        assert_eq!(
            duplicates, 2,
            "both claimants must be named in the operator's diagnostic: {refused:?}"
        );
    }

    #[test]
    fn webmcp_a_tool_name_never_denotes_two_endpoints_across_manifests() {
        // The property per-manifest uniqueness has to keep, and the one the
        // global census was introduced to protect. The auth filter is
        // monotone in the caller's rank, so a lower tier's candidate set is a
        // subset of a higher tier's: a name that is unique somewhere is
        // either claimed by that same endpoint at every tier that can see it,
        // or claimed by nobody. Present or absent, never ambiguous.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/public_thing")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Public get_thing."),
                BlockEndpoint::get("/b/x/admin_thing")
                    .auth(AuthLevel::Admin)
                    .agent_tool("get_thing", "Admin get_thing."),
                BlockEndpoint::get("/b/x/authed_only")
                    .auth(AuthLevel::Authenticated)
                    .agent_tool("get_authed_thing", "Authenticated-only tool."),
            ]);
        let blocks = [block];

        let mut seen: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        for caller in [
            AuthLevel::Public,
            AuthLevel::Authenticated,
            AuthLevel::Admin,
        ] {
            let doc = generate_webmcp_declared_auth(&blocks, caller);
            for tool in doc["tools"].as_array().expect("tools array") {
                let name = tool["name"].as_str().expect("tool name").to_string();
                if let Some(previous) = seen.get(&name) {
                    assert_eq!(
                        previous, &tool["invocation"],
                        "tool '{name}' points at a different endpoint for {caller} \
                         than it did for a lower tier"
                    );
                } else {
                    seen.insert(name, tool["invocation"].clone());
                }
            }
        }
        assert!(
            seen.contains_key("get_thing") && seen.contains_key("get_authed_thing"),
            "the fixture must actually exercise both a duplicated and a \
             tier-restricted name: {seen:?}"
        );
    }

    #[test]
    fn webmcp_duplicate_name_still_suppresses_a_side_filtered_out_for_another_reason() {
        // The second endpoint is dropped for a parameter collision. If
        // duplicate counting ran after that skip, the first would silently
        // inherit `get_thing` as though it had always been unique — the
        // collision filter would have laundered the ambiguity away.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Clean endpoint."),
                BlockEndpoint::post("/b/x/{id}")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "string" } }
                    }))
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "integer" } }
                    }))
                    .agent_tool("get_thing", "Colliding endpoint."),
            ]);

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a duplicated name must stay duplicated even when the other side is \
             dropped for an unrelated reason: {doc}"
        );
        // Scoping the census by auth did not scope it by structural verdict:
        // the broken endpoint still spends its claim on the name, so the
        // clean one is refused as a duplicate rather than inheriting it. The
        // broken one is reported by the defect its author can fix.
        assert_eq!(
            refused
                .iter()
                .map(|r| (r.path.as_str(), r.reason.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("/b/x/a", WebMcpRefusal::DuplicateToolName { count: 2 }),
                (
                    "/b/x/{id}",
                    WebMcpRefusal::CollidingParameterNames {
                        names: vec!["id".to_string()]
                    }
                ),
            ],
            "{refused:?}"
        );
    }

    // -----------------------------------------------------------------
    // unresolvable refs below the top level
    // -----------------------------------------------------------------

    #[test]
    fn agent_input_schema_keeps_a_nested_recursive_body_definition() {
        // `struct Node { children: Vec<Node> }` — schemars closes the cycle
        // with the root marker `{"$ref": "#"}`, which sits at
        // `properties.children.items`. The body source's root is rebased
        // onto a named definition, and that definition is hoisted into the
        // merged schema's single `$defs` table so the back-edge resolves
        // inside the document the agent receives.
        let ep = BlockEndpoint::post("/b/x/tree").input_schema(json!({
            "type": "object",
            "properties": {
                "children": { "type": "array", "items": { "$ref": "#" } }
            }
        }));
        let result = agent_input_schema(&ep);
        assert!(
            result.unrepresentable.is_empty(),
            "the top level is a healthy object: {:?}",
            result.unrepresentable
        );
        assert!(
            result.unresolved_refs.is_empty(),
            "{:?}",
            result.unresolved_refs
        );
        assert!(
            result.colliding_defs.is_empty(),
            "{:?}",
            result.colliding_defs
        );
        assert_eq!(
            result.schema["properties"]["children"]["items"],
            json!({ "$ref": "#/$defs/Root" }),
            "the cycle closes on a definition the schema carries, not on an \
             unconstrained `{{}}`: {:?}",
            result.schema
        );
        assert_eq!(
            result.schema["$defs"]["Root"]["properties"]["children"]["items"],
            json!({ "$ref": "#/$defs/Root" }),
            "{:?}",
            result.schema
        );
        assert_eq!(result.body_params, vec!["children".to_string()]);
    }

    #[test]
    fn webmcp_publishes_an_endpoint_with_a_nested_root_recursive_ref() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/tree")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": {
                            "children": { "type": "array", "items": { "$ref": "#" } }
                        }
                    }))
                    .agent_tool("set_tree", "Set a tree."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(tool_names(&doc), vec!["set_tree".to_string()], "{doc}");
        assert_eq!(
            doc["tools"][0]["inputSchema"]["properties"]["children"]["items"],
            json!({ "$ref": "#/$defs/Root" }),
            "{doc}"
        );
        assert_eq!(
            doc["tools"][0]["inputSchema"]["$defs"]["Root"]["properties"]["children"]["items"],
            json!({ "$ref": "#/$defs/Root" }),
            "the back-edge resolves inside the published document: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_with_a_ref_to_a_missing_def() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/thing")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "status": { "$ref": "#/$defs/Missing" } }
                    }))
                    .agent_tool("set_thing", "Should never be emitted: dangling ref."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a ref with no referent must refuse, not degrade to `{{}}`: {doc}"
        );
    }

    #[test]
    fn webmcp_emits_a_tool_whose_ref_name_is_percent_encoded() {
        // The other half of the ref fix: decoding must make legitimately-named
        // types resolve, so this endpoint is published rather than refused.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/thing")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "status": { "$ref": "#/$defs/Product%20Status" } },
                        "$defs": {
                            "Product Status": { "type": "string", "enum": ["draft", "active"] }
                        }
                    }))
                    .agent_tool("set_thing", "Set a product status."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        assert_eq!(tool_names(&doc), vec!["set_thing".to_string()]);
        assert_eq!(
            doc["tools"][0]["inputSchema"]["properties"]["status"],
            json!({ "type": "string", "enum": ["draft", "active"] })
        );
    }

    // -----------------------------------------------------------------
    // deprecation
    // -----------------------------------------------------------------

    #[test]
    fn webmcp_marks_a_deprecated_tool_without_hiding_it() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/old")
                    .auth(AuthLevel::Public)
                    .deprecated()
                    .agent_tool("get_old_thing", "Fetch a thing the old way."),
                BlockEndpoint::get("/b/x/new")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_new_thing", "Fetch a thing."),
            ]);

        let doc = generate_webmcp_declared_auth(&[block], AuthLevel::Admin);
        let tools = doc["tools"].as_array().expect("tools array");

        let old = tools
            .iter()
            .find(|t| t["name"] == "get_old_thing")
            .expect("a deprecated endpoint that still works is still published");
        assert_eq!(old["deprecated"], json!(true));
        assert_eq!(
            old["description"],
            json!("[Deprecated] Fetch a thing the old way."),
            "clients routinely forward only name/description/inputSchema to the \
             model, so the signal has to reach the description too"
        );

        let new = tools
            .iter()
            .find(|t| t["name"] == "get_new_thing")
            .expect("get_new_thing");
        assert!(
            new.get("deprecated").is_none(),
            "a live tool must not carry the key at all: {new}"
        );
        assert_eq!(new["description"], json!("Fetch a thing."));
    }

    // -----------------------------------------------------------------
    // methods that cannot carry a body
    // -----------------------------------------------------------------

    /// Build a one-endpoint block, and return the manifest plus the refusals
    /// an admin caller's generation produced.
    fn report_for(ep: BlockEndpoint) -> (Value, Vec<WebMcpRefusalReport>) {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![ep]);
        generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth)
    }

    #[test]
    fn webmcp_refuses_a_get_that_declares_a_request_body() {
        // The Fetch standard makes a GET with a body a TypeError, so a client
        // honouring `body_params` throws before the request leaves the page.
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/search")
                .auth(AuthLevel::Public)
                .input_schema(json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }))
                .agent_tool("search_things", "Search things."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new());
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::BodyOnBodylessMethod {
                body_params: vec!["query".to_string()],
            }
        );
    }

    #[test]
    fn webmcp_refuses_a_delete_that_declares_a_request_body() {
        let (doc, refused) = report_for(
            BlockEndpoint::delete("/b/x/things")
                .auth(AuthLevel::Public)
                .input_schema(json!({
                    "type": "object",
                    "properties": { "ids": { "type": "array", "items": { "type": "string" } } }
                }))
                .agent_tool("delete_things", "Delete things."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new());
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::BodyOnBodylessMethod {
                body_params: vec!["ids".to_string()],
            }
        );
    }

    #[test]
    fn webmcp_emits_a_get_with_only_path_and_query_params() {
        // Guard against over-refusal: the rule is about *body* arguments, not
        // about GET.
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/things/{id}")
                .auth(AuthLevel::Public)
                .path_params_schema(json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }))
                .query_params_schema(json!({
                    "type": "object",
                    "properties": { "expand": { "type": "string" } }
                }))
                .agent_tool("get_thing", "Fetch a thing."),
        );
        assert_eq!(tool_names(&doc), vec!["get_thing".to_string()]);
        assert!(refused.is_empty(), "{refused:?}");
    }

    #[test]
    fn webmcp_emits_a_post_that_declares_a_request_body() {
        let (doc, refused) = report_for(
            BlockEndpoint::post("/b/x/things")
                .auth(AuthLevel::Public)
                .input_schema(json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }))
                .agent_tool("create_thing", "Create a thing."),
        );
        assert_eq!(tool_names(&doc), vec!["create_thing".to_string()]);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(
            doc["tools"][0]["invocation"]["body_params"],
            json!(["name"])
        );
    }

    // -----------------------------------------------------------------
    // representability, end to end
    // -----------------------------------------------------------------

    #[test]
    fn webmcp_refuses_an_endpoint_whose_body_flattens_an_enum() {
        let (doc, refused) = report_for(
            BlockEndpoint::post("/b/x/rules")
                .auth(AuthLevel::Public)
                .input_schema(json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "oneOf": [
                        { "type": "object", "properties": { "kind": { "const": "a" } } },
                        { "type": "object", "properties": { "kind": { "const": "b" } } }
                    ]
                }))
                .agent_tool("create_rule", "Create a rule."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new());
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::UnrepresentableSources {
                sources: vec!["body".to_string()],
            }
        );
    }

    #[test]
    fn webmcp_emits_a_tool_for_a_fieldless_object_body() {
        let (doc, refused) = report_for(
            BlockEndpoint::post("/b/x/ping")
                .auth(AuthLevel::Public)
                .input_schema(json!({ "type": "object" }))
                .agent_tool("ping", "Ping the service."),
        );
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(
            doc["tools"][0]["inputSchema"],
            json!({ "type": "object", "properties": {} }),
            "a fieldless body takes no arguments, which is the truth: {doc}"
        );
    }

    // -----------------------------------------------------------------
    // path placeholders are emitted as required
    // -----------------------------------------------------------------

    #[test]
    fn webmcp_emits_a_path_param_as_required_even_when_the_source_did_not() {
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/things/{id}")
                .auth(AuthLevel::Public)
                .path_params_schema(json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } }
                }))
                .agent_tool("get_thing", "Fetch a thing."),
        );
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(
            doc["tools"][0]["inputSchema"]["required"],
            json!(["id"]),
            "an optional path param would let the agent omit it and the client \
             fetch /b/x/things/undefined: {doc}"
        );
    }

    // -----------------------------------------------------------------
    // the source root `title` is WebMCP-side noise
    // -----------------------------------------------------------------

    /// The stored schema keeps its root `title` — `/openapi.json` embeds
    /// these verbatim and client generators name types from it. The merged
    /// agent input schema drops it: it names the source Rust type, not the
    /// argument object the agent fills in.
    #[test]
    fn webmcp_input_schema_drops_the_source_root_title() {
        let source = json!({
            "title": "CreateThing",
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let (doc, refused) = report_for(
            BlockEndpoint::post("/b/x/things")
                .auth(AuthLevel::Public)
                .input_schema(source.clone())
                .agent_tool("create_thing", "Create a thing."),
        );
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(
            doc["tools"][0]["inputSchema"],
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
            "the Rust type name is noise for an agent: {doc}"
        );
        assert_eq!(
            source["title"],
            json!("CreateThing"),
            "and the stored schema is untouched, so /openapi.json still names \
             the generated type"
        );
    }

    // -----------------------------------------------------------------
    // tool-name validation at the generator
    // -----------------------------------------------------------------

    /// `BlockInfo::validate` rejects these at boot. If one reaches the
    /// generator anyway it must be skipped rather than emitted — and it must
    /// not be counted, or two endpoints that both forgot a name would share
    /// the empty-string bucket and turn each other into "duplicates".
    #[test]
    fn webmcp_refuses_an_invalid_tool_name_without_poisoning_other_names() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .agent_tool("", "Nameless."),
                BlockEndpoint::get("/b/x/b")
                    .auth(AuthLevel::Public)
                    .agent_tool("", "Also nameless."),
                BlockEndpoint::get("/b/x/c")
                    .auth(AuthLevel::Public)
                    .agent_tool("get thing", "Space in the name."),
                BlockEndpoint::get("/b/x/d")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Perfectly fine."),
            ]);

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(
            tool_names(&doc),
            vec!["get_thing".to_string()],
            "the valid tool must survive its neighbours' bad names: {doc}"
        );
        assert_eq!(refused.len(), 3);
        for refusal in &refused {
            assert_eq!(
                refusal.reason,
                WebMcpRefusal::InvalidToolName,
                "an empty name must be reported as invalid, never as a \
                 duplicate of the other empty one: {refusal}"
            );
        }
    }

    // -----------------------------------------------------------------
    // required names with no property behind them
    // -----------------------------------------------------------------

    #[test]
    fn agent_input_schema_reports_a_required_name_no_source_declared() {
        let ep = BlockEndpoint::post("/b/x/thing").input_schema(json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "required": ["a", "b"]
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.body_params, vec!["a".to_string()]);
        assert_eq!(
            result.undeclared_required,
            vec!["b".to_string()],
            "`b` is required by the server and has no property, so the client \
             has nowhere to put it: {:?}",
            result.schema
        );
    }

    #[test]
    fn webmcp_refuses_a_phantom_required_name_instead_of_filtering_it() {
        // Filtering `b` out of `required` would publish a tool the model can
        // call without it — and the server, whose own schema still demands
        // it, 400s every one of those calls. The lie is quieter, not smaller.
        let (doc, refused) = report_for(
            BlockEndpoint::post("/b/x/thing")
                .auth(AuthLevel::Public)
                .input_schema(json!({
                    "type": "object",
                    "properties": { "a": { "type": "string" } },
                    "required": ["a", "b"]
                }))
                .agent_tool("set_thing", "Should never be emitted: phantom required."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new(), "{doc}");
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::RequiredNotDeclared {
                names: vec!["b".to_string()],
            }
        );
    }

    // -----------------------------------------------------------------
    // structural keywords must have their structural types
    // -----------------------------------------------------------------

    /// `source_is_flattenable` used to check keyword *names* only, so a
    /// hand-written `"properties": "oops"` sailed through and published a
    /// tool advertising zero arguments, and `"required": "a"` published one
    /// whose required list had silently vanished.
    #[test]
    fn webmcp_refuses_a_source_whose_structural_keyword_has_the_wrong_type() {
        for (what, schema) in [
            ("properties is a string", json!({ "properties": "oops" })),
            (
                "properties is an array",
                json!({ "type": "object", "properties": [] }),
            ),
            (
                "required is a string",
                json!({
                    "type": "object",
                    "properties": { "a": { "type": "string" } },
                    "required": "a"
                }),
            ),
            (
                "required holds a non-string",
                json!({
                    "type": "object",
                    "properties": { "a": { "type": "string" } },
                    "required": [1]
                }),
            ),
        ] {
            let (doc, refused) = report_for(
                BlockEndpoint::post("/b/x/thing")
                    .auth(AuthLevel::Public)
                    .input_schema(schema)
                    .agent_tool("set_thing", "Should never be emitted."),
            );
            assert_eq!(
                tool_names(&doc),
                Vec::<String>::new(),
                "{what} must not publish a tool: {doc}"
            );
            assert_eq!(
                refused[0].reason,
                WebMcpRefusal::UnrepresentableSources {
                    sources: vec!["body".to_string()],
                },
                "{what}"
            );
        }
    }

    // -----------------------------------------------------------------
    // path and query values have to fit in a URL
    // -----------------------------------------------------------------

    #[test]
    fn webmcp_refuses_non_scalar_path_and_query_params() {
        // No serialization style travels in `invocation`, so an array param
        // could mean `?tags=a&tags=b`, `?tags=a,b`, or `?tags[]=a`. The
        // client comma-joins; an object stringifies to `[object Object]`.
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/search")
                .auth(AuthLevel::Public)
                .query_params_schema(json!({
                    "type": "object",
                    "properties": {
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "filter": { "type": "object" }
                    }
                }))
                .agent_tool("search_things", "Search things."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new(), "{doc}");
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::NonScalarPathOrQueryParams {
                params: vec!["query.filter".to_string(), "query.tags".to_string()],
            }
        );

        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/thing/{id}")
                .auth(AuthLevel::Public)
                .path_params_schema(json!({
                    "type": "object",
                    "properties": { "id": { "type": "array", "items": { "type": "string" } } },
                    "required": ["id"]
                }))
                .agent_tool("get_thing", "Get a thing."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new(), "{doc}");
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::NonScalarPathOrQueryParams {
                params: vec!["path.id".to_string()],
            }
        );

        // A param constrained no other way is refused too: nothing here says
        // it is a scalar, so nothing here can be vouched for.
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/search")
                .auth(AuthLevel::Public)
                .query_params_schema(json!({
                    "type": "object",
                    "properties": { "anything": {} }
                }))
                .agent_tool("search_things", "Search things."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new(), "{doc}");
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::NonScalarPathOrQueryParams {
                params: vec!["query.anything".to_string()],
            }
        );
    }

    /// The other half of the scalar rule: a naive "type must be exactly one
    /// scalar string" check would refuse `["string", "null"]` — what schemars
    /// emits for every `Option<T>` param — and every enum-only schema, which
    /// carries no `type` at all. Those are working tools and must survive.
    #[test]
    fn webmcp_emits_a_tool_whose_query_params_are_nullable_or_enum_shaped() {
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/search")
                .auth(AuthLevel::Public)
                .query_params_schema(json!({
                    "type": "object",
                    "properties": {
                        "cursor": { "type": ["string", "null"] },
                        "limit": { "type": "integer" },
                        "status": { "enum": ["draft", "active"] },
                        "exact": { "const": "yes" },
                        "sort": {
                            "anyOf": [
                                { "type": "string", "enum": ["asc", "desc"] },
                                { "type": "null" }
                            ]
                        }
                    }
                }))
                .agent_tool("search_things", "Search things."),
        );
        assert!(
            refused.is_empty(),
            "every one of these serializes as a single URL value: {refused:?}"
        );
        assert_eq!(tool_names(&doc), vec!["search_things".to_string()]);
        assert_eq!(
            doc["tools"][0]["invocation"]["query_params"],
            json!(["cursor", "exact", "limit", "sort", "status"])
        );
    }

    // -----------------------------------------------------------------
    // path templates the router would not read the same way
    // -----------------------------------------------------------------

    /// Each of these published a tool that failed on every call, because the
    /// generator's brace scan and `wafer_block::executor`'s whole-segment
    /// rule disagreed. See `path_placeholders`.
    #[test]
    fn webmcp_refuses_paths_the_router_would_not_read_the_same_way() {
        let declared = |name: &str| {
            json!({
                "type": "object",
                "properties": { name: { "type": "string" } },
                "required": [name]
            })
        };

        // Infix: the router compares the literal segment `v{version}`.
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/v{version}/items")
                .auth(AuthLevel::Public)
                .path_params_schema(declared("version"))
                .agent_tool("list_items", "List items."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new(), "{doc}");
        assert_eq!(refused[0].reason, WebMcpRefusal::MalformedPathTemplate);

        // Two placeholders in one segment: the router sees one, named `a}{b`.
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/{a}{b}")
                .auth(AuthLevel::Public)
                .path_params_schema(json!({
                    "type": "object",
                    "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
                    "required": ["a", "b"]
                }))
                .agent_tool("get_pair", "Get a pair."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new(), "{doc}");
        assert_eq!(refused[0].reason, WebMcpRefusal::MalformedPathTemplate);

        // Wildcard: this one does NOT 404 — `match_path`'s `/**`-prefix rule
        // matches the literal request, so the handler runs against a garbage
        // `**` subpath. A silent wrong answer is worse than a missing tool.
        let (doc, refused) = report_for(
            BlockEndpoint::get("/b/x/files/**")
                .auth(AuthLevel::Public)
                .agent_tool("read_file", "Read a file."),
        );
        assert_eq!(tool_names(&doc), Vec::<String>::new(), "{doc}");
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::WildcardPathSegment {
                segment: "**".to_string(),
            }
        );
    }

    // -----------------------------------------------------------------
    // refusal diagnostics
    // -----------------------------------------------------------------

    /// Every refusal path names the endpoint precisely enough to find in
    /// source, and says which rule it broke. Before this, all of them were a
    /// bare `continue` and an author who annotated an endpoint and got no
    /// tool had nothing to go on.
    #[test]
    fn webmcp_report_names_the_endpoint_and_the_reason_for_every_refusal() {
        let colliding = json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        });

        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
            // 1. invalid name
            BlockEndpoint::get("/b/x/invalid").agent_tool("get thing", "Space in the name."),
            // 2 + 3. duplicate name (both sides)
            BlockEndpoint::get("/b/x/dup_a").agent_tool("dup", "One."),
            BlockEndpoint::get("/b/x/dup_b").agent_tool("dup", "Two."),
            // 4. colliding parameter names
            BlockEndpoint::get("/b/x/collide/{id}")
                .path_params_schema(colliding.clone())
                .query_params_schema(colliding.clone())
                .agent_tool("collide", "Collide."),
            // 5. unrepresentable source
            BlockEndpoint::post("/b/x/enum")
                .input_schema(json!({ "oneOf": [{ "type": "object" }] }))
                .agent_tool("tagged_enum_body", "Tagged enum body."),
            // 6. unresolved `$ref`
            BlockEndpoint::post("/b/x/tree")
                .input_schema(json!({
                    "type": "object",
                    "properties": { "child": { "$ref": "#/$defs/Missing" } }
                }))
                .agent_tool("dangling_ref", "Dangling ref."),
            // 7. malformed path template
            BlockEndpoint::get("/b/x/broken/{id").agent_tool("malformed", "Malformed."),
            // 8. path params disagree with the template
            BlockEndpoint::get("/b/x/mismatch/{product_id}")
                .path_params_schema(colliding)
                .agent_tool("mismatch", "Mismatch."),
            // 9. body on a body-less method
            BlockEndpoint::get("/b/x/get_with_body")
                .input_schema(json!({
                    "type": "object",
                    "properties": { "q": { "type": "string" } }
                }))
                .agent_tool("get_with_body", "Get with a body."),
            // 10. two sources whose kept definitions collide
            BlockEndpoint::post("/b/x/colliding_defs")
                .query_params_schema(json!({
                    "type": "object",
                    "properties": { "q": { "$ref": "#/$defs/T" } },
                    "$defs": {
                        "T": { "type": "object", "properties": { "n": { "$ref": "#/$defs/T" } } }
                    }
                }))
                .input_schema(json!({
                    "type": "object",
                    "properties": { "b": { "$ref": "#/$defs/T" } },
                    "$defs": {
                        "T": { "type": "object", "properties": { "m": { "$ref": "#/$defs/T" } } }
                    }
                }))
                .agent_tool("colliding_defs", "Colliding definitions."),
            // 11. required name with no property behind it
            BlockEndpoint::post("/b/x/phantom")
                .input_schema(json!({
                    "type": "object",
                    "properties": { "a": { "type": "string" } },
                    "required": ["a", "b"]
                }))
                .agent_tool("phantom_required", "Phantom required."),
            // 12. non-scalar query param
            BlockEndpoint::get("/b/x/tagged")
                .query_params_schema(json!({
                    "type": "object",
                    "properties": { "tags": { "type": "array", "items": { "type": "string" } } }
                }))
                .agent_tool("array_query", "Array query."),
            // 13. router wildcard segment
            BlockEndpoint::get("/b/x/files/**").agent_tool("wildcard_path", "Wildcard path."),
        ]);

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(
            tool_names(&doc),
            Vec::<String>::new(),
            "every endpoint here is broken: {doc}"
        );

        let by_tool: std::collections::HashMap<&str, &WebMcpRefusalReport> =
            refused.iter().map(|r| (r.tool_name.as_str(), r)).collect();

        let expected: Vec<(&str, &str, WebMcpRefusal)> = vec![
            ("get thing", "/b/x/invalid", WebMcpRefusal::InvalidToolName),
            (
                "collide",
                "/b/x/collide/{id}",
                WebMcpRefusal::CollidingParameterNames {
                    names: vec!["id".to_string()],
                },
            ),
            (
                "tagged_enum_body",
                "/b/x/enum",
                WebMcpRefusal::UnrepresentableSources {
                    sources: vec!["body".to_string()],
                },
            ),
            (
                "dangling_ref",
                "/b/x/tree",
                WebMcpRefusal::UnresolvedRefs {
                    sources: vec!["body".to_string()],
                },
            ),
            (
                "malformed",
                "/b/x/broken/{id",
                WebMcpRefusal::MalformedPathTemplate,
            ),
            (
                "mismatch",
                "/b/x/mismatch/{product_id}",
                WebMcpRefusal::PathParamsDisagreeWithTemplate {
                    placeholders: vec!["product_id".to_string()],
                    declared: vec!["id".to_string()],
                },
            ),
            (
                "get_with_body",
                "/b/x/get_with_body",
                WebMcpRefusal::BodyOnBodylessMethod {
                    body_params: vec!["q".to_string()],
                },
            ),
            (
                "colliding_defs",
                "/b/x/colliding_defs",
                WebMcpRefusal::CollidingDefinitions {
                    names: vec!["T".to_string()],
                },
            ),
            (
                "phantom_required",
                "/b/x/phantom",
                WebMcpRefusal::RequiredNotDeclared {
                    names: vec!["b".to_string()],
                },
            ),
            (
                "array_query",
                "/b/x/tagged",
                WebMcpRefusal::NonScalarPathOrQueryParams {
                    params: vec!["query.tags".to_string()],
                },
            ),
            (
                "wildcard_path",
                "/b/x/files/**",
                WebMcpRefusal::WildcardPathSegment {
                    segment: "**".to_string(),
                },
            ),
        ];

        for (tool_name, path, reason) in expected {
            let refusal = by_tool
                .get(tool_name)
                .unwrap_or_else(|| panic!("no refusal reported for {tool_name}: {refused:?}"));
            assert_eq!(refusal.reason, reason, "wrong reason for {tool_name}");
            assert_eq!(refusal.path, path);
            assert_eq!(refusal.block, "test/block");

            // The rendered line has to be enough to find the endpoint.
            let rendered = refusal.to_string();
            assert!(rendered.contains("test/block"), "{rendered}");
            assert!(rendered.contains(path), "{rendered}");
            assert!(rendered.contains(tool_name), "{rendered}");
            assert!(rendered.contains(&refusal.method.to_string()), "{rendered}");
        }

        // Both sides of the duplicate are reported, each naming the count.
        let duplicates: Vec<&WebMcpRefusalReport> =
            refused.iter().filter(|r| r.tool_name == "dup").collect();
        assert_eq!(duplicates.len(), 2, "{refused:?}");
        for refusal in duplicates {
            assert_eq!(
                refusal.reason,
                WebMcpRefusal::DuplicateToolName { count: 2 }
            );
        }
    }

    /// An endpoint whose body schema multiplies out is refused with a
    /// diagnostic that names the real cause. It is checked before every other
    /// schema-shaped verdict because the inlined schema in hand is truncated,
    /// so anything else read off it would describe a document the endpoint
    /// never declared.
    #[test]
    fn webmcp_refuses_an_endpoint_whose_schema_expands_past_the_node_budget() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/wide")
                    .input_schema(doubling_schema(22))
                    .agent_tool("wide_body", "Wide body."),
            ]);

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(
            tool_names(&doc),
            Vec::<String>::new(),
            "a schema that cannot be inlined publishes no tool: {doc}"
        );
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(
            refused[0].reason,
            WebMcpRefusal::SchemaTooLarge {
                sources: vec!["body".to_string()]
            },
            "not a missing definition and not a cycle: {refused:?}"
        );

        let rendered = refused[0].to_string();
        assert!(
            rendered.contains(&MAX_INLINED_NODES.to_string()),
            "the diagnostic must name the budget that was broken: {rendered}"
        );
        assert!(
            rendered.contains("/b/x/wide") && rendered.contains("wide_body"),
            "and the endpoint it was broken by: {rendered}"
        );
    }

    /// The budget refuses runaway expansions, not large endpoints. A body far
    /// bigger than anything a block declares still publishes, with every
    /// reference inlined.
    #[test]
    fn webmcp_publishes_a_large_but_realistic_body_schema() {
        let mut record = serde_json::Map::new();
        for field in 0..12 {
            record.insert(format!("f{field}"), json!({ "type": "string" }));
        }
        let mut properties = serde_json::Map::new();
        for field in 0..150 {
            properties.insert(format!("p{field}"), json!({ "$ref": "#/$defs/Record" }));
        }

        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/big")
                    .input_schema(json!({
                        "type": "object",
                        "properties": properties,
                        "$defs": { "Record": { "type": "object", "properties": record } }
                    }))
                    .agent_tool("big_body", "Big body."),
            ]);

        let (doc, refused) = generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);
        assert!(refused.is_empty(), "nothing is wrong with it: {refused:?}");
        assert_eq!(tool_names(&doc), vec!["big_body".to_string()], "{doc}");
        assert_eq!(
            doc["tools"][0]["inputSchema"]["properties"]["p149"]["properties"]["f11"],
            json!({ "type": "string" }),
            "and it is inlined in full: {doc}"
        );
    }

    /// A structural refusal is a defect report about the endpoint, not about
    /// the caller, so the diagnostic itself must not vary with `caller` — an
    /// author debugging a missing admin tool should not have to authenticate
    /// to see why. What *does* vary is `visible_to_caller`, which is how a
    /// consumer rendering one caller's own view knows to drop it.
    #[test]
    fn webmcp_refusals_do_not_depend_on_the_caller() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/admin_broken/{id}")
                    .auth(AuthLevel::Admin)
                    .agent_tool("admin_broken", "Broken admin tool."),
            ]);

        let (public_doc, public_refusals) =
            generate_webmcp_report(std::slice::from_ref(&block), AuthLevel::Public, |_, ep| {
                ep.auth
            });
        let (_, admin_refusals) =
            generate_webmcp_report(&[block], AuthLevel::Admin, |_, ep| ep.auth);

        assert_eq!(tool_names(&public_doc), Vec::<String>::new());
        let diagnostic = |r: &WebMcpRefusalReport| {
            (
                r.block.clone(),
                r.path.clone(),
                r.tool_name.clone(),
                r.scope,
                r.reason.clone(),
            )
        };
        assert_eq!(
            public_refusals.iter().map(diagnostic).collect::<Vec<_>>(),
            admin_refusals.iter().map(diagnostic).collect::<Vec<_>>(),
        );
        assert_eq!(
            public_refusals[0].reason,
            WebMcpRefusal::PathParamsDisagreeWithTemplate {
                placeholders: vec!["id".to_string()],
                declared: Vec::new(),
            }
        );

        // The endpoint is admin-only, so a page rendering "what the public
        // caller receives" must not name it. That decision belongs to the
        // producer's own filter, and this is how it travels.
        assert!(!public_refusals[0].visible_to_caller);
        assert!(admin_refusals[0].visible_to_caller);
    }

    /// Hiding a tool the caller may not invoke is the projection working, so
    /// it must not be reported as a refusal — the refusal channel is for
    /// defects an author has to fix.
    #[test]
    fn webmcp_auth_filtering_is_not_reported_as_a_refusal() {
        let (doc, refused) =
            generate_webmcp_report(&webmcp_fixture_blocks(), AuthLevel::Public, |_, ep| ep.auth);
        assert_eq!(tool_names(&doc), vec!["get_product".to_string()]);
        assert!(
            refused.is_empty(),
            "the hidden admin and authenticated tools are healthy, just \
             out of reach: {refused:?}"
        );
    }

    #[test]
    fn webmcp_matches_the_report_variant_manifest() {
        let blocks = webmcp_fixture_blocks();
        let (reported, _) = generate_webmcp_report(&blocks, AuthLevel::Admin, |_, ep| ep.auth);
        assert_eq!(
            generate_webmcp_declared_auth(&blocks, AuthLevel::Admin),
            reported
        );
    }
}
