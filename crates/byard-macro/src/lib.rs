//! Procedural macros for byard: `#[byard_controller]`, the boundary marker
//! (RFC-0028 §2, correcting erratum CB1), and `#[native_view]`, which declares
//! a package-authored widget (RFC-0039).
//!
//! `#[byard_controller]` has two forms, distinguished by what it annotates:
//!
//! - **On a `struct`** it emits the struct **unchanged** plus an inherent `impl`
//!   exposing field metadata (`BYARD_FIELDS` / `byard_field_type`) the LSP and
//!   interpreter use for type-directed member access. It does **not** by itself
//!   make the struct callable, that is the impl-block form below.
//! - **On an `impl` block** it emits the block unchanged plus
//!   `impl ::byard::bridge::Controller`: a `type_name` and an `invoke` that
//!   dispatches each method taking `&self`, `async` or not, by name,
//!   converting arguments from `HostValue`, running the method on the pool, and
//!   mapping `Ok`/`Err` back to `HostValue`.
//!
//! `#[derive(FromHostValue, IntoHostValue)]` (or `#[derive(HostValue)]`, both)
//! lets a struct with named fields cross the boundary as a record.
//!
//! The generated code references only `::byard::bridge::*` from the *app* crate's
//! dependency graph, so `byard-macro` keeps **no** dependency on
//! `byard-core`/`byard-compiler`. A controller struct must be `Clone`
//! (the shim clones `self` into the `'static` reply future).

use proc_macro::TokenStream;
use quote::quote;
use syn::ext::IdentExt as _;
use syn::{DeriveInput, Fields, Item, ReturnType, Type, parse_macro_input};

mod host_value;
mod names;
mod native_view;

/// Derives `::byard::bridge::FromHostValue` for a struct with named fields: a
/// `HostValue::Record` becomes the struct, each field read by name through its
/// own type's `from_host`. A field the record lacks, or a value that is not a
/// record, gives the field what its type makes of nothing (`0`, `""`, `None`),
/// as the conversions written by hand do. `#[host(rename = "…")]` on a field
/// names it differently in the record. Enums and tuple structs are a compile
/// error.
#[proc_macro_derive(FromHostValue, attributes(host))]
pub fn derive_from_host_value(input: TokenStream) -> TokenStream {
    derive(input, host_value::from_host_value)
}

/// Derives `::byard::bridge::IntoHostValue` for a struct with named fields: it
/// becomes a `HostValue::Record` with a key per field, in declaration order.
/// See [`FromHostValue`](macro@FromHostValue) for `#[host(rename = "…")]` and
/// for what is refused.
#[proc_macro_derive(IntoHostValue, attributes(host))]
pub fn derive_into_host_value(input: TokenStream) -> TokenStream {
    derive(input, host_value::into_host_value)
}

/// Derives both `FromHostValue` and `IntoHostValue`, which is what RFC-0028
/// and RFC-0029 call `#[derive(HostValue)]`.
#[proc_macro_derive(HostValue, attributes(host))]
pub fn derive_host_value(input: TokenStream) -> TokenStream {
    derive(input, host_value::host_value)
}

fn derive(
    input: TokenStream,
    expand: fn(&DeriveInput) -> syn::Result<proc_macro2::TokenStream>,
) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Marks a Rust struct as a native view (RFC-0039): a package-authored widget
/// that draws, measures and handles events like an intrinsic.
///
/// ```
/// # use byard::render::{Layout, NativeView, RenderCtx};
/// #[byard::native_view(name = "Sparkline")]
/// #[derive(Default)]
/// struct Sparkline {
///     #[prop] data: Vec<f32>,
///     #[prop(layout)] thickness: f32,
///     #[event] hover_point: (),
/// }
/// # impl NativeView for Sparkline {
/// #     fn render(&mut self, _layout: Layout, _cx: &mut RenderCtx<'_>) {}
/// # }
/// ```
///
/// `role = "button"` (or `slider`, `image`, `dialog`, …) says what a screen
/// reader should call the view (RFC-0042); without it the view is a group.
///
/// It emits the struct unchanged, a `Default`-based constructor, the catalog
/// entry the compiler validates `byld` against, and the `set_prop` half of
/// `NativeView` that assigns each declared field from the value the language
/// evaluated this tick. The `NativeView` impl itself stays the author's: what
/// the widget draws is the one thing a macro must not guess.
#[proc_macro_attribute]
pub fn native_view(attr: TokenStream, item: TokenStream) -> TokenStream {
    native_view::expand(attr, item)
}

