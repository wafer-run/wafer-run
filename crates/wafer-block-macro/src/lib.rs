use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    parse2,
    punctuated::Punctuated,
    spanned::Spanned,
    Expr, ExprLit, Ident, ImplItem, ImplItemFn, ItemImpl, Lit, Token,
};

// ---------------------------------------------------------------------------
// Attribute argument parsing
// ---------------------------------------------------------------------------

struct KeyValue {
    key: Ident,
    _eq: Token![=],
    value: Expr,
}

impl Parse for KeyValue {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(KeyValue {
            key: input.parse()?,
            _eq: input.parse()?,
            value: input.parse()?,
        })
    }
}

// ---------------------------------------------------------------------------
// capabilities(...) parsing
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CapabilitiesArgs {
    crypto: bool,
    network: bool,
    raw_sql: bool,
    ddl: bool,
    schema: bool,
    config: bool,
    collections: Vec<String>,
    storage_folders: Vec<String>,
    network_allow: Vec<String>,
    config_keys: Vec<String>,
    vector_indexes: Vec<String>,
    callable_blocks: Vec<String>,
    headers_readable: Vec<String>,
    headers_writable: Vec<String>,
    headers_masked: Vec<String>,
}

fn parse_capabilities(meta: &syn::MetaList) -> syn::Result<CapabilitiesArgs> {
    let mut out = CapabilitiesArgs::default();
    let nested: Punctuated<syn::Meta, Token![,]> =
        meta.parse_args_with(Punctuated::parse_terminated)?;
    for item in nested {
        match item {
            syn::Meta::Path(p) => {
                let ident = p
                    .get_ident()
                    .ok_or_else(|| {
                        syn::Error::new(
                            p.span(),
                            "#[wafer_block]: expected identifier in capabilities(...)",
                        )
                    })?
                    .to_string();
                match ident.as_str() {
                    "crypto" => out.crypto = true,
                    "network" => out.network = true,
                    "raw_sql" => out.raw_sql = true,
                    "ddl" => out.ddl = true,
                    "schema" => out.schema = true,
                    "config" => out.config = true,
                    other => {
                        return Err(syn::Error::new(
                            p.span(),
                            format!("#[wafer_block]: unknown bool capability '{other}'"),
                        ));
                    }
                }
            }
            syn::Meta::NameValue(nv) => {
                let ident = nv
                    .path
                    .get_ident()
                    .ok_or_else(|| {
                        syn::Error::new(
                            nv.path.span(),
                            "#[wafer_block]: expected identifier in capabilities(...)",
                        )
                    })?
                    .to_string();
                let list = parse_string_list(&nv.value)?;
                match ident.as_str() {
                    "collections" => out.collections = list,
                    "storage_folders" => out.storage_folders = list,
                    "network_allow" => out.network_allow = list,
                    "config_keys" => out.config_keys = list,
                    "vector_indexes" => out.vector_indexes = list,
                    "callable_blocks" => out.callable_blocks = list,
                    other => {
                        return Err(syn::Error::new(
                            nv.path.span(),
                            format!("#[wafer_block]: unknown list capability '{other}'"),
                        ));
                    }
                }
            }
            syn::Meta::List(inner) if inner.path.is_ident("headers") => {
                parse_headers_nested(&inner, &mut out)?;
            }
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "#[wafer_block]: unexpected token in capabilities(...)",
                ));
            }
        }
    }
    Ok(out)
}

fn parse_headers_nested(inner: &syn::MetaList, out: &mut CapabilitiesArgs) -> syn::Result<()> {
    let nested: Punctuated<syn::Meta, Token![,]> =
        inner.parse_args_with(Punctuated::parse_terminated)?;
    for item in nested {
        match item {
            syn::Meta::NameValue(nv) => {
                let ident = nv
                    .path
                    .get_ident()
                    .ok_or_else(|| {
                        syn::Error::new(
                            nv.path.span(),
                            "#[wafer_block]: expected ident in headers(...)",
                        )
                    })?
                    .to_string();
                let list = parse_string_list(&nv.value)?;
                match ident.as_str() {
                    "readable" => out.headers_readable = list,
                    "writable" => out.headers_writable = list,
                    "masked" => out.headers_masked = list,
                    other => {
                        return Err(syn::Error::new(
                            nv.path.span(),
                            format!("#[wafer_block]: unknown headers field '{other}'"),
                        ));
                    }
                }
            }
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "#[wafer_block]: headers(...) only supports name = [...] pairs",
                ));
            }
        }
    }
    Ok(())
}

