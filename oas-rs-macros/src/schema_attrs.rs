//! `#[api_schema(...)]` on fields, plus doc comments as descriptions.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Expr, ExprLit, Lit, LitStr, Meta};

/// What a field's `api_schema` attributes and doc comment add to its schema.
#[derive(Default)]
pub(crate) struct FieldDoc {
    pub(crate) description: Option<String>,
    /// `(schema keyword, value expression)` pairs.
    pub(crate) entries: Vec<(&'static str, Expr)>,
    pub(crate) deprecated: bool,
}

/// The text of the doc comment: one leading space stripped per line, blank
/// edges trimmed. `None` when there is no doc comment.
pub(crate) fn doc_comment(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("doc")) {
        if let Meta::NameValue(name_value) = &attr.meta
            && let Expr::Lit(ExprLit {
                lit: Lit::Str(text),
                ..
            }) = &name_value.value
        {
            let value = text.value();
            lines.push(value.strip_prefix(' ').unwrap_or(&value).to_owned());
        }
    }
    let text = lines.join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Keywords that take a value, and the schema keyword each one writes.
const VALUED: &[(&str, &str)] = &[
    ("example", "example"),
    ("minimum", "minimum"),
    ("maximum", "maximum"),
    ("min_length", "minLength"),
    ("max_length", "maxLength"),
    ("pattern", "pattern"),
    ("min_items", "minItems"),
    ("max_items", "maxItems"),
    ("format", "format"),
];

pub(crate) fn field_doc(attrs: &[Attribute]) -> syn::Result<FieldDoc> {
    let mut found = FieldDoc {
        description: doc_comment(attrs),
        ..FieldDoc::default()
    };
    for attr in attrs
        .iter()
        .filter(|attr| attr.path().is_ident("api_schema"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("deprecated") {
                found.deprecated = true;
                return Ok(());
            }
            if meta.path.is_ident("description") {
                found.description = Some(meta.value()?.parse::<LitStr>()?.value());
                return Ok(());
            }
            for (name, keyword) in VALUED {
                if meta.path.is_ident(name) {
                    found
                        .entries
                        .push((keyword, meta.value()?.parse::<Expr>()?));
                    return Ok(());
                }
            }
            Err(meta.error(
                "unsupported api_schema attribute; expected one of description, example, \
                 minimum, maximum, min_length, max_length, pattern, min_items, max_items, \
                 format, deprecated",
            ))
        })?;
    }
    Ok(found)
}

impl FieldDoc {
    /// Statements that decorate a schema value held in the variable `property`.
    pub(crate) fn decorate(&self) -> TokenStream {
        let description = self.description.as_ref().map(|text| {
            quote! { object.insert("description".to_owned(), ::oas_rs::__private::serde_json::json!(#text)); }
        });
        let deprecated = self.deprecated.then(|| {
            quote! { object.insert("deprecated".to_owned(), ::oas_rs::__private::serde_json::json!(true)); }
        });
        let entries = self.entries.iter().map(|(keyword, value)| {
            quote! { object.insert(#keyword.to_owned(), ::oas_rs::__private::serde_json::json!(#value)); }
        });
        quote! {
            if let ::oas_rs::__private::serde_json::Value::Object(object) = &mut property {
                #description
                #deprecated
                #(#entries)*
            }
        }
    }
}
