use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, Expr, ExprArray, ExprLit, Ident, ImplItem, ImplItemFn, ItemImpl, Lit, Token};

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

struct Args {
    pairs: Punctuated<KeyValue, Token![,]>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Args {
            pairs: Punctuated::parse_terminated(input)?,
        })
    }
}

impl Args {
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

// ---------------------------------------------------------------------------
// #[wafer_block] proc macro
// ---------------------------------------------------------------------------

/// Derive a WASM block from an impl block.
///
/// Generates the WIT `Guest` trait implementation (with `info()` from the
/// attribute arguments) and the WASM Component Model exports.
///
/// # Required attributes
/// - `name` — block name (e.g. `"my-block"`)
/// - `version` — semantic version (e.g. `"0.1.0"`)
/// - `interface` — interface name (e.g. `"transform"`)
/// - `summary` — human-readable description
///
/// # Optional attributes
/// - `instance_mode` — `"per-node"` (default), `"singleton"`, `"per-flow"`, `"per-execution"`
/// - `requires` — list of block names this block may call (e.g. `["@wafer/database"]`)
///
/// # Example
///
/// ```rust,ignore
/// use wafer_block::*;
///
/// struct MyBlock;
///
/// #[wafer_block(
///     name = "my-block",
///     version = "0.1.0",
///     interface = "transform",
///     summary = "Transforms messages"
/// )]
/// impl MyBlock {
///     fn handle(msg: Message) -> BlockResult {
///         msg.cont()
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn wafer_block(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
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
        other => panic!("#[wafer_block]: unknown instance_mode '{}'", other),
    };

    let handle_sig = &handle_fn.sig;
    let handle_block = &handle_fn.block;
    let handle_attrs = &handle_fn.attrs;

    let lifecycle_impl = match lifecycle_fn {
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

    // Generate the WIT Guest trait impl and Component Model exports.
    let expanded = quote! {
        #other_impl

        impl wafer_block::Guest for #struct_ty {
            fn info() -> wafer_block::wafer::block_world::types::BlockInfo {
                wafer_block::wafer::block_world::types::BlockInfo {
                    name: #name.to_string(),
                    version: #version.to_string(),
                    interface: #interface.to_string(),
                    summary: #summary.to_string(),
                    instance_mode: #instance_mode_tokens,
                    allowed_modes: ::std::vec::Vec::new(),
                }
            }

            #(#handle_attrs)*
            #handle_sig #handle_block

            #lifecycle_impl
        }

        wafer_block::export_wafer_block!(#struct_ty);
    };

    expanded.into()
}