/// Marks a Rust struct, or its `impl` block, as a byard controller (RFC-0028
/// §2). See the crate docs for the two forms. On a struct it emits
/// `BYARD_FIELDS` + `byard_field_type`; on an `impl` block it emits
/// `impl Controller` dispatching the block's methods that take `&self`.
#[proc_macro_attribute]
pub fn byard_controller(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as Item);
    match parsed {
        Item::Struct(s) => expand_struct(&Item::Struct(s)),
        Item::Impl(i) => expand_impl(&i).into(),
        other => syn::Error::new_spanned(
            other,
            "#[byard_controller] applies to a struct or an `impl` block",
        )
        .to_compile_error()
        .into(),
    }
}

/// Emits a controller struct unchanged plus its field-metadata inherent `impl`.
fn expand_struct(item: &Item) -> TokenStream {
    let Item::Struct(input) = item else {
        unreachable!("expand_struct only called for a struct");
    };
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // Collect (field name, byld type) for each named field. Tuple/unit structs
    // simply produce empty metadata (still valid).
    let mut entries = Vec::new();
    if let Fields::Named(named) = &input.fields {
        for field in &named.named {
            if let Some(ident) = &field.ident {
                let fname = ident.to_string();
                let bty = byld_type_name(&field.ty);
                entries.push((fname, bty));
            }
        }
    }

    let field_names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    let field_types: Vec<&str> = entries.iter().map(|(_, t)| t.as_str()).collect();

    let expanded = quote! {
        #input

        impl #impl_generics #name #ty_generics #where_clause {
            /// `(field name, byld type)` metadata for every named field, in
            /// declaration order. Consumed by the byard interpreter for
            /// type-directed `member` access (generated by `#[byard_controller]`).
            pub const BYARD_FIELDS: &'static [(&'static str, &'static str)] = &[
                #( ( #field_names, #field_types ) ),*
            ];

            /// The byld type name of one field, or `None` if no such field
            /// exists (generated by `#[byard_controller]`).
            #[must_use]
            pub fn byard_field_type(name: &str) -> ::core::option::Option<&'static str> {
                Self::BYARD_FIELDS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, t)| *t)
            }
        }
    };

    TokenStream::from(expanded)
}

/// What a function of a `#[byard_controller] impl` is to the macro.
enum Method {
    /// A controller method: a view can call it by name.
    Call,
    /// An ordinary associated function (`fn new() -> Self`, `fn with(self, …)`),
    /// left as written.
    Other,
}

/// Decides what `f` is, or why it cannot be either.
///
/// A method takes `&self`, `async` or not. A controller is cloned for each
/// call, so a method that needs `&mut self` could only change a copy that is
/// then dropped: that is refused rather than left to fail at run time with
/// "unknown controller method". An `async fn` is plainly meant to be a method,
/// so one that cannot be (no receiver, `self` by value) is refused too. A
/// synchronous function that is not a method is a constructor or a helper, and
/// stays what it is.
fn classify(f: &syn::ImplItemFn) -> syn::Result<Method> {
    let name = &f.sig.ident;
    let is_async = f.sig.asyncness.is_some();
    match f.sig.receiver() {
        Some(r) if r.reference.is_some() && r.mutability.is_none() => {
            if f.sig
                .generics
                .params
                .iter()
                .any(|p| !matches!(p, syn::GenericParam::Lifetime(_)))
            {
                return Err(syn::Error::new_spanned(
                    &f.sig.generics,
                    format!(
                        "the controller method `{name}` cannot be generic: a view calls it with \
                         values, and a value has one type"
                    ),
                ));
            }
            Ok(Method::Call)
        }
        Some(r) if r.reference.is_some() => Err(syn::Error::new_spanned(
            r,
            format!(
                "the controller method `{name}` takes `&mut self`, and a controller method \
                 cannot: each call runs on its own copy of the controller, so a change to it \
                 would be lost\n\
                 help: take `&self` and keep what must change behind an `Arc<Mutex<…>>`, or \
                 move `{name}` to an `impl` block that is not `#[byard_controller]`"
            ),
        )),
        _ if is_async => Err(syn::Error::new_spanned(
            name,
            format!(
                "`{name}` is `async` but does not take `&self`, so a view cannot call it: a \
                 controller method is `async fn {name}(&self, …)`\n\
                 help: make it a method, or drop `async` if it is a helper"
            ),
        )),
        _ => Ok(Method::Other),
    }
}

