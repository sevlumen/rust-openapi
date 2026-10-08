//! Minimal reader for the subset of `#[serde(...)]` that affects wire names.

use syn::{Attribute, Ident, LitStr, Token, ext::IdentExt, meta::ParseNestedMeta};

#[derive(Default)]
pub(crate) struct SerdeAttrs {
    pub(crate) rename: Option<String>,
    pub(crate) rename_all: Option<String>,
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
            if meta.path.is_ident("rename") && meta.input.peek(Token![=]) {
                found.rename = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("rename_all") && meta.input.peek(Token![=]) {
                found.rename_all = Some(meta.value()?.parse::<LitStr>()?.value());
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
