//! Derive macros for SACP JSON-RPC traits.
//!
//! This crate provides derive macros to reduce boilerplate when implementing
//! custom JSON-RPC requests, notifications, and response types.
//!
//! # Example
//!
//! ```ignore
//! use sacp::{JrRequest, JrNotification, JrResponsePayload};
//!
//! #[derive(Debug, Clone, Serialize, Deserialize, JrRequest)]
//! #[request(method = "_hello", response = HelloResponse)]
//! struct HelloRequest {
//!     name: String,
//! }
//!
//! #[derive(Debug, Serialize, Deserialize, JrResponsePayload)]
//! struct HelloResponse {
//!     greeting: String,
//! }
//!
//! #[derive(Debug, Clone, Serialize, Deserialize, JrNotification)]
//! #[notification(method = "_ping")]
//! struct PingNotification {
//!     timestamp: u64,
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Expr, Lit, Type};

/// Derive macro for implementing `JrRequest` and `JrMessage` traits.
///
/// # Attributes
///
/// - `#[request(method = "method_name", response = ResponseType)]`
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, Clone, Serialize, Deserialize, JrRequest)]
/// #[request(method = "_hello", response = HelloResponse)]
/// struct HelloRequest {
///     name: String,
/// }
/// ```
#[proc_macro_derive(JrRequest, attributes(request))]
pub fn derive_jr_request(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    // Parse attributes
    let (method, response_type) = match parse_request_attrs(&input) {
        Ok(attrs) => attrs,
        Err(e) => return e.to_compile_error().into(),
    };

    let expanded = quote! {
        impl sacp::JrMessage for #name {
            fn method(&self) -> &str {
                #method
            }

            fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
                sacp::UntypedMessage::new(#method, self)
            }

            fn parse_message(
                method: &str,
                params: &impl serde::Serialize,
            ) -> Option<Result<Self, sacp::Error>> {
                if method != #method {
                    return None;
                }
                Some(sacp::util::json_cast(params))
            }
        }

        impl sacp::JrRequest for #name {
            type Response = #response_type;
        }
    };

    TokenStream::from(expanded)
}

/// Derive macro for implementing `JrNotification` and `JrMessage` traits.
///
/// # Attributes
///
/// - `#[notification(method = "method_name")]`
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, Clone, Serialize, Deserialize, JrNotification)]
/// #[notification(method = "_ping")]
/// struct PingNotification {
///     timestamp: u64,
/// }
/// ```
#[proc_macro_derive(JrNotification, attributes(notification))]
pub fn derive_jr_notification(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    // Parse attributes
    let method = match parse_notification_attrs(&input) {
        Ok(method) => method,
        Err(e) => return e.to_compile_error().into(),
    };

    let expanded = quote! {
        impl sacp::JrMessage for #name {
            fn method(&self) -> &str {
                #method
            }

            fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
                sacp::UntypedMessage::new(#method, self)
            }

            fn parse_message(
                method: &str,
                params: &impl serde::Serialize,
            ) -> Option<Result<Self, sacp::Error>> {
                if method != #method {
                    return None;
                }
                Some(sacp::util::json_cast(params))
            }
        }

        impl sacp::JrNotification for #name {}
    };

    TokenStream::from(expanded)
}

/// Derive macro for implementing `JrResponsePayload` trait.
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, Serialize, Deserialize, JrResponsePayload)]
/// struct HelloResponse {
///     greeting: String,
/// }
/// ```
#[proc_macro_derive(JrResponsePayload)]
pub fn derive_jr_response_payload(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let expanded = quote! {
        impl sacp::JrResponsePayload for #name {
            fn into_json(self, _method: &str) -> Result<serde_json::Value, sacp::Error> {
                serde_json::to_value(self).map_err(sacp::Error::into_internal_error)
            }

            fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, sacp::Error> {
                sacp::util::json_cast(value)
            }
        }
    };

    TokenStream::from(expanded)
}

fn parse_request_attrs(input: &DeriveInput) -> syn::Result<(String, Type)> {
    let mut method: Option<String> = None;
    let mut response_type: Option<Type> = None;

    for attr in &input.attrs {
        if !attr.path().is_ident("request") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("method") {
                let value: Expr = meta.value()?.parse()?;
                if let Expr::Lit(expr_lit) = value {
                    if let Lit::Str(lit_str) = expr_lit.lit {
                        method = Some(lit_str.value());
                        return Ok(());
                    }
                }
                return Err(meta.error("expected string literal for method"));
            }

            if meta.path.is_ident("response") {
                let value: Expr = meta.value()?.parse()?;
                if let Expr::Path(expr_path) = value {
                    response_type = Some(Type::Path(syn::TypePath {
                        qself: None,
                        path: expr_path.path,
                    }));
                    return Ok(());
                }
                return Err(meta.error("expected type for response"));
            }

            Err(meta.error("unknown attribute"))
        })?;
    }

    let method = method.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "missing required attribute: #[request(method = \"...\")]",
        )
    })?;

    let response_type = response_type.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "missing required attribute: #[request(response = ...)]",
        )
    })?;

    Ok((method, response_type))
}

fn parse_notification_attrs(input: &DeriveInput) -> syn::Result<String> {
    let mut method: Option<String> = None;

    for attr in &input.attrs {
        if !attr.path().is_ident("notification") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("method") {
                let value: Expr = meta.value()?.parse()?;
                if let Expr::Lit(expr_lit) = value {
                    if let Lit::Str(lit_str) = expr_lit.lit {
                        method = Some(lit_str.value());
                        return Ok(());
                    }
                }
                return Err(meta.error("expected string literal for method"));
            }

            Err(meta.error("unknown attribute"))
        })?;
    }

    method.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "missing required attribute: #[notification(method = \"...\")]",
        )
    })
}