/// Emits an `impl` block unchanged plus `impl Controller` dispatching its
/// methods by name (RFC-0028 §2). Each method's arguments are converted from
/// `HostValue` via `FromHostValue`, the method runs on a clone of `self` (so
/// the reply future is `'static`), and its `Ok`/`Err` map back through
/// `IntoHostValue`. A synchronous method is called when the future is first
/// polled, which is on the pool and not on the logic thread.
fn expand_impl(input: &syn::ItemImpl) -> proc_macro2::TokenStream {
    let self_ty = &input.self_ty;

    // One match arm per method; what cannot be one is an error, reported
    // beside the untouched impl so nothing else of the person's code is lost.
    let mut arms = Vec::new();
    let mut errors: Option<syn::Error> = None;
    // (name in Byld, name in Rust) of each method taken so far, to refuse two
    // that would be one name, and to answer the Rust spelling with the Byld one.
    let mut taken: Vec<(String, String)> = Vec::new();
    let mut spelled: Vec<(String, String)> = Vec::new();
    for item in &input.items {
        let syn::ImplItem::Fn(f) = item else { continue };
        match classify(f) {
            Ok(Method::Call) => {}
            Ok(Method::Other) => continue,
            Err(e) => {
                refuse(&mut errors, e);
                continue;
            }
        }
        let ident = &f.sig.ident;
        let name_str = match method_name(f) {
            Ok(name) => name,
            Err(e) => {
                refuse(&mut errors, e);
                continue;
            }
        };
        let rust_name = ident.unraw().to_string();
        if let Some((_, other)) = taken.iter().find(|(public, _)| *public == name_str) {
            let e = syn::Error::new_spanned(
                ident,
                format!(
                    "`{rust_name}` and `{other}` are both `{name_str}` in a view\n\
                     help: name one of them with `#[byard(rename = \"…\")]`"
                ),
            );
            refuse(&mut errors, e);
            continue;
        }
        taken.push((name_str.clone(), rust_name.clone()));
        spelled.push((name_str.clone(), rust_name));

        let (binds, call_args) = bind_arguments(f);
        let dispatch = dispatch(f, &call_args);

        arms.push(quote! {
            #name_str => {
                let mut __args = __args.into_iter();
                #(#binds)*
                let __this = ::core::clone::Clone::clone(self);
                ::std::boxed::Box::pin(async move { #dispatch })
            }
        });
    }

    // A view that calls the Rust spelling is told the Byld one, rather than
    // left to wonder why a method it can see in the source is "unknown".
    let hints = rust_name_hints(&spelled);

    // `#[byard(rename = "…")]` is ours; the impl that comes out is ordinary Rust.
    let mut input = input.clone();
    for item in &mut input.items {
        if let syn::ImplItem::Fn(f) = item {
            f.attrs.retain(|a| !a.path().is_ident("byard"));
        }
    }

    let errors = errors.map(syn::Error::into_compile_error);
    quote! {
        #errors
        // A controller method is `async` because the boundary is async, not
        // because its body awaits: a method that computes its answer outright
        // is perfectly ordinary and the shim still has to hand back a future.
        // Allowed on the re-emitted impl so every caller of this macro gets
        // it, rather than each one rediscovering it in its own file.
        #[allow(clippy::unused_async_trait_impl)]
        #input

        impl ::byard::bridge::Controller for #self_ty {
            fn type_name(&self) -> &'static str {
                ::core::stringify!(#self_ty)
            }

            fn invoke(
                &self,
                method: &str,
                __args: ::std::vec::Vec<::byard::bridge::HostValue>,
            ) -> ::byard::bridge::BoxFuture<
                'static,
                ::core::result::Result<::byard::bridge::HostValue, ::byard::bridge::HostValue>,
            > {
                match method {
                    #(#arms)*
                    #(#hints)*
                    _ => {
                        let __m = method.to_string();
                        ::std::boxed::Box::pin(async move {
                            ::core::result::Result::Err(::byard::bridge::HostValue::Str(
                                ::std::format!("error[UnknownMethod]: unknown controller method `{__m}`")
                            ))
                        })
                    }
                }
            }
        }
    }
}

