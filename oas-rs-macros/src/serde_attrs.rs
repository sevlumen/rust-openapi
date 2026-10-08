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
            if meta.path.is_ident("rename") && has_value {
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

fn apply_rename_all(rule: &str, original: &str, base: NameBase) -> Option<String> {
    let words = split_words(original, base);
    let capitalize = |word: &String| {
        let mut chars = word.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    Some(match rule {
        "lowercase" => original.to_ascii_lowercase(),
        "UPPERCASE" => original.to_ascii_uppercase(),
        "PascalCase" => words.iter().map(capitalize).collect(),
        "camelCase" => words
            .iter()
            .enumerate()
            .map(|(index, word)| {
                if index == 0 {
                    word.clone()
                } else {
                    capitalize(word)
                }
            })
            .collect(),
        "snake_case" => words.join("_"),
        "SCREAMING_SNAKE_CASE" => words.join("_").to_ascii_uppercase(),
        "kebab-case" => words.join("-"),
        "SCREAMING-KEBAB-CASE" => words.join("-").to_ascii_uppercase(),
        _ => return None,
    })
}

/// Splits an identifier into lowercase words.
fn split_words(original: &str, base: NameBase) -> Vec<String> {
    match base {
        NameBase::Snake => original
            .split('_')
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect(),
        NameBase::Pascal => {
            let mut words: Vec<String> = Vec::new();
            for character in original.chars() {
                if character.is_uppercase() || words.is_empty() {
                    words.push(character.to_lowercase().collect());
                } else if let Some(last) = words.last_mut() {
                    last.push(character);
                }
            }
            words
        }
    }
}
