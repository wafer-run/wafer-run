use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
    Expr, ExprArray, ExprLit, Ident, ImplItem, ImplItemFn, ItemImpl, Lit, Token,
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
    config: bool,
    collections: Vec<String>,
    storage_folders: Vec<String>,
    network_allow: Vec<String>,
    config_keys: Vec<String>,
    callable_blocks: Vec<String>,
    headers_readable: Vec<String>,
    headers_writable: Vec<String>,
    headers_masked: Vec<String>,
}

fn parse_capabilities(meta: &syn::MetaList) -> CapabilitiesArgs {
    let mut out = CapabilitiesArgs::default();
    let nested: Punctuated<syn::Meta, Token![,]> = meta
        .parse_args_with(Punctuated::parse_terminated)
        .expect("#[wafer_block]: failed to parse capabilities(...)");
    for item in nested {
        match item {
            syn::Meta::Path(p) => {
                let ident = p
                    .get_ident()
                    .expect("expected identifier in capabilities(...)")
                    .to_string();
                match ident.as_str() {
                    "crypto" => out.crypto = true,
                    "network" => out.network = true,
                    "raw_sql" => out.raw_sql = true,
                    "ddl" => out.ddl = true,
                    "config" => out.config = true,
                    other => panic!("#[wafer_block]: unknown bool capability '{other}'"),
                }
            }
            syn::Meta::NameValue(nv) => {
                let ident = nv
                    .path
                    .get_ident()
                    .expect("expected identifier in capabilities(...)")
                    .to_string();
                let list = parse_string_list(&nv.value);
                match ident.as_str() {
                    "collections" => out.collections = list,
                    "storage_folders" => out.storage_folders = list,
                    "network_allow" => out.network_allow = list,
                    "config_keys" => out.config_keys = list,
                    "callable_blocks" => out.callable_blocks = list,
                    other => panic!("#[wafer_block]: unknown list capability '{other}'"),
                }
            }
            syn::Meta::List(inner) if inner.path.is_ident("headers") => {
                parse_headers_nested(&inner, &mut out);
            }
            other => {
                use quote::ToTokens;
                panic!(
                    "#[wafer_block]: unexpected token in capabilities(...): {}",
                    other.to_token_stream()
                );
            }
        }
    }
    out
}

fn parse_headers_nested(inner: &syn::MetaList, out: &mut CapabilitiesArgs) {
    let nested: Punctuated<syn::Meta, Token![,]> = inner
        .parse_args_with(Punctuated::parse_terminated)
        .expect("#[wafer_block]: failed to parse headers(...)");
    for item in nested {
        if let syn::Meta::NameValue(nv) = item {
            let ident = nv
                .path
                .get_ident()
                .expect("expected ident in headers(...)")
                .to_string();
            let list = parse_string_list(&nv.value);
            match ident.as_str() {
                "readable" => out.headers_readable = list,
                "writable" => out.headers_writable = list,
                "masked" => out.headers_masked = list,
                other => panic!("#[wafer_block]: unknown headers field '{other}'"),
            }
        } else {
            panic!("#[wafer_block]: headers(...) only supports name = [...] pairs");
        }
    }
}

