//! `#[derive(NovaSchema)]` (spec §6, §11.3). Use it through `novafile-schema`
//! with the `derive` feature: `use novafile_schema::NovaSchema;`.
//!
//! Attributes — container: `#[nova(name = "…", version = "…", rename = "TypeName", open)]`;
//! field: `#[nova(rename = "…", key, id = N, range = "1..65535", len = "..64, utf8",
//! default = "flow literal", deprecated, skip)]`; variant: `#[nova(rename = "…")]`.
//! Serde attributes are honoured so the schema matches `novafile-serde`'s output:
//! `rename`, `rename_all`, `default`, `skip`, `skip_serializing`/`skip_deserializing`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Ts;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Error, Fields, LitInt, LitStr, parse_macro_input, spanned::Spanned};

#[proc_macro_derive(NovaSchema, attributes(nova))]
pub fn derive_nova_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input).unwrap_or_else(Error::into_compile_error).into()
}

#[derive(Default)]
struct Container {
    name: Option<String>,
    version: Option<String>,
    rename: Option<String>,
    rename_all: Option<String>,
    open: bool,
}

#[derive(Default)]
struct FieldAttrs {
    rename: Option<String>,
    key: bool,
    id: Option<u32>,
    range: Option<String>,
    len: Option<String>,
    default: Option<String>,
    serde_default: bool,
    deprecated: bool,
    skip: bool,
}

fn str_lit(meta: &syn::meta::ParseNestedMeta) -> syn::Result<String> {
    Ok(meta.value()?.parse::<LitStr>()?.value())
}

/// Skip `= value` / `(…)` of attributes we don't interpret.
fn ignore(meta: &syn::meta::ParseNestedMeta) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        meta.value()?.parse::<syn::Expr>()?;
    } else if meta.input.peek(syn::token::Paren) {
        let _content;
        syn::parenthesized!(_content in meta.input);
        let _: Ts = _content.parse()?;
    }
    Ok(())
}

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
                } else if p.is_ident("deprecated") {
                    f.deprecated = true;
                } else if p.is_ident("skip") {
                    f.skip = true;
                } else {
                    return Err(m.error("unknown #[nova] field attribute (rename, key, id, range, len, default, deprecated, skip)"));
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

/// Serde's `rename_all` rules, for snake_case fields and PascalCase variants.
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

/// Code building a `Vec<FieldDef>` from named fields.
fn record_fields(owner: &str, fields: &syn::FieldsNamed, rule: Option<&str>) -> syn::Result<Ts> {
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
        let ty = &f.ty;
        let at = format!("{owner}.{name}");
        let mut attr_text = String::new();
        if let Some(r) = &a.range {
            attr_text.push_str(&format!(" @range({r})"));
        }
        if let Some(l) = &a.len {
            attr_text.push_str(&format!(" @len({l})"));
        }
        let with = if attr_text.is_empty() {
            quote!()
        } else {
            quote!(let ty = ::novafile_schema::derive::__with_attrs(ty, #attr_text, #at);)
        };
        let default = match &a.default {
            Some(d) => quote!(Some(::novafile_schema::derive::__default(#d, #at))),
            None => quote!(None),
        };
        let has_default = a.default.is_some();
        let serde_default = a.serde_default;
        let key = a.key;
        let deprecated = a.deprecated;
        let id = match a.id {
            Some(n) => quote!(Some(#n)),
            None => quote!(None),
        };
        out.push(quote! {{
            let ty = <#ty as ::novafile_schema::NovaSchema>::nova_type(generator);
            #with
            let optional = !#has_default && (#serde_default || <#ty as ::novafile_schema::NovaSchema>::nova_field_optional());
            fields.push(::novafile_schema::FieldDef {
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
        let mut fields: Vec<::novafile_schema::FieldDef> = Vec::new();
        #(#out)*
        fields
    }})
}

fn expand(input: DeriveInput) -> syn::Result<Ts> {
    if !input.generics.params.is_empty() {
        return Err(Error::new(input.generics.span(), "NovaSchema cannot be derived for generic types yet"));
    }
    let ident = &input.ident;
    let c = container_attrs(&input.attrs)?;
    let def_name = c.rename.clone().unwrap_or_else(|| ident.to_string());
    let rule = c.rename_all.as_deref();
    let open = c.open;
    let body = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => {
                let fields = record_fields(&def_name, named, rule)?;
                quote!(::novafile_schema::Def::Record(::novafile_schema::RecordDef { fields: #fields, open: #open }))
            }
            Fields::Unnamed(u) if u.unnamed.len() == 1 => {
                // Newtype structs are transparent in serde: an alias of the inner type.
                let a = field_attrs(&u.unnamed[0].attrs)?;
                let ty = &u.unnamed[0].ty;
                let mut attr_text = String::new();
                if let Some(r) = &a.range {
                    attr_text.push_str(&format!(" @range({r})"));
                }
                if let Some(l) = &a.len {
                    attr_text.push_str(&format!(" @len({l})"));
                }
                let with = if attr_text.is_empty() {
                    quote!()
                } else {
                    quote!(let ty = ::novafile_schema::derive::__with_attrs(ty, #attr_text, #def_name);)
                };
                quote!({
                    let ty = <#ty as ::novafile_schema::NovaSchema>::nova_type(generator);
                    #with
                    ::novafile_schema::Def::Alias(ty)
                })
            }
            Fields::Unnamed(u) => {
                let tys = u.unnamed.iter().map(|f| &f.ty);
                quote!(::novafile_schema::Def::Alias(::novafile_schema::TypeRef::new(
                    ::novafile_schema::TypeKind::Tuple(vec![#(<#tys as ::novafile_schema::NovaSchema>::nova_type(generator)),*])
                )))
            }
            Fields::Unit => quote!(::novafile_schema::Def::Alias(::novafile_schema::TypeRef::new(
                ::novafile_schema::TypeKind::Null
            ))),
        },
        Data::Enum(e) => {
            let mut variants = Vec::new();
            for v in &e.variants {
                let a = field_attrs(&v.attrs)?;
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
                        quote!(Some(<#ty as ::novafile_schema::NovaSchema>::nova_type(generator)))
                    }
                    Fields::Unnamed(u) => {
                        let tys = u.unnamed.iter().map(|f| &f.ty);
                        quote!(Some(::novafile_schema::TypeRef::new(::novafile_schema::TypeKind::Tuple(
                            vec![#(<#tys as ::novafile_schema::NovaSchema>::nova_type(generator)),*]
                        ))))
                    }
                    Fields::Named(named) => {
                        // Struct variants get a record definition of their own.
                        let rec = format!("{def_name}{}", v.ident);
                        let fields = record_fields(&rec, named, None)?;
                        quote!(Some(generator.named(#rec, |generator| ::novafile_schema::Def::Record(::novafile_schema::RecordDef { fields: #fields, open: false }))))
                    }
                };
                variants.push(quote!(::novafile_schema::VariantDef { name: #name.to_owned(), payload: #payload }));
            }
            quote!(::novafile_schema::Def::Enum(vec![#(#variants),*]))
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
        impl ::novafile_schema::NovaSchema for #ident {
            fn nova_type(generator: &mut ::novafile_schema::derive::SchemaGen) -> ::novafile_schema::TypeRef {
                generator.named(#def_name, |generator| #body)
            }
            #id
        }
    })
}
