//! `#[derive(FromHostValue)]`, `#[derive(IntoHostValue)]` and
//! `#[derive(HostValue)]` (RFC-0028 §2, RFC-0029 §3): a struct crosses the
//! controller boundary as a `HostValue::Record`, field for field.
//!
//! The generated code names only `::byard::bridge::*`, like
//! `#[byard_controller]` does, so this crate keeps no dependency on
//! `byard-core`.
//!
//! The conversions are total, as the hand-written ones are (arguments are user
//! data, and user data never panics): a record that lacks a field gives that
//! field what its type makes of nothing (`0`, `""`, an empty list, `None`), and
//! a value that is not a record at all gives that for every field.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, GenericParam, LitStr, Type};

/// The two traits a derive can implement.
#[derive(Clone, Copy)]
enum Direction {
    From,
    Into,
}

impl Direction {
    fn trait_name(self) -> &'static str {
        match self {
            Self::From => "FromHostValue",
            Self::Into => "IntoHostValue",
        }
    }
}

/// One field of the struct, and the name it has in the record.
struct HostField<'a> {
    ident: &'a syn::Ident,
    ty: &'a Type,
    key: String,
}

/// `#[derive(FromHostValue)]`.
pub fn from_host_value(input: &DeriveInput) -> syn::Result<TokenStream> {
    let fields = host_fields(input, Direction::From)?;
    let generics = bounded(input, &quote!(::byard::bridge::FromHostValue));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let name = &input.ident;
    let decodes = fields.iter().map(|f| {
        let (ident, ty, key) = (f.ident, f.ty, &f.key);
        quote! {
            #ident: <#ty as ::byard::bridge::FromHostValue>::from_host(
                __fields
                    .iter()
                    .position(|(__key, _)| __key == #key)
                    .map_or(::byard::bridge::HostValue::Unit, |__at| {
                        __fields.swap_remove(__at).1
                    })
            )
        }
    });
    Ok(quote! {
        impl #impl_generics ::byard::bridge::FromHostValue for #name #ty_generics #where_clause {
            fn from_host(value: ::byard::bridge::HostValue) -> Self {
                // A struct with no fields never reads the vector it was given.
                #[allow(unused_mut, unused_variables)]
                let mut __fields = match value {
                    ::byard::bridge::HostValue::Record(__fields) => __fields,
                    _ => ::std::vec::Vec::new(),
                };
                Self { #(#decodes,)* }
            }
        }
    })
}

