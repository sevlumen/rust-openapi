//! Minimal reader for the subset of `#[serde(...)]` that affects wire names.

use syn::{Attribute, Ident, LitStr, Token, ext::IdentExt, meta::ParseNestedMeta};

#[derive(Default)]
pub(crate) struct SerdeAttrs {
    pub(crate) rename: Option<String>,
    pub(crate) rename_all: Option<String>,
    pub(crate) skip_serializing: bool,
    pub(crate) skip_deserializing: bool,
    /// `default`, in any form: the field may be missing on input.
    pub(crate) default: bool,
    /// `skip_serializing_if = "..."`: the field may be missing on output.
    pub(crate) skip_serializing_if: bool,
    pub(crate) flatten: bool,
    pub(crate) tag: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) untagged: bool,
    pub(crate) transparent: bool,
    pub(crate) deny_unknown_fields: bool,
    pub(crate) rename_all_fields: Option<String>,
    /// `alias`, `with` or `deserialize_with`: serde must do the decoding.
    pub(crate) custom_decode: bool,
}

impl SerdeAttrs {
    /// `#[serde(skip)]`, or both directions skipped.
    pub(crate) fn skipped(&self) -> bool {
        self.skip_serializing && self.skip_deserializing
    }
}

#[derive(Clone, Copy)]
pub(crate) enum NameBase {
    Snake,
    Pascal,
}

pub(crate) fn parse(attrs: &[Attribute]) -> syn::Result<SerdeAttrs> {
    let mut found = SerdeAttrs::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            let has_value = meta.input.peek(Token![=]);
            if (meta.path.is_ident("rename") || meta.path.is_ident("rename_all"))
                && meta.input.peek(syn::token::Paren)
            {
                // `rename(serialize = "a", deserialize = "b")`: one schema cannot
                // describe two names, so only an equal pair is accepted.
                let is_rename = meta.path.is_ident("rename");
                let (mut serialize, mut deserialize) = (None, None);
                meta.parse_nested_meta(|inner| {
                    let value = inner.value()?.parse::<LitStr>()?.value();
                    if inner.path.is_ident("serialize") {
                        serialize = Some(value);
                    } else if inner.path.is_ident("deserialize") {
                        deserialize = Some(value);
                    }
                    Ok(())
                })?;
                match (serialize, deserialize) {
                    (Some(a), Some(b)) if a == b => {
                        if is_rename {
                            found.rename = Some(a);
                        } else {
                            found.rename_all = Some(a);
                        }
                    }
                    _ => {
                        return Err(meta.error(
                            "ApiSchema cannot describe different serialize and deserialize names \
                             (one schema serves requests and responses): use a single name, or \
                             implement ApiSchema by hand",
                        ));
                    }
                }
            } else if meta.path.is_ident("rename") && has_value {
                found.rename = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("rename_all") && has_value {
                found.rename_all = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("rename_all_fields") && has_value {
                found.rename_all_fields = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("transparent") {
                found.transparent = true;
            } else if meta.path.is_ident("deny_unknown_fields") {
                found.deny_unknown_fields = true;
            } else if meta.path.is_ident("tag") && has_value {
                found.tag = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("content") && has_value {
                found.content = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("untagged") {
                found.untagged = true;
            } else if meta.path.is_ident("skip") {
                found.skip_serializing = true;
                found.skip_deserializing = true;
            } else if meta.path.is_ident("skip_serializing") {
                found.skip_serializing = true;
            } else if meta.path.is_ident("skip_deserializing") {
                found.skip_deserializing = true;
            } else if meta.path.is_ident("flatten") {
                found.flatten = true;
            } else if meta.path.is_ident("default") {
                found.default = true;
                skip_meta(&meta)?;
            } else if meta.path.is_ident("alias")
                || meta.path.is_ident("with")
                || meta.path.is_ident("deserialize_with")
            {
                found.custom_decode = true;
                skip_meta(&meta)?;
            } else if meta.path.is_ident("skip_serializing_if") {
                found.skip_serializing_if = true;
                skip_meta(&meta)?;
            } else {
                skip_meta(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(found)
}

/// Consumes an attribute we do not interpret (`flatten`, `default = "f"`,
/// `rename(serialize = "a")`, ...) so parsing can continue.
fn skip_meta(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(Token![=]) {
        meta.value()?.parse::<syn::Expr>()?;
    } else if meta.input.peek(syn::token::Paren) {
        meta.parse_nested_meta(|inner| skip_meta(&inner))?;
    }
    Ok(())
}

/// The name a field or variant has on the wire: an explicit `rename`, else the
/// container's `rename_all` rule applied to the identifier, else the identifier.
pub(crate) fn wire_name(
    attrs: &[Attribute],
    ident: &Ident,
    rename_all: Option<&str>,
    base: NameBase,
) -> syn::Result<String> {
    if let Some(rename) = parse(attrs)?.rename {
        return Ok(rename);
    }
    let original = ident.unraw().to_string();
    match rename_all {
        None => Ok(original),
        Some(rule) => apply_rename_all(rule, &original, base).ok_or_else(|| {
            syn::Error::new(
                ident.span(),
                format!("unknown serde rename_all rule `{rule}`"),
            )
        }),
    }
}

/// Serde's own casing rules, ported rule for rule (including how they treat
/// underscores, existing capitals and the base identifier convention: field
/// names are `snake_case`, variant names `PascalCase`), so the schema names are
/// exactly what serde writes.
fn apply_rename_all(rule: &str, original: &str, base: NameBase) -> Option<String> {
    Some(match base {
        NameBase::Snake => match rule {
            "lowercase" | "snake_case" => original.to_owned(),
            "UPPERCASE" | "SCREAMING_SNAKE_CASE" => original.to_ascii_uppercase(),
            "PascalCase" => pascal_from_snake(original),
            "camelCase" => {
                let pascal = pascal_from_snake(original);
                match pascal.chars().next() {
                    Some(first) => {
                        first.to_ascii_lowercase().to_string() + &pascal[first.len_utf8()..]
                    }
                    None => pascal,
                }
            }
            "kebab-case" => original.replace('_', "-"),
            "SCREAMING-KEBAB-CASE" => original.to_ascii_uppercase().replace('_', "-"),
            _ => return None,
        },
        NameBase::Pascal => {
            let snake = || {
                let mut snake = String::new();
                for (index, character) in original.char_indices() {
                    if index > 0 && character.is_uppercase() {
                        snake.push('_');
                    }
                    snake.push(character.to_ascii_lowercase());
                }
                snake
            };
            match rule {
                "PascalCase" => original.to_owned(),
                "lowercase" => original.to_ascii_lowercase(),
                "UPPERCASE" => original.to_ascii_uppercase(),
                "camelCase" => match original.chars().next() {
                    Some(first) => {
                        first.to_ascii_lowercase().to_string() + &original[first.len_utf8()..]
                    }
                    None => String::new(),
                },
                "snake_case" => snake(),
                "SCREAMING_SNAKE_CASE" => snake().to_ascii_uppercase(),
                "kebab-case" => snake().replace('_', "-"),
                "SCREAMING-KEBAB-CASE" => snake().to_ascii_uppercase().replace('_', "-"),
                _ => return None,
            }
        }
    })
}

fn pascal_from_snake(original: &str) -> String {
    let mut pascal = String::new();
    let mut capitalize = true;
    for character in original.chars() {
        if character == '_' {
            capitalize = true;
        } else if capitalize {
            pascal.push(character.to_ascii_uppercase());
            capitalize = false;
        } else {
            pascal.push(character);
        }
    }
    pascal
}
