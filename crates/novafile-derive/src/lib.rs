//! `#[derive(NovaSchema)]` (spec §6, §11.3). Use it through the `novafile` umbrella crate
//! or `novafile-schema`, each with the `derive` feature: `use novafile::NovaSchema;` or
//! `use novafile_schema::NovaSchema;`. The generated code finds whichever of the two the
//! deriving crate depends on.
//!
//! The root type carries `#[nova(name = "…", version = "…")]`: that is the schema's identity
//! (`@schema name@version`), which documents reference and version checks compare. Without it the
//! derived schema is anonymous and cannot check a document's `@schema` line. Nested types need no
//! attribute.
//!
//! Attributes — container: `#[nova(name = "…", version = "…", rename = "TypeName", open)]`;
//! field: `#[nova(rename = "…", key, id = N, range = "1..65535", len = "..64, utf8",
//! default = "flow literal", secret, personal, type = "string @len(1..)", deprecated, skip)]`;
//! variant: `#[nova(rename = "…", id = N)]`.
//!
//! `secret` and `personal` classify the field's type (`@secret` / `@personal`, proposal 0009).
//! `type` replaces the field type's `NovaSchema` impl with a schema type expression, for
//! foreign types that have none (`url::Url`, `secrecy::SecretString`, …); on a field written
//! as `Option<…>` (not through an alias) it gives the inner type, which stays optional and
//! nullable. `range`, `len` and the class apply on top of it.
//!
//! The derived schema gets the checks a parsed `.novs` gets (references resolve, no alias
//! cycles, no class on a `@key` field); `novafile_schema()` panics if one fails.
//! Serde attributes are honoured so the schema matches `novafile-serde`'s output:
//! `rename`, `rename_all`, `default`, `skip`, `skip_serializing`/`skip_deserializing`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Ts;
use quote::quote;
use std::fmt::Write as _;
use syn::{Attribute, Data, DeriveInput, Error, Fields, LitInt, LitStr, parse_macro_input, spanned::Spanned};

/// Derives `NovaSchema`; see the crate documentation for the supported attributes.
#[proc_macro_derive(NovaSchema, attributes(nova))]
pub fn derive_nova_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input).unwrap_or_else(Error::into_compile_error).into()
}

/// The path generated code uses to reach `novafile-schema` from the crate deriving `NovaSchema`.
///
/// `::novafile_schema` when that crate depends on `novafile-schema` (under any name), or
/// `::novafile::schema` when it only depends on the `novafile` umbrella crate. Falls back to
/// `::novafile_schema` when neither is found in its manifest.
fn schema_crate() -> Ts {
    use proc_macro_crate::{FoundCrate, crate_name};

    let path = |name: &str| syn::Ident::new(&name.replace('-', "_"), proc_macro2::Span::call_site());
    match crate_name("novafile-schema") {
        Ok(FoundCrate::Name(name)) => {
            let name = path(&name);
            quote!(::#name)
        }

        // novafile-schema's own tests and examples link the library under its crate name.
        Ok(FoundCrate::Itself) | Err(_) => match crate_name("novafile") {
            Ok(FoundCrate::Name(name)) => {
                let name = path(&name);
                quote!(::#name::schema)
            }

            Ok(FoundCrate::Itself) => quote!(::novafile::schema),
            Err(_) => quote!(::novafile_schema),
        },
    }
}

/// Container-level settings, from `#[nova(...)]` and the relevant `#[serde(...)]` keys.
#[derive(Default)]
struct Container {
    /// Schema name for `nova_schema_id`; only valid together with `version`.
    name: Option<String>,
    /// Schema version for `nova_schema_id`; only valid together with `name`.
    version: Option<String>,
    /// Definition name to use instead of the Rust type name.
    rename: Option<String>,
    /// Serde-style `rename_all` rule applied to field and variant names.
    rename_all: Option<String>,
    /// Emit the record as `@open`, accepting unknown fields.
    open: bool,
}

/// Settings of a field or enum variant, from `#[nova(...)]`, `#[serde(...)]` and `#[deprecated]`.
#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per independent attribute")]
struct FieldAttrs {
    /// Explicit NOVA name; `#[nova(rename)]` wins over `#[serde(rename)]`.
    rename: Option<String>,
    /// Mark the field as `@key`.
    key: bool,
    /// Field id for `@id(N)`.
    id: Option<u32>,
    /// Body of an `@range(...)` constraint, inserted verbatim.
    range: Option<String>,
    /// Body of an `@len(...)` constraint, inserted verbatim.
    len: Option<String>,
    /// Class attribute name (`secret` or `personal`), emitted as `@secret` / `@personal`.
    class: Option<&'static str>,
    /// Schema type expression used instead of the field type's `NovaSchema` impl.
    ty: Option<String>,
    /// Default value as a NOVA flow literal; makes the field non-optional.
    default: Option<String>,
    /// `#[serde(default)]` is present, so the field may be absent (optional).
    serde_default: bool,
    /// Mark the field as `@deprecated`.
    deprecated: bool,
    /// Leave the field or variant out of the schema, as serde does.
    skip: bool,
}

/// The string literal of a `key = "..."` attribute.
fn str_lit(meta: &syn::meta::ParseNestedMeta) -> syn::Result<String> {
    Ok(meta.value()?.parse::<LitStr>()?.value())
}

/// Skip `= value` / `(…)` of attributes we don't interpret.
fn ignore(meta: &syn::meta::ParseNestedMeta) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        meta.value()?.parse::<syn::Expr>()?;
    } else if meta.input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in meta.input);
        let _: Ts = content.parse()?;
    }

    Ok(())
}