/// Binds each non-receiver parameter from the argument list, annotated with
/// its declared type so `FromHostValue::from_host` resolves.
fn bind_arguments(
    f: &syn::ImplItemFn,
) -> (Vec<proc_macro2::TokenStream>, Vec<proc_macro2::TokenStream>) {
    let mut binds = Vec::new();
    let mut call_args = Vec::new();
    let mut arg_index = 0usize;
    for input_arg in &f.sig.inputs {
        let syn::FnArg::Typed(pat) = input_arg else {
            continue; // skip `&self`
        };
        let ty = &pat.ty;
        let var = quote::format_ident!("__arg{arg_index}");
        binds.push(quote! {
            let #var: #ty = ::byard::bridge::FromHostValue::from_host(
                __args.next().unwrap_or(::byard::bridge::HostValue::Unit)
            );
        });
        call_args.push(quote! { #var });
        arg_index += 1;
    }

    (binds, call_args)
}

/// The call of `f` and the mapping of its result back to a `HostValue`.
fn dispatch(
    f: &syn::ImplItemFn,
    call_args: &[proc_macro2::TokenStream],
) -> proc_macro2::TokenStream {
    let ident = &f.sig.ident;
    let is_async = f.sig.asyncness.is_some();
    let call_expr = if is_async {
        quote! { __this.#ident(#(#call_args),*).await }
    } else {
        quote! { __this.#ident(#(#call_args),*) }
    };

    // A `-> Result<T, E>` maps ok/err separately; any other return type is
    // treated as an infallible success.
    let returns_result = matches!(&f.sig.output, ReturnType::Type(_, ty) if is_result(ty));
    if returns_result {
        quote! {
            match #call_expr {
                ::core::result::Result::Ok(__v) =>
                    ::core::result::Result::Ok(::byard::bridge::IntoHostValue::into_host(__v)),
                ::core::result::Result::Err(__e) =>
                    ::core::result::Result::Err(::byard::bridge::IntoHostValue::into_host(__e)),
            }
        }
    } else {
        quote! {
            ::core::result::Result::Ok(
                ::byard::bridge::IntoHostValue::into_host(#call_expr)
            )
        }
    }
}

/// For each method a view would call by a different name than Rust's, an arm
/// that answers the Rust spelling with the Byld one, rather than leaving the
/// author to wonder why a method visible in the source is "unknown".
fn rust_name_hints(spelled: &[(String, String)]) -> Vec<proc_macro2::TokenStream> {
    spelled
        .iter()
        .filter(|(public, rust)| public != rust && !spelled.iter().any(|(other, _)| other == rust))
        .map(|(public, rust)| {
            let message = format!(
                "error[UnknownMethod]: unknown controller method `{rust}`\n\
                 help: a controller method is called by its camelCase name in Byld: `{public}`"
            );
            quote! {
                #rust => ::std::boxed::Box::pin(async move {
                    ::core::result::Result::Err(::byard::bridge::HostValue::Str(
                        ::std::string::String::from(#message)
                    ))
                }),
            }
        })
        .collect()
}

/// Adds `e` to the errors found so far.
fn refuse(errors: &mut Option<syn::Error>, e: syn::Error) {
    match errors {
        Some(all) => all.combine(e),
        None => *errors = Some(e),
    }
}

/// The name a view calls `f` by: its `#[byard(rename = "…")]`, or its Rust name
/// in camelCase.
fn method_name(f: &syn::ImplItemFn) -> syn::Result<String> {
    let mut rename: Option<String> = None;
    for attr in f.attrs.iter().filter(|a| a.path().is_ident("byard")) {
        attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident("rename") {
                return Err(meta.error("unknown `byard` option; the only one is `rename = \"…\"`"));
            }
            if rename.is_some() {
                return Err(meta.error("`rename` is given twice"));
            }
            let name: syn::LitStr = meta.value()?.parse()?;
            if name.value().is_empty() {
                return Err(syn::Error::new(name.span(), "a method needs a name"));
            }
            rename = Some(name.value());
            Ok(())
        })?;
    }
    Ok(rename.unwrap_or_else(|| names::of_ident(&f.sig.ident)))
}

