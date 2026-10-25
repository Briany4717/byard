//! `#[byard::native_view(name = "…")]`, the declaration half of RFC-0039.
//!
//! # What it generates, and what it deliberately leaves alone
//!
//! Generated: the struct unchanged, an `impl NativeViewMeta` carrying the
//! catalog entry the compiler checks `byld` against, and a `set_prop` that
//! assigns each `#[prop]` field from the value the language evaluated this
//! tick. Left alone: `render`, `measure`, `on_event`, everything about what the
//! widget *is*. A macro that guessed at drawing would be a framework inside a
//! framework.
//!
//! # Why the catalog entry is generated rather than written
//!
//! Because it has to agree with the fields, and a hand-written entry agrees
//! with them only until somebody renames one. The prop list, its types, and
//! whether each reaches layout are read off the declaration, so the compiler's
//! view of a package widget cannot drift from the widget.
//!
//! Like the controller macro, everything emitted names `::byard::…`, so this
//! crate keeps no dependency on `byard-core`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Fields, ItemStruct, Type, parse_macro_input};

/// One declared prop: the field, and what the catalog should say about it.
struct Prop {
    ident: syn::Ident,
    /// The catalog's type name (`Int`, `Float`, `Bool`, `Str`, `Color`,
    /// `Vec2`, `Floats`).
    ty: &'static str,
    /// Whether a change can move geometry (`#[prop(layout)]`).
    layout: bool,
}

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let (name, role) = match view_args(attr.into(), &input) {
        Ok(args) => args,
        Err(e) => return e.to_compile_error().into(),
    };
    // RFC-0042 §2: what a screen reader calls the view. Unwritten, the trait's
    // own default (a group) stands.
    let role_const = role.map(|variant| {
        quote! {
            const ACCESS_ROLE: ::byard::access::AccessRole =
                ::byard::access::AccessRole::#variant;
        }
    });

    let mut props = Vec::new();
    let mut events: Vec<String> = Vec::new();
    let mut stripped = input.clone();
    if let Fields::Named(named) = &mut stripped.fields {
        for field in &mut named.named {
            let Some(ident) = field.ident.clone() else {
                continue;
            };
            let is_prop = field.attrs.iter().any(|a| a.path().is_ident("prop"));
            let is_event = field.attrs.iter().any(|a| a.path().is_ident("event"));
            if is_prop && is_event {
                return syn::Error::new_spanned(
                    &field.ident,
                    "a field is either a `#[prop]` or an `#[event]`, not both",
                )
                .to_compile_error()
                .into();
            }
            if is_prop {
                let layout = field.attrs.iter().any(|a| {
                    a.path().is_ident("prop")
                        && a.parse_args::<syn::Ident>()
                            .is_ok_and(|arg| arg == "layout")
                });
                match prop_type(&field.ty) {
                    Some(ty) => props.push(Prop { ident, ty, layout }),
                    None => {
                        return syn::Error::new_spanned(
                            &field.ty,
                            "a `#[prop]` must be a type `byld` can write: an integer, a float, \
                             a bool, a String, a colour (u32), a (f32, f32), or a Vec of numbers",
                        )
                        .to_compile_error()
                        .into();
                    }
                }
            } else if is_event {
                events.push(ident.to_string());
            }
            // The marker attributes are ours; the struct that comes out the
            // other side is ordinary Rust.
            field
                .attrs
                .retain(|a| !(a.path().is_ident("prop") || a.path().is_ident("event")));
        }
    }

    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let prop_names: Vec<String> = props.iter().map(|p| p.ident.to_string()).collect();
    let prop_types: Vec<syn::Ident> = props
        .iter()
        .map(|p| syn::Ident::new(p.ty, proc_macro2::Span::call_site()))
        .collect();
    let prop_layout: Vec<bool> = props.iter().map(|p| p.layout).collect();
    let prop_idents: Vec<&syn::Ident> = props.iter().map(|p| &p.ident).collect();

    let expanded = quote! {
        #stripped

        impl #impl_generics ::byard::render::NativeViewMeta for #ident #ty_generics #where_clause {
            #role_const

            const INFO: ::byard::render::NativeViewInfo = ::byard::render::NativeViewInfo {
                name: #name,
                props: &[
                    #(
                        ::byard::render::NativeProp {
                            name: #prop_names,
                            ty: ::byard::render::NativePropType::#prop_types,
                            layout: #prop_layout,
                        }
                    ),*
                ],
                events: &[ #( #events ),* ],
            };

            fn create() -> ::std::boxed::Box<dyn ::byard::render::NativeView> {
                ::std::boxed::Box::new(<Self as ::core::default::Default>::default())
            }
        }

        impl #impl_generics ::byard::render::NativeProps for #ident #ty_generics #where_clause {
            fn set_prop(&mut self, __name: &str, __value: &::byard::bridge::HostValue) {
                match __name {
                    #(
                        #prop_names => {
                            self.#prop_idents =
                                ::byard::bridge::FromHostValue::from_host(__value.clone());
                        }
                    )*
                    _ => {}
                }
            }
        }
    };
    TokenStream::from(expanded)
}

