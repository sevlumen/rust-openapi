use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Data, DataEnum, DeriveInput, Field, Fields, GenericArgument, Ident, PathArguments, Type,
    parse_macro_input,
};

mod schema_attrs;
mod serde_attrs;

use schema_attrs::{doc_comment, field_doc};
use serde_attrs::{NameBase, SerdeAttrs, wire_name};

const JSON: &str = "::oas_rs::__private::serde_json";

fn json() -> TokenStream2 {
    JSON.parse().expect("a valid path")
}

/// The component name from `#[api_schema(name = "...")]`, or the type's name.
fn schema_name(attrs: &[syn::Attribute], default: &Ident) -> syn::Result<String> {
    let mut name = default.to_string();
    for attr in attrs
        .iter()
        .filter(|attr| attr.path().is_ident("api_schema"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: syn::LitStr = meta.value()?.parse()?;
                name = value.value();
                Ok(())
            } else {
                Err(meta.error("unsupported api_schema attribute; expected `name = \"...\"`"))
            }
        })?;
    }
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !valid {
        return Err(syn::Error::new_spanned(
            attrs.iter().find(|attr| attr.path().is_ident("api_schema")),
            "an OpenAPI component name may only contain letters, digits, '.', '-' and '_'",
        ));
    }
    Ok(name)
}

#[proc_macro_derive(ApiSchema, attributes(serde, api_schema))]
pub fn derive_api_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let component = schema_name(&input.attrs, &input.ident)?;
    let serde = serde_attrs::parse(&input.attrs)?;
    let description = doc_comment(&input.attrs);
    let name = input.ident;
    match input.data {
        Data::Enum(data) => derive_enum(&name, &component, &data, &serde, description),
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => {
                derive_struct(&name, &component, fields.named.iter(), &serde, description)
            }
            _ => Err(syn::Error::new_spanned(
                name,
                "ApiSchema requires named struct fields",
            )),
        },
        _ => Err(syn::Error::new_spanned(
            name,
            "ApiSchema can only derive for structs and enums",
        )),
    }
}

/// `impl ApiSchema` around a schema expression that may use `registry`.
fn schema_impl(name: &Ident, component: &str, body: TokenStream2) -> TokenStream2 {
    let json = json();
    quote! {
        impl ::oas_rs::ApiSchema for #name {
            fn schema() -> #json::Value {
                <Self as ::oas_rs::ApiSchema>::schema_with(&mut ::oas_rs::SchemaRegistry::inline())
            }

            fn schema_with(registry: &mut ::oas_rs::SchemaRegistry) -> #json::Value {
                registry.define::<Self>(#component, |registry| {
                    let _ = &registry;
                    #body
                })
            }
        }
    }
}