/// Read the container attributes. Unknown `#[nova]` keys and serde enum
/// representations other than external tagging are errors; other serde keys are ignored.
fn container_attrs(attrs: &[Attribute]) -> syn::Result<Container> {
    let mut c = Container::default();
    for a in attrs {
        if a.path().is_ident("nova") {
            a.parse_nested_meta(|m| {
                if m.path.is_ident("name") {
                    c.name = Some(str_lit(&m)?);
                } else if m.path.is_ident("version") {
                    c.version = Some(str_lit(&m)?);
                } else if m.path.is_ident("rename") {
                    c.rename = Some(str_lit(&m)?);
                } else if m.path.is_ident("rename_all") {
                    c.rename_all = Some(str_lit(&m)?);
                } else if m.path.is_ident("open") {
                    c.open = true;
                } else {
                    return Err(m.error("unknown #[nova] container attribute (name, version, rename, rename_all, open)"));
                }

                Ok(())
            })?;
        } else if a.path().is_ident("serde") {
            a.parse_nested_meta(|m| {
                if m.path.is_ident("rename_all") {
                    if m.input.peek(syn::Token![=]) {
                        c.rename_all = Some(str_lit(&m)?);
                    } else {
                        ignore(&m)?;
                    }
                } else if m.path.is_ident("tag") || m.path.is_ident("untagged") || m.path.is_ident("content") {
                    return Err(m.error("NovaSchema supports externally tagged enums only (they map to NOVA variants)"));
                } else {
                    ignore(&m)?;
                }

                Ok(())
            })?;
        }
    }

    Ok(c)
}

/// Read the attributes of a field or variant. Unknown `#[nova]` keys and
/// `#[serde(flatten)]` are errors; other serde keys are ignored.
fn field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttrs> {
    let mut f = FieldAttrs::default();
    for a in attrs {
        if a.path().is_ident("deprecated") {
            f.deprecated = true;
        } else if a.path().is_ident("nova") {
            a.parse_nested_meta(|m| {
                let p = &m.path;
                if p.is_ident("rename") {
                    f.rename = Some(str_lit(&m)?);
                } else if p.is_ident("key") {
                    f.key = true;
                } else if p.is_ident("id") {
                    f.id = Some(m.value()?.parse::<LitInt>()?.base10_parse()?);
                } else if p.is_ident("range") {
                    f.range = Some(str_lit(&m)?);
                } else if p.is_ident("len") {
                    f.len = Some(str_lit(&m)?);
                } else if p.is_ident("default") {
                    f.default = Some(str_lit(&m)?);
                } else if p.is_ident("secret") || p.is_ident("personal") {
                    if f.class.is_some() {
                        return Err(m.error("a field has at most one class (secret or personal)"));
                    }

                    f.class = Some(if p.is_ident("secret") { "secret" } else { "personal" });
                } else if p.is_ident("type") {
                    f.ty = Some(str_lit(&m)?);
                } else if p.is_ident("deprecated") {
                    f.deprecated = true;
                } else if p.is_ident("skip") {
                    f.skip = true;
                } else {
                    return Err(m.error("unknown #[nova] field attribute (rename, key, id, range, len, default, secret, personal, type, deprecated, skip)"));
                }

                Ok(())
            })?;
        } else if a.path().is_ident("serde") {
            a.parse_nested_meta(|m| {
                let p = &m.path;
                if p.is_ident("rename") && m.input.peek(syn::Token![=]) {
                    if f.rename.is_none() {
                        f.rename = Some(str_lit(&m)?);
                    }
                } else if p.is_ident("default") {
                    f.serde_default = true;
                    ignore(&m)?;
                } else if p.is_ident("skip") {
                    f.skip = true;
                } else if p.is_ident("flatten") {
                    return Err(m.error("NovaSchema does not support #[serde(flatten)]"));
                } else {
                    ignore(&m)?;
                }

                Ok(())
            })?;
        }
    }

    Ok(f)
}