/// `#[derive(IntoHostValue)]`.
pub fn into_host_value(input: &DeriveInput) -> syn::Result<TokenStream> {
    let fields = host_fields(input, Direction::Into)?;
    let generics = bounded(input, &quote!(::byard::bridge::IntoHostValue));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let name = &input.ident;
    let encodes = fields.iter().map(|f| {
        let (ident, key) = (f.ident, &f.key);
        quote! {
            (
                ::std::string::String::from(#key),
                ::byard::bridge::IntoHostValue::into_host(self.#ident),
            )
        }
    });
    Ok(quote! {
        impl #impl_generics ::byard::bridge::IntoHostValue for #name #ty_generics #where_clause {
            fn into_host(self) -> ::byard::bridge::HostValue {
                ::byard::bridge::HostValue::Record(::std::vec![ #(#encodes,)* ])
            }
        }
    })
}

/// `#[derive(HostValue)]`: both, as RFC-0028 and RFC-0029 write it.
pub fn host_value(input: &DeriveInput) -> syn::Result<TokenStream> {
    let from = from_host_value(input);
    let into = into_host_value(input);
    match (from, into) {
        (Ok(from), Ok(into)) => Ok(quote!(#from #into)),
        // The two fail for the same reason: say it once.
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// `input`'s generics, each type parameter bounded by `trait_path`, so
/// `Page<T>` converts when `T` does.
fn bounded(input: &DeriveInput, trait_path: &TokenStream) -> syn::Generics {
    let mut generics = input.generics.clone();
    let parameters: Vec<syn::Ident> = generics
        .params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type(t) => Some(t.ident.clone()),
            _ => None,
        })
        .collect();
    let clause = generics.make_where_clause();
    for parameter in parameters {
        clause
            .predicates
            .push(syn::parse_quote!(#parameter: #trait_path));
    }
    generics
}

/// The named fields of a struct, with the key each has in the record.
fn host_fields(input: &DeriveInput, direction: Direction) -> syn::Result<Vec<HostField<'_>>> {
    let derive = direction.trait_name();
    let name = &input.ident;
    let data = match &input.data {
        Data::Struct(data) => data,
        Data::Enum(_) => {
            return Err(syn::Error::new_spanned(
                name,
                format!(
                    "`{derive}` cannot be derived for the enum `{name}`: only a struct with \
                     named fields crosses the controller boundary, as a record\n\
                     help: implement `{derive}` by hand, for instance as the variant's name in \
                     a `HostValue::Str`, or wrap the choice in a struct"
                ),
            ));
        }
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                name,
                format!("`{derive}` cannot be derived for the union `{name}`"),
            ));
        }
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            name,
            format!(
                "`{derive}` can only be derived for a struct with named fields, and `{name}` \
                 has none: a record is a set of named values\n\
                 help: give the fields names, or implement `{derive}` by hand"
            ),
        ));
    };

    let mut fields: Vec<HostField<'_>> = Vec::with_capacity(named.named.len());
    for field in &named.named {
        let ident = field.ident.as_ref().expect("a named field has a name");
        let key = renamed(field)?.unwrap_or_else(|| crate::names::of_ident(ident));
        if fields.iter().any(|other| other.key == key) {
            return Err(syn::Error::new_spanned(
                ident,
                format!("two fields of `{name}` are named `{key}` in the record"),
            ));
        }
        fields.push(HostField {
            ident,
            ty: &field.ty,
            key,
        });
    }
    Ok(fields)
}

/// `#[host(rename = "…")]` on a field: the name it has in the record.
fn renamed(field: &syn::Field) -> syn::Result<Option<String>> {
    let mut rename = None;
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("host")) {
        attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident("rename") {
                return Err(meta.error("unknown `host` option; the only one is `rename = \"…\"`"));
            }
            if rename.is_some() {
                return Err(meta.error("`rename` is given twice"));
            }
            let key: LitStr = meta.value()?.parse()?;
            if key.value().is_empty() {
                return Err(syn::Error::new(key.span(), "a record field needs a name"));
            }
            rename = Some(key.value());
            Ok(())
        })?;
    }
    Ok(rename)
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn message(result: syn::Result<TokenStream>) -> String {
        result.expect_err("the derive must refuse").to_string()
    }

    #[test]
    fn an_enum_is_refused_with_a_way_out() {
        let input: DeriveInput = parse_quote! { enum Mode { Fast, Slow } };
        let text = message(from_host_value(&input));
        assert!(
            text.contains("`FromHostValue` cannot be derived for the enum `Mode`"),
            "{text}"
        );
        assert!(text.contains("help:"), "{text}");
        let text = message(into_host_value(&input));
        assert!(
            text.contains("`IntoHostValue` cannot be derived for the enum `Mode`"),
            "{text}"
        );
        // The combined derive says it once, not twice.
        let text = message(host_value(&input));
        assert_eq!(text.matches("cannot be derived").count(), 1, "{text}");
    }

    #[test]
    fn a_tuple_or_unit_struct_is_refused() {
        for input in [
            parse_quote! { struct Pair(i32, i32); },
            parse_quote! { struct Marker; },
        ] {
            let text = message(from_host_value(&input));
            assert!(text.contains("named fields"), "{text}");
        }
    }

    #[test]
    fn a_union_is_refused() {
        let input: DeriveInput = parse_quote! { union Bits { a: u32, b: f32 } };
        assert!(message(into_host_value(&input)).contains("union"));
    }

    #[test]
    fn two_fields_with_one_key_are_refused() {
        let input: DeriveInput = parse_quote! {
            struct Clash { a: i32, #[host(rename = "a")] b: i32 }
        };
        let text = message(from_host_value(&input));
        assert!(
            text.contains("two fields of `Clash` are named `a`"),
            "{text}"
        );
    }

    #[test]
    fn a_bad_host_attribute_is_refused() {
        let unknown: DeriveInput = parse_quote! { struct S { #[host(skip)] a: i32 } };
        assert!(message(from_host_value(&unknown)).contains("unknown `host` option"));
        let twice: DeriveInput =
            parse_quote! { struct S { #[host(rename = "x", rename = "y")] a: i32 } };
        assert!(message(from_host_value(&twice)).contains("given twice"));
        let empty: DeriveInput = parse_quote! { struct S { #[host(rename = "")] a: i32 } };
        assert!(message(from_host_value(&empty)).contains("needs a name"));
        let bare: DeriveInput = parse_quote! { struct S { #[host(rename)] a: i32 } };
        assert!(from_host_value(&bare).is_err());
    }

    #[test]
    fn a_rename_and_a_raw_identifier_set_the_key() {
        let input: DeriveInput = parse_quote! {
            struct S { #[host(rename = "userName")] user_name: String, r#type: String, plain: i32 }
        };
        let keys: Vec<String> = host_fields(&input, Direction::From)
            .unwrap()
            .into_iter()
            .map(|f| f.key)
            .collect();
        assert_eq!(keys, ["userName", "type", "plain"]);
    }

    #[test]
    fn every_type_parameter_is_bounded() {
        let input: DeriveInput = parse_quote! {
            struct Page<'a, T, U: Clone = i32> where T: Sized {
                items: Vec<T>,
                next: Option<U>,
                tag: &'a str,
            }
        };
        let tokens = into_host_value(&input).unwrap().to_string();
        assert!(
            tokens.contains("T : :: byard :: bridge :: IntoHostValue"),
            "{tokens}"
        );
        assert!(
            tokens.contains("U : :: byard :: bridge :: IntoHostValue"),
            "{tokens}"
        );
        // The user's own bound stays, and the lifetime is carried.
        assert!(tokens.contains("T : Sized"), "{tokens}");
        assert!(tokens.contains("Page < 'a , T , U >"), "{tokens}");
    }
}