fn parse_string_list(expr: &syn::Expr) -> Vec<String> {
    if let syn::Expr::Array(arr) = expr {
        arr.elems
            .iter()
            .map(|e| match e {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => s.value(),
                other => {
                    use quote::ToTokens;
                    panic!(
                        "#[wafer_block]: expected string literal, got: {}",
                        other.to_token_stream()
                    );
                }
            })
            .collect()
    } else {
        use quote::ToTokens;
        panic!(
            "#[wafer_block]: expected array expression `[...]`, got: {}",
            expr.to_token_stream()
        );
    }
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

    fn get_str_list(&self, key: &str) -> Vec<String> {
        self.pairs
            .iter()
            .find(|kv| kv.key == key)
            .map(|kv| {
                if let Expr::Array(ExprArray { elems, .. }) = &kv.value {
                    elems
                        .iter()
                        .filter_map(|e| {
                            if let Expr::Lit(ExprLit {
                                lit: Lit::Str(s), ..
                            }) = e
                            {
                                Some(s.value())
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    vec![]
                }
            })
            .unwrap_or_default()
    }
}

/// Parsed skill(...) attribute — description and JSON parameters schema for
/// the LLM-facing tool definition.
struct SkillArgs {
    description: String,
    parameters: String,
}

/// Parse a `skill(description = "...", parameters = "...")` MetaList.
fn parse_skill(meta: &syn::MetaList) -> SkillArgs {
    let nested: Punctuated<syn::Meta, Token![,]> = meta
        .parse_args_with(Punctuated::parse_terminated)
        .expect("#[wafer_block]: failed to parse skill(...)");
    let mut description = None::<String>;
    let mut parameters = None::<String>;
    for item in nested {
        match item {
            syn::Meta::NameValue(nv) => {
                let key = nv
                    .path
                    .get_ident()
                    .expect("expected identifier in skill(...)")
                    .to_string();
                let val = if let Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) = &nv.value
                {
                    s.value()
                } else {
                    use quote::ToTokens;
                    panic!(
                        "#[wafer_block]: skill({key}) value must be a string literal, got: {}",
                        nv.value.to_token_stream()
                    )
                };
                match key.as_str() {
                    "description" => description = Some(val),
                    "parameters" => parameters = Some(val),
                    other => panic!("#[wafer_block]: unknown skill attribute '{other}'"),
                }
            }
            other => {
                use quote::ToTokens;
                panic!(
                    "#[wafer_block]: unexpected token in skill(...): {}",
                    other.to_token_stream()
                );
            }
        }
    }
    SkillArgs {
        description: description
            .expect("#[wafer_block]: skill(...) requires description = \"...\""),
        parameters: parameters.expect("#[wafer_block]: skill(...) requires parameters = \"...\""),
    }
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
fn parse_attr(attr: TokenStream) -> ParsedAttr {
    // We perform a two-step parse:
    //   1. Convert to a proc_macro2 TokenStream and scan Meta items.
    //   2. Re-collect the non-capabilities/non-skill items for flat parsing.

    use proc_macro2::TokenStream as Ts2;
    use quote::ToTokens;

    let ts2: Ts2 = attr.into();
    let metas: Punctuated<syn::Meta, Token![,]> = syn::parse2::<MetaList>(ts2)
        .map(|m| m.0)
        .unwrap_or_default();

    let mut caps: Option<CapabilitiesArgs> = None;
    let mut skill: Option<SkillArgs> = None;
    let mut flat_tokens: Vec<proc_macro2::TokenStream> = Vec::new();

    for meta in &metas {
        match meta {
            syn::Meta::List(ml) if ml.path.is_ident("capabilities") => {
                caps = Some(parse_capabilities(ml));
            }
            syn::Meta::List(ml) if ml.path.is_ident("skill") => {
                skill = Some(parse_skill(ml));
            }
            other => {
                flat_tokens.push(other.to_token_stream());
            }
        }
    }

    // Reassemble flat tokens into a comma-separated TokenStream for FlatArgs.
    let flat_ts: Ts2 = if flat_tokens.is_empty() {
        Ts2::new()
    } else {
        let joined = flat_tokens
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i + 1 < flat_tokens.len() {
                    quote! { #t , }
                } else {
                    quote! { #t }
                }
            })
            .collect::<Ts2>();
        joined
    };

    let flat: FlatArgs = syn::parse2(flat_ts)
        .unwrap_or_else(|e| panic!("#[wafer_block]: failed to parse flat attributes: {e}"));

    ParsedAttr {
        flat,
        capabilities: caps,
        skill,
    }
}

/// Helper wrapper so we can use syn::parse2 with a Punctuated return.
struct MetaList(Punctuated<syn::Meta, Token![,]>);

impl Parse for MetaList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(MetaList(Punctuated::parse_terminated(input)?))
    }
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
/// - `name` — block name (e.g. `"my-block"`)
/// - `version` — semantic version (e.g. `"0.1.0"`)
/// - `interface` — interface name (e.g. `"transform"`)
/// - `summary` — human-readable description
///
/// # Optional attributes
/// - `instance_mode` — `"per-node"` (default), `"singleton"`, `"per-flow"`, `"per-execution"`
/// - `requires` — list of block names this block may call (e.g. `["wafer-run/database"]`)
/// - `capabilities(...)` — declare block capabilities (see below)
///
/// # Capabilities
///
/// The `capabilities(...)` group controls what services the WASM block may
/// access. Omitting it leaves `BlockInfo::capabilities` as `None` (runtime
/// applies its default). Providing an empty group sets an explicit
/// zero-capabilities declaration.
///
/// Boolean flags: `crypto`, `network`, `raw_sql`, `ddl`, `config`
///
/// List fields: `collections = [...]`, `storage_folders = [...]`,
/// `network_allow = [...]`, `config_keys = [...]`, `callable_blocks = [...]`
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
///     name = "my-block",
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
    let parsed = parse_attr(attr);
    let args = &parsed.flat;
    let capabilities_args = parsed.capabilities;
    let skill_args = parsed.skill;

    let input = parse_macro_input!(item as ItemImpl);

    // Required attributes
    let name = args
        .get_str("name")
        .expect("#[wafer_block]: `name` is required");
    let version = args
        .get_str("version")
        .expect("#[wafer_block]: `version` is required");
    let interface = args
        .get_str("interface")
        .expect("#[wafer_block]: `interface` is required");
    let summary = args
        .get_str("summary")
        .expect("#[wafer_block]: `summary` is required");

    // Optional attributes
    let instance_mode_str = args
        .get_str("instance_mode")
        .unwrap_or_else(|| "per-node".to_string());
    let _requires = args.get_str_list("requires");

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

    let handle_fn = handle_fn.expect("#[wafer_block]: `handle` method is required");

    let instance_mode_tokens = match instance_mode_str.as_str() {
        "per-node" => quote! { wafer_block::InstanceMode::PerNode },
        "singleton" => quote! { wafer_block::InstanceMode::Singleton },
        "per-flow" => quote! { wafer_block::InstanceMode::PerFlow },
        "per-execution" => quote! { wafer_block::InstanceMode::PerExecution },
        other => panic!("#[wafer_block]: unknown instance_mode '{other}'"),
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
        let network = c.network;
        let raw_sql = c.raw_sql;
        let ddl = c.ddl;
        let config_cap = c.config;
        let collections = &c.collections;
        let storage_folders = &c.storage_folders;
        let network_allow = &c.network_allow;
        let config_keys = &c.config_keys;
        let callable_blocks = &c.callable_blocks;
        let readable = &c.headers_readable;
        let writable = &c.headers_writable;
        let masked = &c.headers_masked;
        quote! {
            Some(wafer_block::BlockCapabilities {
                crypto: #crypto,
                network: #network,
                raw_sql: #raw_sql,
                ddl: #ddl,
                config: #config_cap,
                collections: {
                    let mut s = ::std::collections::HashSet::new();
                    #(s.insert(#collections.to_string());)*
                    s
                },
                storage_folders: {
                    let mut s = ::std::collections::HashSet::new();
                    #(s.insert(#storage_folders.to_string());)*
                    s
                },
                config_keys: {
                    let mut s = ::std::collections::HashSet::new();
                    #(s.insert(#config_keys.to_string());)*
                    s
                },
                callable_blocks: {
                    let mut s = ::std::collections::HashSet::new();
                    #(s.insert(#callable_blocks.to_string());)*
                    s
                },
                network_allow: vec![#(#network_allow.to_string()),*],
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

    // Build the optional SkillTool expression for `block_info()`.
    let skill_tool_expr = if let Some(skill) = &skill_args {
        let description = &skill.description;
        let parameters_json = &skill.parameters;
        quote! {
            info = info
                .role(wafer_block::types::SkillRole::Skill)
                .tool(wafer_block::types::SkillTool {
                    description: #description.to_string(),
                    parameters: serde_json::from_str(#parameters_json)
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
                #skill_tool_expr
                info
            }
        }

        #[cfg(all(not(test), target_arch = "wasm32"))]
        #[no_mangle]
        pub extern "C" fn __wafer_info() -> i64 {
            let info = <#struct_ty>::block_info();
            let bytes = serde_json::to_vec(&info).expect("failed to serialize BlockInfo");
            let ptr = bytes.as_ptr() as u32;
            let len = bytes.len() as u32;
            ::std::mem::forget(bytes);
            wafer_sdk::core_abi::pack_ptr_len(ptr, len)
        }

        #[cfg(all(not(test), target_arch = "wasm32"))]
        #[no_mangle]
        pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
            let msg_bytes = unsafe {
                ::std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize)
            };
            // The host sends a 2-element JSON tuple: [Message, Vec<u8>]
            let (msg, body): (wafer_block::Message, ::std::vec::Vec<u8>) =
                serde_json::from_slice(msg_bytes)
                    .expect("failed to deserialize (Message, body) tuple");
            let result: wafer_sdk::core_abi::GuestResult = <#struct_ty>::handle(msg, body);
            let result_bytes = serde_json::to_vec(&result).expect("failed to serialize GuestResult");
            let ptr = result_bytes.as_ptr() as u32;
            let len = result_bytes.len() as u32;
            ::std::mem::forget(result_bytes);
            wafer_sdk::core_abi::pack_ptr_len(ptr, len)
        }

        #[cfg(all(not(test), target_arch = "wasm32"))]
        #[no_mangle]
        pub extern "C" fn __wafer_lifecycle(evt_ptr: i32, evt_len: i32) -> i64 {
            let evt_bytes = unsafe {
                ::std::slice::from_raw_parts(evt_ptr as *const u8, evt_len as usize)
            };
            let event: wafer_block::LifecycleEvent = serde_json::from_slice(evt_bytes)
                .expect("failed to deserialize LifecycleEvent");
            let result = <#struct_ty>::lifecycle(event);
            let result_bytes = serde_json::to_vec(&result).expect("failed to serialize lifecycle result");
            let ptr = result_bytes.as_ptr() as u32;
            let len = result_bytes.len() as u32;
            ::std::mem::forget(result_bytes);
            wafer_sdk::core_abi::pack_ptr_len(ptr, len)
        }

        // The link-time block registry uses `linkme`, which is only used on
        // native targets. On `wasm32-*` the registry doesn't exist, and skill
        // blocks compiled for wasm32-wasip1 don't (and shouldn't) depend on
        // `wafer-run`. Cfg-gate the emission so wasm32 expansions never
        // reference `::wafer_run::...`, which would fail path resolution at
        // macro-expansion time when the consumer crate has no `wafer-run`
        // dep in its graph.
        #[cfg(not(target_arch = "wasm32"))]
        ::wafer_run::register_static_block!(#name, #struct_ty);
    };

    expanded.into()
}