/// Emit an `Allowlist` token from the DSL's `(flag, allow_list)` pair (SEC-06),
/// preserving the pre-enum semantics: flag off → `None`, flag on + empty list →
/// `Any`, flag on + list → `Only`.
fn allowlist_tokens(flag: bool, allow: &[String]) -> proc_macro2::TokenStream {
    if !flag {
        quote::quote! { wafer_block::capabilities::Allowlist::None }
    } else if allow.is_empty() {
        quote::quote! { wafer_block::capabilities::Allowlist::Any }
    } else {
        quote::quote! {
            wafer_block::capabilities::Allowlist::Only({
                let mut s = ::std::collections::BTreeSet::new();
                #(s.insert(#allow.to_string());)*
                s
            })
        }
    }
}

/// Emit an `Allowlist` token for the wildcard-list capability fields
/// (`collections`, `storage_folders`, `vector_indexes`, `callable_blocks`),
/// preserving the pre-enum DSL sentinels: the list `["*"]` → `Any`, an absent
/// (empty) list → `None`, anything else → `Only`.
fn wildcard_list_tokens(allow: &[String]) -> proc_macro2::TokenStream {
    if allow.iter().any(|s| s == "*") {
        quote::quote! { wafer_block::capabilities::Allowlist::Any }
    } else if allow.is_empty() {
        quote::quote! { wafer_block::capabilities::Allowlist::None }
    } else {
        quote::quote! {
            wafer_block::capabilities::Allowlist::Only({
                let mut s = ::std::collections::BTreeSet::new();
                #(s.insert(#allow.to_string());)*
                s
            })
        }
    }
}