/// Whether a return type is syntactically a `Result<…>` (so `invoke` maps its
/// `Ok`/`Err` arms).
fn is_result(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Result"))
}

/// Maps a Rust field type to the byld type name the interpreter understands.
fn byld_type_name(ty: &Type) -> String {
    let ident = match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        Type::Reference(r) => return byld_type_name(&r.elem),
        _ => None,
    };
    match ident.as_deref() {
        Some(
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
            | "usize",
        ) => "Int".to_string(),
        Some("f32" | "f64") => "Float".to_string(),
        Some("bool") => "Bool".to_string(),
        Some("String" | "str") => "Str".to_string(),
        Some(other) => other.to_string(),
        None => "Unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn expansion(item: &syn::ItemImpl) -> String {
        expand_impl(item).to_string()
    }

    #[test]
    fn a_synchronous_method_taking_ref_self_is_dispatched() {
        let item: syn::ItemImpl = parse_quote! {
            impl Calc {
                fn double(&self, n: i64) -> i64 { n * 2 }
            }
        };
        let text = expansion(&item);
        assert!(text.contains("\"double\" =>"), "{text}");
        assert!(!text.contains(". await"), "{text}");
        assert!(!text.contains("compile_error"), "{text}");
    }

    #[test]
    fn an_async_method_is_awaited() {
        let item: syn::ItemImpl = parse_quote! {
            impl Calc { async fn slow(&self) -> i64 { 1 } }
        };
        assert!(expansion(&item).contains(". await"));
    }

    #[test]
    fn constructors_and_builders_are_left_alone() {
        let item: syn::ItemImpl = parse_quote! {
            impl Calc {
                fn new() -> Self { Calc }
                fn with_base(self, base: i64) -> Self { self }
                fn helper(&self) -> i64 { 1 }
            }
        };
        let text = expansion(&item);
        assert!(!text.contains("compile_error"), "{text}");
        assert!(!text.contains("\"new\""), "{text}");
        assert!(!text.contains("\"with_base\""), "{text}");
        assert!(text.contains("\"helper\""), "{text}");
    }

    fn refusal(item: &syn::ItemImpl) -> String {
        let text = expansion(item);
        assert!(text.contains("compile_error"), "should refuse: {text}");
        text
    }

    #[test]
    fn a_mutable_receiver_is_refused_and_says_why() {
        let text = refusal(&parse_quote! { impl C { fn bump(&mut self) {} } });
        assert!(text.contains("takes `&mut self`"), "{text}");
        assert!(text.contains("Arc<Mutex"), "{text}");
        let text = refusal(&parse_quote! { impl C { async fn bump(&mut self) {} } });
        assert!(text.contains("takes `&mut self`"), "{text}");
    }

    #[test]
    fn an_async_function_that_is_not_a_method_is_refused() {
        let text = refusal(&parse_quote! { impl C { async fn make() -> i64 { 1 } } });
        assert!(text.contains("does not take `&self`"), "{text}");
        let text = refusal(&parse_quote! { impl C { async fn eat(self) {} } });
        assert!(text.contains("does not take `&self`"), "{text}");
    }

    #[test]
    fn a_generic_method_is_refused() {
        let text = refusal(&parse_quote! { impl C { fn id<T>(&self, t: T) -> T { t } } });
        assert!(text.contains("cannot be generic"), "{text}");
    }

    #[test]
    fn every_problem_is_reported_and_the_impl_is_kept() {
        let text = refusal(&parse_quote! {
            impl C {
                fn a(&mut self) {}
                async fn b() {}
            }
        });
        assert!(text.matches("compile_error").count() >= 2, "{text}");
        // The person's code is still there for the compiler's other messages.
        assert!(text.contains("fn a (& mut self)"), "{text}");
    }
}
