use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse::Parser, parse_macro_input, ItemFn, LitStr, Meta, Token};

struct ToolAttrs {
    name: String,
    version: String,
    description: String,
    permissions: Vec<String>,
}

impl ToolAttrs {
    fn parse(input: proc_macro2::TokenStream) -> syn::Result<Self> {
        let mut name = None;
        let mut version = None;
        let mut description = None;
        let mut permissions = Vec::new();

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
        })
    }
}

/// Convert a kebab-case or snake_case name to PascalCase for the struct name.
fn to_pascal_case(s: &str) -> String {
    s.split(|c: char| c == '-' || c == '_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

/// Parse a permission string like "fs.read:r" into (resource, PermissionOp).
fn parse_permission(perm: &str) -> (String, proc_macro2::TokenStream) {
    let parts: Vec<&str> = perm.splitn(2, ':').collect();
    let resource = parts[0].to_string();
    let op = if parts.len() > 1 {
        match parts[1] {
            "r" => quote! { agentos_types::PermissionOp::Read },
            "w" => quote! { agentos_types::PermissionOp::Write },
            "x" => quote! { agentos_types::PermissionOp::Execute },
            "rw" => quote! { agentos_types::PermissionOp::Read }, // default to read for compound
            _ => quote! { agentos_types::PermissionOp::Read },
        }
    } else {
        quote! { agentos_types::PermissionOp::Read }
    };
    (resource, op)
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

    // Parse permissions
    let perm_entries: Vec<_> = attrs
        .permissions
        .iter()
        .map(|p| {
            let (resource, op) = parse_permission(p);
            let resource_lit = LitStr::new(&resource, proc_macro2::Span::call_site());
            quote! { (#resource_lit.to_string(), #op) }
        })
        .collect();

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
        }
    };

    TokenStream::from(expanded)
}