fn parse_string_list(expr: &syn::Expr) -> syn::Result<Vec<String>> {
    let arr = match expr {
        syn::Expr::Array(arr) => arr,
        other => {
            return Err(syn::Error::new(
                other.span(),
                "#[wafer_block]: expected array expression `[...]`",
            ));
        }
    };
    arr.elems
        .iter()
        .map(|e| match e {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Ok(s.value()),
            other => Err(syn::Error::new(
                other.span(),
                "#[wafer_block]: expected string literal in array",
            )),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Top-level attribute token handling — two-pass approach
// ---------------------------------------------------------------------------
//
// The existing Args parser handles `key = value` pairs using a custom Parse
// impl. `capabilities(...)` is a MetaList and cannot be expressed as a
// `KeyValue`. We therefore parse the attribute token stream using `syn::Meta`
// items first, then split them into flat key=value pairs (for the existing
// Args helpers) and capabilities groups.

/// Flat key=value attribute arg. Repurposed from `Args` but now drives both
/// passes without owning the whole argument list.
struct FlatArgs {
    pairs: Punctuated<KeyValue, Token![,]>,
}

impl Parse for FlatArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(FlatArgs {
            pairs: Punctuated::parse_terminated(input)?,
        })
    }
}

impl FlatArgs {
    fn get_str(&self, key: &str) -> Option<String> {
        self.pairs.iter().find(|kv| kv.key == key).and_then(|kv| {
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &kv.value
            {
                Some(s.value())
            } else {
                None
            }
        })
    }

    /// Returns the string-array value of `key = [...]`, or an empty list if
    /// `key` is absent. If `key` is present but its value isn't an array of
    /// string literals, returns a spanned error rather than silently dropping
    /// the malformed entries — an attribute like `requires` is a runtime
    /// access-control gate, so a mistyped value (e.g. a bare string instead of
    /// an array) must never resolve to an empty ("unrestricted") list.
    fn get_str_list(&self, key: &str) -> syn::Result<Vec<String>> {
        self.pairs
            .iter()
            .find(|kv| kv.key == key)
            .map(|kv| parse_string_list(&kv.value))
            .transpose()
            .map(Option::unwrap_or_default)
    }
}

/// Parsed skill(...) attribute — description and the JSON parameters schema for
/// the LLM-facing tool definition.
#[derive(Debug)]
struct SkillArgs {
    description: String,
    /// Tokens of an expression evaluating to the JSON-schema string. For the
    /// string-literal form this is the validated literal (JSON checked at
    /// macro-expansion time); for the expression form (e.g.
    /// `parameters = my_core::schema_json()`) these are the author's tokens,
    /// with JSON validity enforced at runtime in `block_info()` (and by the
    /// consumer's test) instead of at the macro site.
    parameters: proc_macro2::TokenStream,
}

/// Parse a `skill(description = "...", parameters = <literal | expr>)` MetaList.
///
/// `parameters` accepts either a string literal (its JSON is validated here at
/// macro-expansion time) or any expression evaluating to a value `AsRef<str>`
/// that yields the JSON schema (e.g. `parameters = my_core::schema_json()`),
/// whose validity is enforced at runtime in `block_info()`.
fn parse_skill(meta: &syn::MetaList) -> syn::Result<SkillArgs> {
    let nested: Punctuated<syn::Meta, Token![,]> =
        meta.parse_args_with(Punctuated::parse_terminated)?;
    let mut description = None::<String>;
    let mut parameters = None::<proc_macro2::TokenStream>;
    for item in nested {
        let nv = match item {
            syn::Meta::NameValue(nv) => nv,
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "#[wafer_block]: unexpected token in skill(...)",
                ));
            }
        };
        let key = nv
            .path
            .get_ident()
            .ok_or_else(|| {
                syn::Error::new(
                    nv.path.span(),
                    "#[wafer_block]: expected identifier in skill(...)",
                )
            })?
            .to_string();
        match key.as_str() {
            "description" => {
                // description is always a string literal.
                let s = match &nv.value {
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) => s.value(),
                    other => {
                        return Err(syn::Error::new(
                            other.span(),
                            "#[wafer_block]: skill(description) value must be a string literal",
                        ));
                    }
                };
                description = Some(s);
            }
            "parameters" => {
                // Two accepted forms:
                //  1) a string literal — validate the JSON at macro-expansion
                //     time (author-controlled, fully known here) and emit the
                //     literal; preserves the original behavior exactly.
                //  2) any expression evaluating to a value `AsRef<str>` (e.g.
                //     `my_core::schema_json()`) — emit the tokens verbatim;
                //     JSON validity is enforced at runtime in `block_info()`.
                match &nv.value {
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) => {
                        let json = s.value();
                        if let Err(e) = serde_json::from_str::<serde_json::Value>(&json) {
                            return Err(syn::Error::new_spanned(
                                s,
                                format!(
                                    "#[wafer_block]: skill `parameters` is not valid JSON: {e}"
                                ),
                            ));
                        }
                        parameters = Some(quote! { #s });
                    }
                    expr => {
                        parameters = Some(quote! { #expr });
                    }
                }
            }
            other => {
                return Err(syn::Error::new(
                    nv.path.span(),
                    format!("#[wafer_block]: unknown skill attribute '{other}'"),
                ));
            }
        }
    }
    let description = description.ok_or_else(|| {
        syn::Error::new(
            meta.span(),
            "#[wafer_block]: skill(...) requires description = \"...\"",
        )
    })?;
    let parameters = parameters.ok_or_else(|| {
        syn::Error::new(
            meta.span(),
            "#[wafer_block]: skill(...) requires parameters = \"...\" or an expression",
        )
    })?;
    Ok(SkillArgs {
        description,
        parameters,
    })
}

/// Parsed attribute: flat key=value pairs, optional capabilities group, and
/// optional skill(...) group.
struct ParsedAttr {
    flat: FlatArgs,
    capabilities: Option<CapabilitiesArgs>,
    skill: Option<SkillArgs>,
}