/// Serde's `rename_all` rules, for `snake_case` fields and `PascalCase` variants.
fn rename(name: &str, rule: Option<&str>, is_variant: bool) -> syn::Result<String> {
    let Some(rule) = rule else { return Ok(name.to_owned()) };
    let words: Vec<String> = if is_variant {
        let mut w = Vec::new();
        let mut cur = String::new();
        for c in name.chars() {
            if c.is_uppercase() && !cur.is_empty() {
                w.push(std::mem::take(&mut cur));
            }

            cur.extend(c.to_lowercase());
        }

        if !cur.is_empty() {
            w.push(cur);
        }

        w
    } else {
        name.split('_').filter(|s| !s.is_empty()).map(str::to_lowercase).collect()
    };

    let cap = |s: &str| {
        let mut c = s.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    };

    Ok(match rule {
        "lowercase" => words.concat(),
        "UPPERCASE" => words.concat().to_uppercase(),
        "PascalCase" => words.iter().map(|w| cap(w)).collect(),
        "camelCase" => words.iter().enumerate().map(|(i, w)| if i == 0 { w.clone() } else { cap(w) }).collect(),
        "snake_case" => words.join("_"),
        "SCREAMING_SNAKE_CASE" => words.join("_").to_uppercase(),
        "kebab-case" => words.join("-"),
        "SCREAMING-KEBAB-CASE" => words.join("-").to_uppercase(),
        other => return Err(Error::new(proc_macro2::Span::call_site(), format!("unknown rename_all rule {other:?}"))),
    })
}

