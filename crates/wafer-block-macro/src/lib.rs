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
/// )]
/// impl MyBlock {
///     fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {
///         GuestResult::respond(vec![])
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

    let expanded = quote! {
        #other_impl

        impl #struct_ty {
            #(#handle_attrs)*
            #handle_sig #handle_block

            #lifecycle_impl
        }

        #[no_mangle]
        pub extern "C" fn __wafer_info() -> i64 {
            let info = wafer_block::BlockInfo::new(#name, #version, #interface, #summary)
                .instance_mode(#instance_mode_tokens);
            let bytes = serde_json::to_vec(&info).expect("failed to serialize BlockInfo");
            let ptr = bytes.as_ptr() as u32;
            let len = bytes.len() as u32;
            ::std::mem::forget(bytes);
            wafer_sdk::core_abi::pack_ptr_len(ptr, len)
        }

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
    };

    expanded.into()
}