/// Parse the full attribute token stream by splitting on `capabilities(...)` and
/// `skill(...)`.
///
/// Strategy: scan the token stream for `capabilities` / `skill`, peel them off
/// into their respective `syn::MetaList`s, forward everything else to `FlatArgs`.
fn parse_attr(attr: TokenStream) -> syn::Result<ParsedAttr> {
    use proc_macro2::TokenStream as Ts2;
    use quote::ToTokens;

    let ts2: Ts2 = attr.into();
    let metas: Punctuated<syn::Meta, Token![,]> = if ts2.is_empty() {
        Punctuated::new()
    } else {
        parse2::<MetaList>(ts2)?.0
    };

    let mut caps: Option<CapabilitiesArgs> = None;
    let mut skill: Option<SkillArgs> = None;
    let mut flat_tokens: Vec<proc_macro2::TokenStream> = Vec::new();

    for meta in &metas {
        match meta {
            syn::Meta::List(ml) if ml.path.is_ident("capabilities") => {
                if caps.is_some() {
                    return Err(syn::Error::new(
                        ml.span(),
                        "#[wafer_block]: `capabilities(...)` may only be specified once",
                    ));
                }
                caps = Some(parse_capabilities(ml)?);
            }
            syn::Meta::List(ml) if ml.path.is_ident("skill") => {
                if skill.is_some() {
                    return Err(syn::Error::new(
                        ml.span(),
                        "#[wafer_block]: `skill(...)` may only be specified once",
                    ));
                }
                skill = Some(parse_skill(ml)?);
            }
            other => {
                flat_tokens.push(other.to_token_stream());
            }
        }
    }

    // `capabilities(...)` and `skill(...)` compose: skill declares the
    // LLM-facing tool contract, capabilities declares the runtime sandbox
    // policy, and a skill that calls services (e.g. `wafer-run/network` via
    // `call_block`) legitimately needs both. The generated `block_info()`
    // already emits `Option<wafer_block::BlockCapabilities>` (as `None`) into
    // every skill crate, so the paths resolve in a skill block's minimal
    // dep graph (`wafer-sdk` re-exports `wafer_block`) — locked by the
    // `skill_block_without_wafer_run_dep` fixture, which declares both.
    // Under SEC-02 fail-closed defaults an undeclared WASM skill gets
    // `none()`, so this is the only way a skill can hold a service grant.

    let flat_ts: Ts2 = if flat_tokens.is_empty() {
        Ts2::new()
    } else {
        flat_tokens
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i + 1 < flat_tokens.len() {
                    quote! { #t , }
                } else {
                    quote! { #t }
                }
            })
            .collect::<Ts2>()
    };

    let flat: FlatArgs = parse2(flat_ts)?;

    Ok(ParsedAttr {
        flat,
        capabilities: caps,
        skill,
    })
}

/// Helper wrapper so we can use syn::parse2 with a Punctuated return.
struct MetaList(Punctuated<syn::Meta, Token![,]>);

impl Parse for MetaList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(MetaList(Punctuated::parse_terminated(input)?))
    }
}

// ---------------------------------------------------------------------------
// #[wafer_async_trait] proc macro
// ---------------------------------------------------------------------------

/// Convenience attribute macro that expands to the platform-appropriate
/// `async_trait` annotation.
///
/// On native targets (`not(target_arch = "wasm32")`) the trait/impl requires
/// `Send` bounds; on `wasm32` (single-threaded) the `?Send` relaxation is
/// used instead. Rather than repeating the two-attribute cfg_attr pair at
/// every site, apply `#[wafer_async_trait]` once:
///
/// ```rust,ignore
/// use wafer_block_macro::wafer_async_trait;
///
/// #[wafer_async_trait]
/// trait MyService {
///     async fn do_work(&self) -> Result<(), Error>;
/// }
///
/// #[wafer_async_trait]
/// impl MyService for MyImpl {
///     async fn do_work(&self) -> Result<(), Error> { Ok(()) }
/// }
/// ```
///
/// This expands to:
/// ```rust,ignore
/// #[cfg_attr(not(target_arch = "wasm32"), ::async_trait::async_trait)]
/// #[cfg_attr(target_arch = "wasm32",     ::async_trait::async_trait(?Send))]
/// trait MyService { ... }
/// ```
#[proc_macro_attribute]
pub fn wafer_async_trait(
    _attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item_ts: proc_macro2::TokenStream = item.into();
    let expanded = quote::quote! {
        #[cfg_attr(not(target_arch = "wasm32"), ::async_trait::async_trait)]
        #[cfg_attr(target_arch = "wasm32", ::async_trait::async_trait(?Send))]
        #item_ts
    };
    expanded.into()
}

// ---------------------------------------------------------------------------
// #[wafer_block] proc macro
// ---------------------------------------------------------------------------

