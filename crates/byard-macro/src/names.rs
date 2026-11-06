//! How a Rust name is spelled in Byld.
//!
//! Byld spells its methods camelCase (`toLower`, `sortBy`, `formatDate`), and
//! Rust spells functions `snake_case`, so the controller macro translates a
//! method's name: `fn export_csv` is `room.exportCsv(…)`. Only methods:
//! attributes and events (`row_height`, `key_down`) and record fields (data,
//! `temperature_2m`) keep the name they were given.

/// `snake_case` as `camelCase`. A name with no underscore is returned as it
/// is, so a word that was already camelCase keeps its capitals. A leading
/// underscore (`_unused`) and a doubled one are dropped, and a digit after an
/// underscore stays a digit (`to_utf8` is `toUtf8`).
pub fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = false;
    for ch in name.chars() {
        if ch == '_' {
            // The first word is never capitalised, whatever precedes it.
            upper_next = !out.is_empty();
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The Byld spelling of a Rust identifier (a raw one, `r#type`, loses its `r#`).
pub fn of_ident(ident: &syn::Ident) -> String {
    use syn::ext::IdentExt as _;
    camel_case(&ident.unraw().to_string())
}

#[cfg(test)]
mod tests {
    use super::camel_case;

    #[test]
    fn snake_case_becomes_camel_case() {
        assert_eq!(camel_case("export_csv"), "exportCsv");
        assert_eq!(camel_case("unit_price"), "unitPrice");
        assert_eq!(camel_case("to_utf8"), "toUtf8");
        assert_eq!(camel_case("a_b_c"), "aBC");
    }

    #[test]
    fn a_single_word_is_unchanged() {
        assert_eq!(camel_case("forecast"), "forecast");
        assert_eq!(camel_case("alreadyCamel"), "alreadyCamel");
    }

    #[test]
    fn stray_underscores_are_dropped() {
        assert_eq!(camel_case("_hidden"), "hidden");
        assert_eq!(camel_case("a__b"), "aB");
        assert_eq!(camel_case("trailing_"), "trailing");
    }
}
