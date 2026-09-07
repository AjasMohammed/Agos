use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse::Parser, parse_macro_input, ItemFn, LitStr, Meta, Token};

struct ToolAttrs {
    name: String,
    version: String,
    description: String,
    permissions: Vec<String>,
    /// Optional input type used to auto-derive a JSON Schema for the tool's
    /// payload. When provided, the generated struct exposes a
    /// `payload_schema()` constructor that returns the schema as
    /// `serde_json::Value`, suitable for embedding in the tool's manifest or
    /// in a registry registration call.
    input_type: Option<syn::Path>,
}

impl ToolAttrs {
    fn parse(input: proc_macro2::TokenStream) -> syn::Result<Self> {
        let mut name = None;
        let mut version = None;
        let mut description = None;
        let mut permissions = Vec::new();
        let mut input_type: Option<syn::Path> = None;

        let parser = syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated;
        let metas = parser.parse2(input)?;

        for meta in metas {
            match &meta {
                Meta::NameValue(nv) => {
                    let key = nv
                        .path
                        .get_ident()
                        .ok_or_else(|| syn::Error::new_spanned(&nv.path, "expected identifier"))?
                        .to_string();

                    match key.as_str() {
                        "name" => {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(s),
                                ..
                            }) = &nv.value
                            {
                                name = Some(s.value());
                            }
                        }
                        "version" => {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(s),
                                ..
                            }) = &nv.value
                            {
                                version = Some(s.value());
                            }
                        }
                        "description" => {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(s),
                                ..
                            }) = &nv.value
                            {
                                description = Some(s.value());
                            }
                        }
                        "permissions" => {
                            // permissions = "fs.read:r, network.outbound:x"
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(s),
                                ..
                            }) = &nv.value
                            {
                                permissions = s
                                    .value()
                                    .split(',')
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty())
                                    .collect();
                            }
                        }
                        "input" => {
                            // input = MyInputType — used by schemars to derive a JSON Schema.
                            // Accept either a bare path (`input = MyInput`) or a string literal
                            // (`input = "crate::path::MyInput"`) for ergonomics.
                            match &nv.value {
                                syn::Expr::Path(p) => {
                                    input_type = Some(p.path.clone());
                                }
                                syn::Expr::Lit(syn::ExprLit {
                                    lit: syn::Lit::Str(s),
                                    ..
                                }) => {
                                    let parsed: syn::Path = syn::parse_str(&s.value())
                                        .map_err(|e| syn::Error::new_spanned(s, e.to_string()))?;
                                    input_type = Some(parsed);
                                }
                                other => {
                                    return Err(syn::Error::new_spanned(
                                        other,
                                        "expected a type path or string literal for `input`",
                                    ));
                                }
                            }
                        }
                        other => {
                            return Err(syn::Error::new_spanned(
                                &nv.path,
                                format!("unknown attribute: {}", other),
                            ));
                        }
                    }
                }
                other => {
                    return Err(syn::Error::new_spanned(other, "expected key = \"value\""));
                }
            }
        }

        Ok(ToolAttrs {
            name: name
                .ok_or_else(|| syn::Error::new(proc_macro2::Span::call_site(), "missing `name`"))?,
            version: version.unwrap_or_else(|| "0.1.0".to_string()),
            description: description.unwrap_or_default(),
            permissions,
            input_type,
        })
    }
}

