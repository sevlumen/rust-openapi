use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, GenericArgument, PathArguments, Type, parse_macro_input};

#[proc_macro_derive(ApiSchema)]
pub fn derive_api_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;
    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return syn::Error::new_spanned(name, "ApiSchema requires named struct fields")
                    .to_compile_error()
                    .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(name, "ApiSchema can only derive for structs")
                .to_compile_error()
                .into();
        }
    };

    let mut properties = Vec::new();
    let mut required = Vec::new();
    let mut parameters = Vec::new();
    let mut query_variables = Vec::new();
    let mut query_arms = Vec::new();
    let mut raw_query_arms = Vec::new();
    let mut query_fields = Vec::new();
    let mut direct_query_parser = true;
    for field in fields {
        let field_name = field.ident.expect("named field");
        let field_name_string = field_name.to_string();
        let (schema_type, is_optional) = option_inner(&field.ty);
        let required_flag = !is_optional;
        let schema = quote! { <#schema_type as ::oas_rs::ApiSchema>::schema() };
        properties.push(quote! {
            properties.insert(#field_name_string.to_owned(), #schema);
        });
        parameters.push(quote! {
            parameters.push(::oas_rs::__private::serde_json::json!({
                "in": "query",
                "name": #field_name_string,
                "required": #required_flag,
                "schema": <#schema_type as ::oas_rs::ApiSchema>::schema()
            }));
        });
        if !is_optional {
            required.push(quote! { required.push(#field_name_string); });
        }
        direct_query_parser &= supports_query_value(schema_type);
        query_variables.push(quote! {
            let mut #field_name: Option<#schema_type> = None;
        });
        query_arms.push(quote! {
                #field_name_string => {
                #field_name = Some(::oas_rs::__private::parse_query_value::<#schema_type>(&value)?);
            }
        });
        raw_query_arms.push(quote! {
            #field_name_string => {
                let value = ::oas_rs::__private::decode_query_component(raw_value)?;
                #field_name = Some(::oas_rs::__private::parse_query_value::<#schema_type>(&value)?);
            }
        });
        let query_value = if is_optional {
            quote! { #field_name }
        } else {
            quote! {
                #field_name.ok_or_else(|| ::oas_rs::ApiError::bad_request(
                    format!("missing query parameter {}", #field_name_string)
                ))?
            }
        };
        query_fields.push(quote! { #field_name: #query_value });
    }

    let required_value = if required.is_empty() {
        quote! { None }
    } else {
        quote! { Some(::oas_rs::__private::serde_json::json!(required)) }
    };

    let query_parser = if direct_query_parser {
        quote! {
            fn parse(query: &str) -> Result<Self, ::oas_rs::ApiError> {
                #(#query_variables)*
                for pair in query.split('&').filter(|pair| !pair.is_empty()) {
                    let (key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
                    match key {
                        #(#raw_query_arms,)*
                        _ => {
                            let key = ::oas_rs::__private::decode_query_component(key)?;
                            let value = ::oas_rs::__private::decode_query_component(raw_value)?;
                            match key.as_ref() {
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

    quote! {
        impl ::oas_rs::ApiSchema for #name {
            fn schema() -> ::oas_rs::__private::serde_json::Value {
                let mut properties = ::oas_rs::__private::serde_json::Map::new();
                #(#properties)*
                let mut schema = ::oas_rs::__private::serde_json::Map::new();
                schema.insert("type".to_owned(), ::oas_rs::__private::serde_json::json!("object"));
                schema.insert("properties".to_owned(), ::oas_rs::__private::serde_json::Value::Object(properties));
                let mut required = Vec::new();
                #(#required)*
                if let Some(required) = #required_value {
                    schema.insert("required".to_owned(), required);
                }
                ::oas_rs::__private::serde_json::Value::Object(schema)
            }
        }

        impl ::oas_rs::__private::OpenApiQuery for #name {
            fn parameters() -> Vec<::oas_rs::__private::serde_json::Value> {
                let mut parameters = Vec::new();
                #(#parameters)*
                parameters
            }

            #query_parser
        }
    }
    .into()
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