/// Derive a WASM block from an impl block.
///
/// Generates core ABI exports (`__wafer_info`, `__wafer_handle`,
/// `__wafer_lifecycle`) as `#[no_mangle] pub extern "C"` functions that
/// serialize/deserialize via JSON using the wasmi-based ABI convention.
///
/// The generated exports use `wafer_sdk::core_abi::pack_ptr_len` to pack
/// `(ptr, len)` pairs into an `i64` return value. The consuming crate must
/// depend on `wafer-sdk-rs` (package name `wafer-sdk`).
///
/// # Required attributes
/// - `name` — block name, two segments `{org}/{block}` (e.g. `"acme/my-block"`)
/// - `version` — semantic version (e.g. `"0.1.0"`)
/// - `interface` — interface name (e.g. `"transform"`)
/// - `summary` — human-readable description
///
/// # Optional attributes
/// - `instance_mode` — `"per-node"` (default), `"singleton"`, `"per-flow"`, `"per-execution"`
/// - `requires` — list of block names this block may call (e.g. `["wafer-run/database"]`)
/// - `capabilities(...)` — declare block capabilities (see below)
/// - `skill(description = "...", parameters = <json-schema>)` — declare the
///   LLM-facing tool contract (sets `BlockInfo::tool`). Composes with
///   `capabilities(...)`: a skill that calls services (e.g.
///   `wafer-run/network` via `call_block`) declares both.
///
/// # Capabilities
///
/// The `capabilities(...)` group controls what services the WASM block may
/// access. Omitting it leaves `BlockInfo::capabilities` as `None` — under
/// the runtime's fail-closed default an undeclared WASM block gets
/// `BlockCapabilities::none()`, so a WASM block (skill or otherwise) that
/// uses any service MUST declare it here. Providing an empty group sets an
/// explicit zero-capabilities declaration.
///
/// Boolean flags: `crypto`, `network`, `raw_sql`, `ddl`, `schema`, `config`
///
/// `ddl` opens the raw `database.ddl` statement channel; `schema` opens only
/// the structured `database.ensure_table` / `add_column` / `drop_table` ops.
/// They are independent — declare `schema` alone for a block that just needs
/// its own tables.
///
/// List fields: `collections = [...]`, `storage_folders = [...]`,
/// `network_allow = [...]`, `config_keys = [...]`, `vector_indexes = [...]`,
/// `callable_blocks = [...]`
///
/// Header policy: `headers(readable = [...], writable = [...], masked = [...])`
///
/// # Example
///
/// ```rust,ignore
/// use wafer_sdk::*;
///
/// struct MyBlock;
///
/// #[wafer_block(
///     name = "acme/my-block",
///     version = "0.1.0",
///     interface = "http-handler@v1",
///     summary = "My block",
///     capabilities(
///         crypto,
///         network,
///         collections = ["users"],
///         headers(readable = ["authorization"]),
///     ),
/// )]
/// impl MyBlock {
///     fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {
///         GuestResult::respond(vec![])
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn wafer_block(attr: TokenStream, item: TokenStream) -> TokenStream {
    match wafer_block_impl(attr, item) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error().into(),
    }
}

/// Compile-time check that `name` has the structural `{org}/{block}` shape
/// (exactly two non-empty segments separated by a single `/`).
///
/// This catches the common authoring mistake (single-segment or extra-segment
/// names) at the macro site with a spanned error. The full naming ruleset
/// (allowed characters, hyphen placement, etc.) is enforced authoritatively at
/// registration time by `wafer_run::runtime::validate_block_name`; this macro
/// guard is deliberately limited to the segment structure so the two checks
/// cannot drift on the finer rules.
fn validate_block_name_shape(name: &str, span: proc_macro2::Span) -> syn::Result<()> {
    let mut segments = name.split('/');
    let org = segments.next().unwrap_or("");
    let block = segments.next();
    let extra = segments.next();
    if block.is_none() || extra.is_some() || org.is_empty() || block == Some("") {
        return Err(syn::Error::new(
            span,
            format!(
                "#[wafer_block]: `name` must be a two-segment `{{org}}/{{block}}` identifier \
                 (e.g. \"acme/widget\"), got {name:?}"
            ),
        ));
    }
    Ok(())
}