/// Convert a kebab-case or snake_case name to PascalCase for the struct name.
fn to_pascal_case(s: &str) -> String {
    s.split(['-', '_'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

/// Parse a permission string like "fs.data:rwq" into one or more (resource, PermissionOp) pairs.
///
/// Supported single-char flags: r=Read, w=Write, x=Execute, q=Query, o=Observe.
/// Named ops "query" and "observe" are also accepted.
/// Compound ops like "rw", "rwq", etc. expand to multiple entries.
/// Returns `Err` with a diagnostic for unknown op suffixes (e.g. `"fs.data:z"`).
fn parse_permission(perm: &str) -> Result<Vec<(String, proc_macro2::TokenStream)>, String> {
    let parts: Vec<&str> = perm.splitn(2, ':').collect();
    let resource = parts[0].to_string();
    let op_str = if parts.len() > 1 { parts[1] } else { "r" };

    // Handle named ops first
    match op_str {
        "query" => {
            return Ok(vec![(
                resource,
                quote! { agentos_types::PermissionOp::Query },
            )])
        }
        "observe" => {
            return Ok(vec![(
                resource,
                quote! { agentos_types::PermissionOp::Observe },
            )])
        }
        _ => {}
    }

    // Character-based parsing: each char maps to a PermissionOp.
    // Duplicates are tracked via a HashSet<char> to avoid fragile TokenStream comparison.
    let mut seen = std::collections::HashSet::<char>::new();
    let mut ops: Vec<proc_macro2::TokenStream> = Vec::new();
    for ch in op_str.chars() {
        if !seen.insert(ch) {
            continue; // skip duplicate
        }
        let op = match ch {
            'r' => quote! { agentos_types::PermissionOp::Read },
            'w' => quote! { agentos_types::PermissionOp::Write },
            'x' => quote! { agentos_types::PermissionOp::Execute },
            'q' => quote! { agentos_types::PermissionOp::Query },
            'o' => quote! { agentos_types::PermissionOp::Observe },
            other => {
                return Err(format!(
                    "unknown permission flag '{}' in \"{}\"; expected r, w, x, q, o",
                    other, perm
                ))
            }
        };
        ops.push(op);
    }

    if ops.is_empty() {
        return Err(format!(
            "empty permission op in \"{}\"; expected r, w, x, q, or o",
            perm
        ));
    }

    let entries = ops
        .into_iter()
        .map(|op| (resource.clone(), op))
        .collect::<Vec<_>>();
    Ok(entries)
}

/// Attribute macro that generates an `AgentTool` implementation from an async function.
///
/// # Example
///
/// ```ignore
/// #[tool(
///     name = "web-search",
///     version = "1.0.0",
///     description = "Search the web for information",
///     permissions = "network.outbound:x"
/// )]
/// async fn web_search(
///     payload: serde_json::Value,
///     context: ToolExecutionContext,
/// ) -> Result<serde_json::Value, AgentOSError> {
///     // ... implementation ...
/// }
/// ```
///
/// This generates a `WebSearch` struct that implements `AgentTool`.
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = match ToolAttrs::parse(attr.into()) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let func = parse_macro_input!(item as ItemFn);

    let tool_name = &attrs.name;
    let tool_version = &attrs.version;
    let tool_description = &attrs.description;
    let func_name = &func.sig.ident;

    let struct_name_str = to_pascal_case(tool_name);
    let struct_name = format_ident!("{}", struct_name_str);

    // Parse permissions — compound ops like "rw" expand to multiple entries.
    // Unknown ops produce a compile error rather than silently defaulting to Read.
    let parsed_perms: Result<Vec<Vec<_>>, String> = attrs
        .permissions
        .iter()
        .map(|p| parse_permission(p))
        .collect();
    let perm_entries: Vec<_> = match parsed_perms {
        Ok(nested) => nested
            .into_iter()
            .flatten()
            .map(|(resource, op)| {
                let resource_lit = LitStr::new(&resource, proc_macro2::Span::call_site());
                quote! { (#resource_lit.to_string(), #op) }
            })
            .collect(),
        Err(msg) => {
            return syn::Error::new(proc_macro2::Span::call_site(), msg)
                .to_compile_error()
                .into();
        }
    };

    // Optional auto-derived JSON Schema constructor. Only emitted when the
    // macro was invoked with `input = TypeName` and that type implements
    // `schemars::JsonSchema`. The constructor returns a `serde_json::Value`
    // suitable for embedding in a `ToolManifest.payload_schema`.
    let schema_fn = match &attrs.input_type {
        Some(ty) => quote! {
            /// JSON Schema (draft-07) for this tool's payload, derived from
            /// `#ty` via `schemars`. Embed in `ToolManifest.payload_schema`.
            pub fn payload_schema() -> serde_json::Value {
                let schema = schemars::schema_for!(#ty);
                serde_json::to_value(&schema).expect(
                    "schemars-derived schema must serialise to JSON",
                )
            }
        },
        None => quote! {},
    };

    let expanded = quote! {
        // Keep the original function available
        #func

        /// Auto-generated tool struct from `#[tool]` attribute.
        pub struct #struct_name;

        #[async_trait::async_trait]
        impl agentos_tools::traits::AgentTool for #struct_name {
            fn name(&self) -> &str {
                #tool_name
            }

            async fn execute(
                &self,
                payload: serde_json::Value,
                context: agentos_tools::traits::ToolExecutionContext,
            ) -> Result<serde_json::Value, agentos_types::AgentOSError> {
                #func_name(payload, context).await
            }

            fn required_permissions(&self) -> Vec<(String, agentos_types::PermissionOp)> {
                vec![#(#perm_entries),*]
            }
        }

        impl #struct_name {
            pub fn version() -> &'static str {
                #tool_version
            }

            pub fn description() -> &'static str {
                #tool_description
            }

            #schema_fn
        }
    };

    TokenStream::from(expanded)
}
