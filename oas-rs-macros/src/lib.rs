use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, GenericArgument, PathArguments, Type, parse_macro_input};

#[proc_macro_derive(OpenApi)]
pub fn derive_openapi(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;
    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return syn::Error::new_spanned(name, "OpenApi requires named struct fields")
                    .to_compile_error()
                    .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(name, "OpenApi can only derive for structs")
                .to_compile_error()
                .into();
        }
    };

    let mut properties = Vec::new();
    let mut required = Vec::new();
    let mut parameters = Vec::new();
    for field in fields {
        let field_name = field.ident.expect("named field");
        let field_name_string = field_name.to_string();
        let (schema_type, is_optional) = option_inner(&field.ty);
        let required_flag = !is_optional;
        let schema = quote! { <#schema_type as ::oas_rs::OpenApiSchema>::schema() };
        properties.push(quote! {
            properties.insert(#field_name_string.to_owned(), #schema);
        });
        parameters.push(quote! {
            parameters.push(::oas_rs::serde_json::json!({
                "in": "query",
                "name": #field_name_string,
                "required": #required_flag,
                "schema": <#schema_type as ::oas_rs::OpenApiSchema>::schema()
            }));
        });
        if !is_optional {
            required.push(quote! { required.push(#field_name_string); });
        }
    }

    let required_value = if required.is_empty() {
        quote! { None }
    } else {
        quote! { Some(::oas_rs::serde_json::json!(required)) }
    };

    quote! {
        impl ::oas_rs::OpenApiSchema for #name {
            fn schema() -> ::oas_rs::serde_json::Value {
                let mut properties = ::oas_rs::serde_json::Map::new();
                #(#properties)*
                let mut schema = ::oas_rs::serde_json::Map::new();
                schema.insert("type".to_owned(), ::oas_rs::serde_json::json!("object"));
                schema.insert("properties".to_owned(), ::oas_rs::serde_json::Value::Object(properties));
                let mut required = Vec::new();
                #(#required)*
                if let Some(required) = #required_value {
                    schema.insert("required".to_owned(), required);
                }
                ::oas_rs::serde_json::Value::Object(schema)
            }
        }

        impl ::oas_rs::OpenApiQuery for #name {
            fn parameters() -> Vec<::oas_rs::serde_json::Value> {
                let mut parameters = Vec::new();
                #(#parameters)*
                parameters
            }
        }
    }
    .into()
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