/// Real implementation; `wafer_block` is a thin wrapper that funnels errors
/// through `syn::Error::to_compile_error` so attribute typos surface as
/// spanned compile errors rather than proc-macro panics.
fn wafer_block_impl(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let parsed = parse_attr(attr)?;
    let args = &parsed.flat;
    let capabilities_args = parsed.capabilities;
    let skill_args = parsed.skill;

    let input = syn::parse::<ItemImpl>(item)?;
    let impl_span = input.span();

    // Required attributes
    let name = args
        .get_str("name")
        .ok_or_else(|| syn::Error::new(impl_span, "#[wafer_block]: `name` is required"))?;
    validate_block_name_shape(&name, impl_span)?;
    let version = args
        .get_str("version")
        .ok_or_else(|| syn::Error::new(impl_span, "#[wafer_block]: `version` is required"))?;
    let interface = args
        .get_str("interface")
        .ok_or_else(|| syn::Error::new(impl_span, "#[wafer_block]: `interface` is required"))?;
    let summary = args
        .get_str("summary")
        .ok_or_else(|| syn::Error::new(impl_span, "#[wafer_block]: `summary` is required"))?;

    // Optional attributes
    let instance_mode_str = args
        .get_str("instance_mode")
        .unwrap_or_else(|| "per-node".to_string());
    let requires = args.get_str_list("requires")?;

    let struct_ty = &input.self_ty;

    // Partition methods into handle, lifecycle, and other
    let mut handle_fn: Option<ImplItemFn> = None;
    let mut lifecycle_fn: Option<ImplItemFn> = None;
    let mut other_items: Vec<ImplItem> = Vec::new();

    for item in input.items {
        match item {
            ImplItem::Fn(f) if f.sig.ident == "handle" => {
                handle_fn = Some(f);
            }
            ImplItem::Fn(f) if f.sig.ident == "lifecycle" => {
                lifecycle_fn = Some(f);
            }
            other => other_items.push(other),
        }
    }

    let handle_fn = handle_fn
        .ok_or_else(|| syn::Error::new(impl_span, "#[wafer_block]: `handle` method is required"))?;

    let instance_mode_tokens = match instance_mode_str.as_str() {
        "per-node" => quote! { wafer_block::InstanceMode::PerNode },
        "singleton" => quote! { wafer_block::InstanceMode::Singleton },
        "per-flow" => quote! { wafer_block::InstanceMode::PerFlow },
        "per-execution" => quote! { wafer_block::InstanceMode::PerExecution },
        other => {
            return Err(syn::Error::new(
                impl_span,
                format!("#[wafer_block]: unknown instance_mode '{other}'"),
            ));
        }
    };

    let handle_sig = &handle_fn.sig;
    let handle_block = &handle_fn.block;
    let handle_attrs = &handle_fn.attrs;

    let lifecycle_impl = match &lifecycle_fn {
        Some(lf) => {
            let sig = &lf.sig;
            let block = &lf.block;
            let attrs = &lf.attrs;
            quote! {
                #(#attrs)*
                #sig #block
            }
        }
        None => quote! {
            fn lifecycle(_event: wafer_block::LifecycleEvent) -> ::std::result::Result<(), wafer_block::WaferError> {
                Ok(())
            }
        },
    };

    let other_impl = if other_items.is_empty() {
        quote! {}
    } else {
        quote! {
            impl #struct_ty {
                #(#other_items)*
            }
        }
    };

    // Build the capabilities expression for `__wafer_info`.
    let capabilities_expr = if let Some(c) = &capabilities_args {
        let crypto = c.crypto;
        let raw_sql = c.raw_sql;
        let ddl = c.ddl;
        let schema = c.schema;
        let collections_expr = wildcard_list_tokens(&c.collections);
        let storage_folders_expr = wildcard_list_tokens(&c.storage_folders);
        let vector_indexes_expr = wildcard_list_tokens(&c.vector_indexes);
        let callable_blocks_expr = wildcard_list_tokens(&c.callable_blocks);
        let readable = &c.headers_readable;
        let writable = &c.headers_writable;
        let masked = &c.headers_masked;
        // SEC-06: map the (flag, allow-list) DSL pair onto the `Allowlist`
        // enum, preserving the pre-enum semantics exactly:
        //   flag absent            → None  (disabled — deny all)
        //   flag present, no list  → Any   (enabled — allow all)
        //   flag present, w/ list  → Only  (enabled — restricted)
        // The `capabilities(network, network_allow(...))` DSL is unchanged;
        // only the emitted representation moves to the enum.
        let network_expr = allowlist_tokens(c.network, &c.network_allow);
        let config_expr = allowlist_tokens(c.config, &c.config_keys);
        quote! {
            Some(wafer_block::BlockCapabilities {
                crypto: #crypto,
                network: #network_expr,
                raw_sql: #raw_sql,
                ddl: #ddl,
                schema: #schema,
                config: #config_expr,
                collections: #collections_expr,
                storage_folders: #storage_folders_expr,
                vector_indexes: #vector_indexes_expr,
                callable_blocks: #callable_blocks_expr,
                headers: wafer_block::capabilities::HeaderPolicy {
                    readable: vec![#(#readable.to_string()),*],
                    writable: vec![#(#writable.to_string()),*],
                    masked: vec![#(#masked.to_string()),*],
                },
            })
        }
    } else {
        quote! { None::<wafer_block::BlockCapabilities> }
    };

    // Build the optional `requires` expression for `block_info()`. The
    // `requires = [...]` attribute declares which other blocks this block may
    // `call_block`; the runtime enforces it as an access-control gate (an empty
    // list means "unrestricted"), so a declared-but-unwired list would silently
    // disable the restriction.
    let requires_expr = if requires.is_empty() {
        quote! {}
    } else {
        quote! {
            info = info.requires(vec![ #( #requires.to_string() ),* ]);
        }
    };

    // Build the optional SkillTool expression for `block_info()`.
    let skill_tool_expr = if let Some(skill) = &skill_args {
        let description = &skill.description;
        let parameters_expr = &skill.parameters;
        quote! {
            info = info
                .tool(wafer_block::types::SkillTool {
                    description: #description.to_string(),
                    // `parameters` is either a string literal that `parse_skill`
                    // already validated as JSON, or an author-supplied expression
                    // evaluating to `&str`/`String`. Coerce to `&str` and parse;
                    // for the literal form this is infallible, for the expression
                    // form a malformed schema panics here (covered by a test).
                    parameters: serde_json::from_str(
                        ::core::convert::AsRef::<str>::as_ref(&(#parameters_expr))
                    )
                    .expect(concat!("skill parameters JSON parse error in block ", #name)),
                });
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #other_impl

        impl #struct_ty {
            #(#handle_attrs)*
            #handle_sig #handle_block

            #lifecycle_impl

            /// Returns the block's metadata. Generated by `#[wafer_block]`.
            /// Used in tests and by the native runtime adapter.
            pub fn block_info() -> wafer_block::BlockInfo {
                let mut info = wafer_block::BlockInfo::new(#name, #version, #interface, #summary)
                    .instance_mode(#instance_mode_tokens);
                let caps: Option<wafer_block::BlockCapabilities> = #capabilities_expr;
                if let Some(c) = caps {
                    info = info.capabilities(c);
                }
                #requires_expr
                #skill_tool_expr
                info
            }
        }

        /// Core-ABI version this guest speaks (v2 = MessagePack frames; see
        /// `wafer_block::abi`). The host probes this export; guests built
        /// before it existed are decoded as v1 (JSON).
        #[cfg(all(not(test), target_arch = "wasm32"))]
        #[no_mangle]
        pub extern "C" fn __wafer_abi_version() -> i32 {
            wafer_block::abi::ABI_VERSION
        }

        #[cfg(all(not(test), target_arch = "wasm32"))]
        #[no_mangle]
        pub extern "C" fn __wafer_info() -> i64 {
            // FFI boundary: serialization failures cannot panic across the
            // WASM trap-vs-trap boundary cleanly, so on failure we return
            // a zero (ptr, len) packet. The host decodes that as "no info
            // available" rather than receiving a wasm trap.
            let info = <#struct_ty>::block_info();
            let bytes = match wafer_block::codec::encode(&info) {
                Ok(b) => b,
                Err(_) => return wafer_sdk::core_abi::pack_ptr_len(0, 0),
            };
            let ptr = bytes.as_ptr() as u32;
            let len = bytes.len() as u32;
            ::std::mem::forget(bytes);
            wafer_sdk::core_abi::pack_ptr_len(ptr, len)
        }

        #[cfg(all(not(test), target_arch = "wasm32"))]
        #[no_mangle]
        pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
            // SAFETY: the wasmi host ABI passes (msg_ptr, msg_len) pointing at a
            // valid, initialized, properly-aligned (u8 has alignment 1) region
            // of this guest's linear memory. The guest is single-threaded and
            // does not mutate that region for the duration of this call, so the
            // borrow is exclusive; the slice is read-only and dropped before we
            // return control to the host.
            let msg_bytes = unsafe {
                ::std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize)
            };
            // ABI v2 frame: MessagePack [Message, bin body] (see wafer_block::abi).
            let frame: wafer_block::abi::CallFrame =
                match wafer_block::codec::decode(msg_bytes) {
                    Ok(parsed) => parsed,
                    Err(_) => return wafer_sdk::core_abi::pack_ptr_len(0, 0),
                };
            let (msg, body) = (frame.0, frame.1.into_vec());
            let result: wafer_sdk::core_abi::GuestResult = <#struct_ty>::handle(msg, body);
            let result_bytes = match wafer_block::codec::encode(&result) {
                Ok(b) => b,
                Err(_) => return wafer_sdk::core_abi::pack_ptr_len(0, 0),
            };
            let ptr = result_bytes.as_ptr() as u32;
            let len = result_bytes.len() as u32;
            ::std::mem::forget(result_bytes);
            wafer_sdk::core_abi::pack_ptr_len(ptr, len)
        }

        #[cfg(all(not(test), target_arch = "wasm32"))]
        #[no_mangle]
        pub extern "C" fn __wafer_lifecycle(evt_ptr: i32, evt_len: i32) -> i64 {
            // SAFETY: the wasmi host ABI passes (evt_ptr, evt_len) pointing at a
            // valid, initialized, properly-aligned (u8 has alignment 1) region
            // of this guest's linear memory. The guest is single-threaded and
            // does not mutate that region for the duration of this call, so the
            // borrow is exclusive; the slice is read-only and dropped before we
            // return control to the host.
            let evt_bytes = unsafe {
                ::std::slice::from_raw_parts(evt_ptr as *const u8, evt_len as usize)
            };
            let event: wafer_block::LifecycleEvent =
                match wafer_block::codec::decode(evt_bytes) {
                    Ok(e) => e,
                    Err(_) => return wafer_sdk::core_abi::pack_ptr_len(0, 0),
                };
            let result = <#struct_ty>::lifecycle(event);
            let result_bytes = match wafer_block::codec::encode(&result) {
                Ok(b) => b,
                Err(_) => return wafer_sdk::core_abi::pack_ptr_len(0, 0),
            };
            let ptr = result_bytes.as_ptr() as u32;
            let len = result_bytes.len() as u32;
            ::std::mem::forget(result_bytes);
            wafer_sdk::core_abi::pack_ptr_len(ptr, len)
        }

        // The link-time block registry uses `linkme`, which is only used on
        // native targets. On `wasm32-*` the registry doesn't exist, and skill
        // blocks compiled for wasm32-wasip1 don't (and shouldn't) depend on
        // `wafer-run`. Cfg-gate the emission so wasm32 expansions never
        // reference `::wafer_block::...`, which would fail path resolution at
        // macro-expansion time when the consumer crate has no `wafer-block`
        // dep in its graph.
        #[cfg(not(target_arch = "wasm32"))]
        ::wafer_block::register_static_block!(#name, #struct_ty);
    };

    Ok(expanded.into())
}

#[cfg(test)]
mod tests {
    use super::parse_skill;

    /// Build a `syn::MetaList` for a `skill(...)` attribute from source text.
    fn skill_meta(src: &str) -> syn::MetaList {
        syn::parse_str::<syn::MetaList>(src).expect("test input must parse as a MetaList")
    }

    #[test]
    fn parse_skill_accepts_valid_json_parameters() {
        let meta = skill_meta(
            r#"skill(description = "does a thing", parameters = "{\"type\":\"object\",\"properties\":{}}")"#,
        );
        let args = parse_skill(&meta).expect("valid JSON parameters should parse");
        assert_eq!(args.description, "does a thing");
        // The literal form stores the validated literal as tokens; recover it.
        let lit: syn::LitStr =
            syn::parse2(args.parameters).expect("literal form yields a string-literal token");
        assert_eq!(lit.value(), r#"{"type":"object","properties":{}}"#);
    }

    #[test]
    fn parse_skill_rejects_malformed_json_parameters() {
        // Missing closing brace — well-formed string literal, malformed JSON.
        let meta = skill_meta(r#"skill(description = "d", parameters = "{\"type\":")"#);
        let err =
            parse_skill(&meta).expect_err("malformed JSON parameters must be a macro-site error");
        assert!(
            err.to_string().contains("not valid JSON"),
            "error should explain the JSON validation failure, got: {err}"
        );
    }

    #[test]
    fn parse_skill_accepts_non_object_json_scalar() {
        // Any well-formed JSON value parses; the macro only checks well-formedness,
        // not that the schema is a JSON object.
        let meta = skill_meta(r#"skill(description = "d", parameters = "42")"#);
        let args = parse_skill(&meta).expect("scalar JSON is still valid JSON");
        let lit: syn::LitStr =
            syn::parse2(args.parameters).expect("literal form yields a string-literal token");
        assert_eq!(lit.value(), "42");
    }

    #[test]
    fn parse_skill_accepts_expression_parameters_without_json_check() {
        // Expression form: `parameters` is not a string literal, so its JSON is
        // NOT validated at macro-expansion time (validity is enforced at runtime
        // in `block_info()`). The author's tokens are stored verbatim — even an
        // expression that could never be a JSON literal is accepted at parse.
        let meta = skill_meta(r#"skill(description = "d", parameters = my_core::schema_json())"#);
        let args = parse_skill(&meta).expect("expression parameters must be accepted");
        assert!(
            args.parameters.to_string().contains("schema_json"),
            "expression tokens stored verbatim, got: {}",
            args.parameters
        );
    }
}