/// The roles `role = "…"` accepts, as the accessibility role each names
/// (RFC-0042 §2).
const ROLES: &[(&str, &str)] = &[
    ("button", "Button"),
    ("text_input", "TextInput"),
    ("switch", "Switch"),
    ("check_box", "CheckBox"),
    ("radio_button", "RadioButton"),
    ("slider", "Slider"),
    ("label", "Label"),
    ("image", "Image"),
    ("scroll_view", "ScrollView"),
    ("tab_list", "TabList"),
    ("tab", "Tab"),
    ("dialog", "Dialog"),
    ("group", "Group"),
];

/// Reads `name = "…"` and `role = "…"` off the attribute. The name defaults to
/// the struct's own; the role to nothing, which leaves the view a group.
fn view_args(
    attr: proc_macro2::TokenStream,
    input: &ItemStruct,
) -> syn::Result<(String, Option<syn::Ident>)> {
    use syn::parse::Parser as _;
    let mut name = input.ident.to_string();
    let mut role = None;
    let metas = syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated
        .parse2(attr)?;
    for meta in metas {
        let value = match &meta.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => s.value(),
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "the arguments take string literals",
                ));
            }
        };
        if meta.path.is_ident("name") {
            name = value;
        } else if meta.path.is_ident("role") {
            let Some((_, variant)) = ROLES.iter().find(|(word, _)| *word == value) else {
                let words: Vec<&str> = ROLES.iter().map(|(w, _)| *w).collect();
                return Err(syn::Error::new_spanned(
                    &meta.value,
                    format!("`role` is one of {}", words.join(", ")),
                ));
            };
            role = Some(syn::Ident::new(variant, proc_macro2::Span::call_site()));
        } else {
            return Err(syn::Error::new_spanned(
                meta.path,
                "the arguments are `name = \"ElementName\"` and `role = \"…\"`",
            ));
        }
    }
    Ok((name, role))
}

/// Maps a field's Rust type to the catalog's prop type.
///
/// A closed set, because a prop is written in `byld` and `byld` has a closed
/// set of things to write. A field of any other type is a compile error at the
/// declaration rather than a prop nobody can set.
fn prop_type(ty: &Type) -> Option<&'static str> {
    if let Type::Tuple(tuple) = ty {
        // `(f32, f32)` is a Vec2; any other tuple is not something `byld` can
        // hand over.
        return (tuple.elems.len() == 2
            && tuple
                .elems
                .iter()
                .all(|e| matches!(prop_type(e), Some("Float"))))
        .then_some("Vec2");
    }
    let Type::Path(path) = ty else { return None };
    let last = path.path.segments.last()?;
    match last.ident.to_string().as_str() {
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u64" | "usize" => Some("Int"),
        // A colour is the one `u32` a widget author writes, and `Color` is what
        // the language calls it, so the catalog says so and a hex literal
        // type-checks.
        "u32" => Some("Color"),
        "f32" | "f64" => Some("Float"),
        "bool" => Some("Bool"),
        "String" => Some("Str"),
        "Vec" => {
            // Only a list of numbers, which is what a chart series is. A list
            // of anything else has no `byld` spelling yet, and inventing one
            // here would be inventing it in the wrong place.
            let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
                return None;
            };
            let syn::GenericArgument::Type(inner) = args.args.first()? else {
                return None;
            };
            matches!(prop_type(inner), Some("Float" | "Int")).then_some("Floats")
        }
        _ => None,
    }
}