/// An expression building the object schema for `fields`. `tag` adds a
/// required string property fixed to one value (an internally tagged variant).
fn object_schema<'a>(
    fields: impl Iterator<Item = &'a Field>,
    rename_all: Option<&str>,
    tag: Option<(&str, &str)>,
    description: Option<&str>,
) -> syn::Result<TokenStream2> {
    let json = json();
    let mut statements = Vec::new();
    if let Some((key, value)) = tag {
        statements.push(quote! {
            properties.insert(#key.to_owned(), #json::json!({ "type": "string", "enum": [#value] }));
            required.push(#key.to_owned());
        });
    }
    for field in fields {
        let serde = serde_attrs::parse(&field.attrs)?;
        if serde.skipped() {
            continue;
        }
        let (schema_type, is_optional) = option_inner(&field.ty);
        if serde.flatten {
            statements.push(quote! {
                all_of.push(<#schema_type as ::oas_rs::ApiSchema>::schema_with(registry));
            });
            continue;
        }
        let ident = field.ident.as_ref().expect("named field");
        let wire = wire_name(&field.attrs, ident, rename_all, NameBase::Snake)?;
        let doc = field_doc(&field.attrs)?;
        let decorate = doc.decorate();
        let direction = if serde.skip_serializing {
            quote! { object.insert("writeOnly".to_owned(), #json::json!(true)); }
        } else if serde.skip_deserializing {
            quote! { object.insert("readOnly".to_owned(), #json::json!(true)); }
        } else {
            quote! {}
        };
        let is_required = !(is_optional
            || serde.default
            || serde.skip_serializing_if
            || serde.skip_deserializing);
        let require = is_required.then(|| quote! { required.push(#wire.to_owned()); });
        statements.push(quote! {
            {
                let mut property = <#schema_type as ::oas_rs::ApiSchema>::schema_with(registry);
                #decorate
                if let #json::Value::Object(object) = &mut property {
                    #direction
                }
                properties.insert(#wire.to_owned(), property);
                #require
            }
        });
    }
    let description = description.map(|text| {
        quote! { schema.insert("description".to_owned(), #json::json!(#text)); }
    });
    Ok(quote! {
        {
            let mut properties = #json::Map::new();
            let mut required: Vec<String> = Vec::new();
            let mut all_of: Vec<#json::Value> = Vec::new();
            #(#statements)*
            let mut schema = #json::Map::new();
            schema.insert("type".to_owned(), #json::json!("object"));
            #description
            schema.insert("properties".to_owned(), #json::Value::Object(properties));
            if !required.is_empty() {
                schema.insert("required".to_owned(), #json::json!(required));
            }
            if !all_of.is_empty() {
                schema.insert("allOf".to_owned(), #json::Value::Array(all_of));
            }
            #json::Value::Object(schema)
        }
    })
}

fn derive_struct<'a>(
    name: &Ident,
    component: &str,
    fields: impl Iterator<Item = &'a Field> + Clone,
    container: &SerdeAttrs,
    description: Option<String>,
) -> syn::Result<TokenStream2> {
    let rename_all = container.rename_all.as_deref();
    let json = json();
    let schema = object_schema(fields.clone(), rename_all, None, description.as_deref())?;

    // Query parameters and the direct query parser: skipped and flattened
    // fields are left out, and any of them disables the direct parser (the
    // serde fallback understands them).
    let mut parameters = Vec::new();
    let mut query_variables = Vec::new();
    let mut query_arms = Vec::new();
    let mut raw_query_arms = Vec::new();
    let mut query_fields = Vec::new();
    let mut direct_query_parser = true;
    for (index, field) in fields.enumerate() {
        let serde = serde_attrs::parse(&field.attrs)?;
        if serde.skipped() || serde.flatten || serde.skip_deserializing {
            direct_query_parser = false;
            continue;
        }
        let field_name = field.ident.as_ref().expect("named field");
        let wire = wire_name(&field.attrs, field_name, rename_all, NameBase::Snake)?;
        let (schema_type, is_optional) = option_inner(&field.ty);
        let required_flag = !is_optional && !serde.default;
        parameters.push(quote! {
            parameters.push(#json::json!({
                "in": "query",
                "name": #wire,
                "required": #required_flag,
                "schema": <#schema_type as ::oas_rs::ApiSchema>::schema()
            }));
        });
        // `default` fields are filled by serde, not by the direct parser.
        if serde.default {
            direct_query_parser = false;
        }
        direct_query_parser &= supports_query_value(schema_type);
        let variable = format_ident!("__oas_field_{}", index);
        query_variables.push(quote! {
            let mut #variable: Option<#schema_type> = None;
        });
        query_arms.push(quote! {
            #wire => {
                #variable = Some(::oas_rs::__private::parse_query_value::<#schema_type>(&__oas_value)?);
            }
        });
        raw_query_arms.push(quote! {
            #wire => {
                let __oas_value = ::oas_rs::__private::decode_query_component(__oas_raw)?;
                #variable = Some(::oas_rs::__private::parse_query_value::<#schema_type>(&__oas_value)?);
            }
        });
        let value = if is_optional {
            quote! { #variable }
        } else {
            quote! {
                #variable.ok_or_else(|| ::oas_rs::ApiError::bad_request(
                    format!("missing query parameter {}", #wire)
                ))?
            }
        };
        query_fields.push(quote! { #field_name: #value });
    }

    let query_parser = if direct_query_parser {
        quote! {
            fn parse(query: &str) -> Result<Self, ::oas_rs::ApiError> {
                #(#query_variables)*
                for __oas_pair in query.split('&').filter(|pair| !pair.is_empty()) {
                    let (__oas_key, __oas_raw) = __oas_pair.split_once('=').unwrap_or((__oas_pair, ""));
                    match __oas_key {
                        #(#raw_query_arms,)*
                        _ => {
                            let __oas_key = ::oas_rs::__private::decode_query_component(__oas_key)?;
                            let __oas_value = ::oas_rs::__private::decode_query_component(__oas_raw)?;
                            match __oas_key.as_ref() {
                                #(#query_arms,)*
                                _ => {}
                            }
                        }
                    }
                }
                Ok(Self {
                    #(#query_fields,)*
                })
            }
        }
    } else {
        quote! {}
    };

    let schema_impl = schema_impl(name, component, schema);
    Ok(quote! {
        #schema_impl

        impl ::oas_rs::__private::OpenApiQuery for #name {
            fn parameters() -> Vec<#json::Value> {
                let mut parameters = Vec::new();
                #(#parameters)*
                parameters
            }

            #query_parser
        }
    })
}

/// How an enum is represented on the wire (serde's four forms).
enum Tagging {
    External,
    Internal(String),
    Adjacent(String, String),
    Untagged,
}

fn derive_enum(
    name: &Ident,
    component: &str,
    data: &DataEnum,
    container: &SerdeAttrs,
    description: Option<String>,
) -> syn::Result<TokenStream2> {
    let json = json();
    let rename_all = container.rename_all.as_deref();
    let tagging = match (&container.tag, &container.content, container.untagged) {
        (_, _, true) => Tagging::Untagged,
        (Some(tag), Some(content), false) => Tagging::Adjacent(tag.clone(), content.clone()),
        (Some(tag), None, false) => Tagging::Internal(tag.clone()),
        (None, _, false) => Tagging::External,
    };
    let variants: Vec<_> = data
        .variants
        .iter()
        .map(|variant| Ok((variant, serde_attrs::parse(&variant.attrs)?)))
        .collect::<syn::Result<_>>()?;
    let variants: Vec<_> = variants
        .into_iter()
        .filter(|(_, serde)| !serde.skipped())
        .collect();

    // Unit-only enums keep the plain string enum.
    if matches!(tagging, Tagging::External)
        && variants
            .iter()
            .all(|(variant, _)| matches!(variant.fields, Fields::Unit))
    {
        let mut values = Vec::new();
        for (variant, _) in &variants {
            values.push(wire_name(
                &variant.attrs,
                &variant.ident,
                rename_all,
                NameBase::Pascal,
            )?);
        }
        let description = description.map(|text| quote! { "description": #text, });
        return Ok(schema_impl(
            name,
            component,
            quote! { #json::json!({ #description "type": "string", "enum": [#(#values),*] }) },
        ));
    }

    let mut alternatives = Vec::new();
    for (variant, serde) in &variants {
        let wire = wire_name(&variant.attrs, &variant.ident, rename_all, NameBase::Pascal)?;
        let variant_rename_all = serde.rename_all.as_deref();
        // The payload: what the variant carries, as a schema expression.
        let payload: Option<TokenStream2> = match &variant.fields {
            Fields::Unit => None,
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                let (ty, _) = option_inner(&fields.unnamed[0].ty);
                Some(quote! { <#ty as ::oas_rs::ApiSchema>::schema_with(registry) })
            }
            Fields::Unnamed(fields) => {
                let count = fields.unnamed.len();
                let items = fields.unnamed.iter().map(|field| {
                    let (ty, _) = option_inner(&field.ty);
                    quote! { <#ty as ::oas_rs::ApiSchema>::schema_with(registry) }
                });
                Some(quote! {
                    #json::json!({
                        "type": "array",
                        "prefixItems": [#(#items),*],
                        "minItems": #count,
                        "maxItems": #count
                    })
                })
            }
            Fields::Named(fields) => Some(object_schema(
                fields.named.iter(),
                variant_rename_all,
                None,
                None,
            )?),
        };
        let alternative = match (&tagging, payload) {
            (Tagging::External, None) => {
                quote! { #json::json!({ "type": "string", "enum": [#wire] }) }
            }
            (Tagging::External, Some(payload)) => quote! {
                {
                    let mut properties = #json::Map::new();
                    properties.insert(#wire.to_owned(), #payload);
                    #json::json!({
                        "type": "object",
                        "properties": #json::Value::Object(properties),
                        "required": [#wire]
                    })
                }
            },
            (Tagging::Internal(tag), None) => {
                quote! { #json::json!({
                    "type": "object",
                    "properties": { #tag: { "type": "string", "enum": [#wire] } },
                    "required": [#tag]
                }) }
            }
            (Tagging::Internal(tag), Some(_)) => match &variant.fields {
                Fields::Named(fields) => object_schema(
                    fields.named.iter(),
                    variant_rename_all,
                    Some((tag, &wire)),
                    None,
                )?,
                Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                    let (ty, _) = option_inner(&fields.unnamed[0].ty);
                    quote! { #json::json!({
                        "allOf": [
                            {
                                "type": "object",
                                "properties": { #tag: { "type": "string", "enum": [#wire] } },
                                "required": [#tag]
                            },
                            <#ty as ::oas_rs::ApiSchema>::schema_with(registry)
                        ]
                    }) }
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        variant,
                        "internally tagged enums cannot have tuple variants",
                    ));
                }
            },
            (Tagging::Adjacent(tag, _), None) => {
                quote! { #json::json!({
                    "type": "object",
                    "properties": { #tag: { "type": "string", "enum": [#wire] } },
                    "required": [#tag]
                }) }
            }
            (Tagging::Adjacent(tag, content), Some(payload)) => quote! {
                {
                    let mut properties = #json::Map::new();
                    properties.insert(#tag.to_owned(), #json::json!({ "type": "string", "enum": [#wire] }));
                    properties.insert(#content.to_owned(), #payload);
                    #json::json!({
                        "type": "object",
                        "properties": #json::Value::Object(properties),
                        "required": [#tag, #content]
                    })
                }
            },
            (Tagging::Untagged, None) => quote! { #json::json!({ "type": "null" }) },
            (Tagging::Untagged, Some(payload)) => payload,
        };
        alternatives.push(alternative);
    }
    let description = description.map(|text| {
        quote! { schema.insert("description".to_owned(), #json::json!(#text)); }
    });
    let body = quote! {
        {
            let mut schema = #json::Map::new();
            #description
            schema.insert("oneOf".to_owned(), #json::Value::Array(vec![#(#alternatives),*]));
            #json::Value::Object(schema)
        }
    };
    Ok(schema_impl(name, component, body))
}

fn supports_query_value(ty: &Type) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    if segment.ident == "Option"
        && let PathArguments::AngleBracketed(arguments) = &segment.arguments
        && let Some(GenericArgument::Type(inner)) = arguments.args.first()
    {
        return supports_query_value(inner);
    }
    matches!(
        segment.ident.to_string().as_str(),
        "String" | "bool" | "u32" | "u64" | "i32" | "i64" | "f32" | "f64" | "Uuid"
    )
}

fn option_inner(ty: &Type) -> (&Type, bool) {
    if let Type::Path(path) = ty
        && let Some(segment) = path.path.segments.last()
        && segment.ident == "Option"
        && let PathArguments::AngleBracketed(arguments) = &segment.arguments
        && let Some(GenericArgument::Type(inner)) = arguments.args.first()
    {
        return (inner, true);
    }
    (ty, false)
}