/// Whether `ty` is written as `Option<…>` (any path ending in `Option`).
fn is_option(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

/// Code binding `ty` to the schema type of a field of Rust type `rust_ty`, and an expression
/// telling whether the field may be absent. Honours `type`, `range`, `len` and the class.
fn field_type(krate: &Ts, a: &FieldAttrs, rust_ty: &syn::Type, at: &str) -> (Ts, Ts) {
    let mut attr_text = String::new();
    if let Some(r) = &a.range {
        let _ = write!(attr_text, " @range({r})");
    }

    if let Some(l) = &a.len {
        let _ = write!(attr_text, " @len({l})");
    }

    if let Some(c) = a.class {
        let _ = write!(attr_text, " @{c}");
    }

    let with = if attr_text.is_empty() {
        quote!()
    } else {
        quote!(let ty = #krate::derive::__with_attrs(ty, #attr_text, #at);)
    };

    // With `type`, never name `<rust_ty as NovaSchema>`: the point is that it has no impl.
    let (base, optional) = match &a.ty {
        Some(t) => {
            let option = is_option(rust_ty);
            (quote!(#krate::derive::__type(#t, #at, #option)), quote!(#option))
        }
        None => (
            quote!(<#rust_ty as #krate::NovaSchema>::nova_type(generator)),
            quote!(<#rust_ty as #krate::NovaSchema>::nova_field_optional()),
        ),
    };

    (quote!(let ty = #base; #with), optional)
}

/// Code building a `Vec<FieldDef>` from named fields.
fn record_fields(krate: &Ts, owner: &str, fields: &syn::FieldsNamed, rule: Option<&str>) -> syn::Result<Ts> {
    let mut out = Vec::new();
    for f in &fields.named {
        let a = field_attrs(&f.attrs)?;
        if a.skip {
            continue;
        }

        let ident = f.ident.as_ref().unwrap().to_string();
        let ident = ident.strip_prefix("r#").unwrap_or(&ident).to_owned();
        let name = match &a.rename {
            Some(r) => r.clone(),
            None => rename(&ident, rule, false)?,
        };

        if a.key && a.class.is_some() {
            return Err(Error::new(f.span(), "a @key field cannot be secret or personal, since keys appear in paths"));
        }

        let at = format!("{owner}.{name}");
        let (ty, field_optional) = field_type(krate, &a, &f.ty, &at);

        let default = if let Some(d) = &a.default {
            quote!(Some(#krate::derive::__default(#d, #at)))
        } else {
            quote!(None)
        };

        let has_default = a.default.is_some();
        let serde_default = a.serde_default;
        let key = a.key;
        let deprecated = a.deprecated;
        let id = if let Some(n) = a.id { quote!(Some(#n)) } else { quote!(None) };
        out.push(quote! {{
            #ty
            let optional = !#has_default && (#serde_default || #field_optional);
            fields.push(#krate::FieldDef {
                name: #name.to_owned(),
                ty,
                optional,
                default: #default,
                key: #key,
                id: #id,
                deprecated: #deprecated,
            });
        }});
    }

    Ok(quote! {{
        let mut fields: Vec<#krate::FieldDef> = Vec::new();
        #(#out)*
        fields
    }})
}

/// Generate the `NovaSchema` impl: the type registers itself as a named
/// definition, and `nova_schema_id` is emitted when name and version are given.
fn expand(input: &DeriveInput) -> syn::Result<Ts> {
    if !input.generics.params.is_empty() {
        return Err(Error::new(input.generics.span(), "NovaSchema cannot be derived for generic types yet"));
    }

    let krate = schema_crate();
    let ident = &input.ident;
    let c = container_attrs(&input.attrs)?;
    let def_name = c.rename.clone().unwrap_or_else(|| ident.to_string());
    let rule = c.rename_all.as_deref();
    let open = c.open;
    let body = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => {
                let fields = record_fields(&krate, &def_name, named, rule)?;
                quote!(#krate::Def::Record(#krate::RecordDef { fields: #fields, open: #open }))
            }
            Fields::Unnamed(u) if u.unnamed.len() == 1 => {
                // Newtype structs are transparent in serde: an alias of the inner type.
                let a = field_attrs(&u.unnamed[0].attrs)?;
                let (ty, _) = field_type(&krate, &a, &u.unnamed[0].ty, &def_name);
                quote!({
                    #ty
                    #krate::Def::Alias(ty)
                })
            }
            Fields::Unnamed(u) => {
                let tys = u.unnamed.iter().map(|f| &f.ty);
                quote!(#krate::Def::Alias(#krate::TypeRef::new(
                    #krate::TypeKind::Tuple(vec![#(<#tys as #krate::NovaSchema>::nova_type(generator)),*])
                )))
            }
            Fields::Unit => quote!(#krate::Def::Alias(#krate::TypeRef::new(
                #krate::TypeKind::Null
            ))),
        },
        Data::Enum(e) => {
            let mut variants = Vec::new();
            for v in &e.variants {
                let a = field_attrs(&v.attrs)?;
                if a.class.is_some() || a.ty.is_some() {
                    return Err(Error::new(v.span(), "secret, personal and type apply to fields, not enum variants"));
                }

                if a.skip {
                    continue;
                }

                let name = match &a.rename {
                    Some(r) => r.clone(),
                    None => rename(&v.ident.to_string(), rule, true)?,
                };

                let payload = match &v.fields {
                    Fields::Unit => quote!(None),
                    Fields::Unnamed(u) if u.unnamed.len() == 1 => {
                        let ty = &u.unnamed[0].ty;
                        quote!(Some(<#ty as #krate::NovaSchema>::nova_type(generator)))
                    }
                    Fields::Unnamed(u) => {
                        let tys = u.unnamed.iter().map(|f| &f.ty);
                        quote!(Some(#krate::TypeRef::new(#krate::TypeKind::Tuple(
                            vec![#(<#tys as #krate::NovaSchema>::nova_type(generator)),*]
                        ))))
                    }
                    Fields::Named(named) => {
                        // Struct variants get a record definition of their own.
                        let rec = format!("{def_name}{}", v.ident);
                        let fields = record_fields(&krate, &rec, named, None)?;
                        quote!(Some(generator.named(#rec, |generator| #krate::Def::Record(#krate::RecordDef { fields: #fields, open: false }))))
                    }
                };

                let id = if let Some(n) = a.id { quote!(Some(#n)) } else { quote!(None) };
                variants.push(quote!(#krate::VariantDef { name: #name.to_owned(), payload: #payload, id: #id }));
            }

            quote!(#krate::Def::Enum(vec![#(#variants),*]))
        }
        Data::Union(u) => return Err(Error::new(u.union_token.span, "NovaSchema cannot be derived for unions")),
    };

    let id = match (&c.name, &c.version) {
        (Some(n), Some(v)) => quote! {
            fn nova_schema_id() -> Option<(&'static str, &'static str)> {
                Some((#n, #v))
            }
        },
        (None, None) => quote!(),
        _ => return Err(Error::new(ident.span(), "#[nova(name = …)] and #[nova(version = …)] go together")),
    };

    Ok(quote! {
        impl #krate::NovaSchema for #ident {
            fn nova_type(generator: &mut #krate::derive::SchemaGen) -> #krate::TypeRef {
                generator.named(#def_name, |generator| #body)
            }

            #id
        }
    })
}
