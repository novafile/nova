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
//! Attributes — container: `#[nova(name = "…", version = "…", import = "name@version as alias", rename = "TypeName", rename_all = "…",
//! rename_all_fields = "…", open, doc = "…", together = "a, b", one_of = "a, b", any_of = "a, b")]`;
//! field: `#[nova(rename = "…", key, id = N, range = "1..65535", len = "..64, utf8",
//! default = "flow literal", secret, personal, type = "string @len(1..)", not_null, deprecated, skip,
//! doc = "…")]`;
//! variant: `#[nova(rename = "…", id = N, rename_all = "…", doc = "…")]`.
//!
//! Doc comments (`///`) on the type, its fields and its variants become the documentation of
//! the definitions, fields and variants: `#` comments in the schema text, hover text in
//! editors and `description` in JSON Schema. `doc = "…"` replaces that text, for example to
//! leave out Rust intra-doc links; `doc = ""` gives no documentation.
//!
//! `together`, `one_of` and `any_of` on a struct add record rules (proposal 0010): all or none,
//! exactly one, or at least one of the named fields is present (not `null`). They name Rust
//! fields, which must be `Option<…>` or `#[serde(default)]` and have no `default`; each may
//! be given more than once. Partial validation of a configuration layer skips them.
//!
//! The standard library `novafile.dev/std` (proposal 0008) is available as `std`: a
//! `type = "std.Uri @pattern(\"https://.*\")"` refines one of its types, and the derived schema
//! then carries `@import novafile.dev/std@… as std`, the newest bundled version. `import` on the
//! root type adds an `@import` line of its own, to pin that version
//! (`import = "novafile.dev/std@0.1.0 as std"`); it may be given more than once, and only bundled
//! schemas can be imported.
//!
//! `secret` and `personal` classify the field's type (`@secret` / `@personal`, proposal 0009).
//! `type` replaces the field type's `NovaSchema` impl with a schema type expression, for
//! foreign types that have none (`secrecy::SecretString`, …); on a field written
//! as `Option<…>` (not through an alias) it gives the inner type, which stays optional and
//! nullable. `range`, `len` and the class apply on top of it. The `novafile-schema` features
//! `url`, `uuid` and `semver` give `url::Url`, `uuid::Uuid` and `semver::Version` an impl that
//! derives `std.Uri`, `std.Uuid` and `std.SemVer`. `std.Uri` follows RFC 3986 and is stricter
//! than the WHATWG parsing of `url::Url` (unencoded spaces, Unicode hosts), and `std.Uuid` takes
//! lower case only; what those types write always passes.
//!
//! An `Option<…>` field derives `name?: T?`: it may be absent, and it may be `null`.
//! `not_null` keeps the field optional but rejects `null`, so it derives `name?: T`. Serde
//! still reads a missing key as `None`. `novafile-serde` writes `None` as `null`, so give such
//! a field `#[serde(skip_serializing_if = "Option::is_none")]` when its serialized value
//! must validate.
//!
//! The derived schema gets the checks a parsed `.novs` gets (references resolve, no alias
//! cycles, no class on a `@key` field); `novafile_schema()` panics if one fails.
//! Serde attributes are honoured so the schema matches `novafile-serde`'s output:
//! `rename`, `rename_all` (also on variants), `rename_all_fields`, `default`, `skip`,
//! `skip_serializing`/`skip_deserializing`. Rename rules change names exactly as serde does.

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
    /// Serde-style `rename_all_fields` rule: the `rename_all` of every struct variant
    /// that has none of its own.
    rename_all_fields: Option<String>,
    /// Emit the record as `@open`, accepting unknown fields.
    open: bool,
    /// The documentation of the definition: `#[nova(doc)]`, else the doc comments.
    doc: Option<String>,
    /// Record rules: the rule name (`together`, `one_of`, `any_of`) and the Rust
    /// field names it lists, in the order written.
    rules: Vec<(String, Vec<String>, proc_macro2::Span)>,
    /// `@import` arguments (`name@version as alias`), in the order written.
    imports: Vec<String>,
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
    /// Reject `null`: the field's type is not nullable, even for an `Option<…>` field.
    not_null: bool,
    /// Mark the field as `@deprecated`.
    deprecated: bool,
    /// Leave the field or variant out of the schema, as serde does.
    skip: bool,
    /// On a struct variant: the `rename_all` rule for its fields.
    rename_all: Option<String>,
    /// The documentation: `#[nova(doc)]`, else the doc comments.
    doc: Option<String>,
}

/// The text of the doc comments (`#[doc = "…"]`) in `attrs`, or `None` if there are none.
/// Each line loses one leading space, which `/// text` adds. Doc attributes that are not
/// string literals (`#[doc = include_str!(…)]`) are left out.
fn doc_comments(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for a in attrs.iter().filter(|a| a.path().is_ident("doc")) {
        if let syn::Meta::NameValue(nv) = &a.meta
            && let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value
        {
            let text = s.value();
            lines.extend(text.lines().map(|l| l.strip_prefix(' ').unwrap_or(l).trim_end().to_owned()));
        }
    }

    non_empty(&lines.join("\n"))
}

/// `text` without leading and trailing blank lines, or `None` if nothing is left.
fn non_empty(text: &str) -> Option<String> {
    let text = text.trim_matches('\n');
    (!text.trim().is_empty()).then(|| text.to_owned())
}

/// The documentation from `#[nova(doc = "…")]` if given, else from the doc comments.
fn doc_of(attrs: &[Attribute], nova_doc: Option<String>) -> Option<String> {
    match nova_doc {
        Some(d) => non_empty(&d),
        None => doc_comments(attrs),
    }
}

/// Code for an `Option<String>` holding `doc`.
fn doc_tokens(doc: Option<&str>) -> Ts {
    if let Some(d) = doc { quote!(Some(#d.to_owned())) } else { quote!(None) }
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
    let mut nova_doc = None;
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
                } else if m.path.is_ident("rename_all_fields") {
                    c.rename_all_fields = Some(str_lit(&m)?);
                } else if m.path.is_ident("open") {
                    c.open = true;
                } else if m.path.is_ident("doc") {
                    nova_doc = Some(str_lit(&m)?);
                } else if m.path.is_ident("import") {
                    c.imports.push(str_lit(&m)?);
                } else if let Some(rule) = ["together", "one_of", "any_of"].into_iter().find(|r| m.path.is_ident(r)) {
                    let span = m.path.span();
                    let fields = str_lit(&m)?.split(',').map(|f| f.trim().to_owned()).collect();
                    c.rules.push((rule.to_owned(), fields, span));
                } else {
                    return Err(m.error(
                        "unknown #[nova] container attribute (name, version, import, rename, rename_all, rename_all_fields, open, doc, together, one_of, any_of)",
                    ));
                }

                Ok(())
            })?;
        } else if a.path().is_ident("serde") {
            a.parse_nested_meta(|m| {
                if m.path.is_ident("rename_all") && m.input.peek(syn::Token![=]) {
                    c.rename_all = Some(str_lit(&m)?);
                } else if m.path.is_ident("rename_all_fields") && m.input.peek(syn::Token![=]) {
                    c.rename_all_fields = Some(str_lit(&m)?);
                } else if m.path.is_ident("tag") || m.path.is_ident("untagged") || m.path.is_ident("content") {
                    return Err(m.error("NovaSchema supports externally tagged enums only (they map to NOVA variants)"));
                } else {
                    ignore(&m)?;
                }

                Ok(())
            })?;
        }
    }

    c.doc = doc_of(attrs, nova_doc);
    Ok(c)
}

/// Read the attributes of a field or variant. Unknown `#[nova]` keys and
/// `#[serde(flatten)]` are errors; other serde keys are ignored.
fn field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttrs> {
    let mut f = FieldAttrs::default();
    let mut nova_doc = None;
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
                } else if p.is_ident("not_null") {
                    f.not_null = true;
                } else if p.is_ident("deprecated") {
                    f.deprecated = true;
                } else if p.is_ident("skip") {
                    f.skip = true;
                } else if p.is_ident("rename_all") {
                    f.rename_all = Some(str_lit(&m)?);
                } else if p.is_ident("doc") {
                    nova_doc = Some(str_lit(&m)?);
                } else {
                    return Err(m.error(
                        "unknown #[nova] field attribute (rename, key, id, range, len, default, secret, personal, type, not_null, deprecated, skip, rename_all, doc)",
                    ));
                }

                Ok(())
            })?;
        } else if a.path().is_ident("serde") {
            a.parse_nested_meta(|m| {
                let p = &m.path;
                if p.is_ident("rename") && m.input.peek(syn::Token![=]) {
                    // Read the value even when `#[nova(rename)]` wins, so the next key parses.
                    let r = str_lit(&m)?;
                    f.rename.get_or_insert(r);
                } else if p.is_ident("rename_all") && m.input.peek(syn::Token![=]) {
                    let r = str_lit(&m)?;
                    f.rename_all.get_or_insert(r);
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

    f.doc = doc_of(attrs, nova_doc);
    Ok(f)
}

/// Serde's `rename_all` rules, applied exactly as serde applies them.
///
/// Serde does not split names into words. It assumes `snake_case` fields and
/// `PascalCase` variants and changes characters in place, so `user_name` under
/// `lowercase` stays `user_name`. Copying that keeps the schema on serde's output.
fn rename(name: &str, rule: Option<&str>, is_variant: bool) -> syn::Result<String> {
    let Some(rule) = rule else { return Ok(name.to_owned()) };
    let renamed = if is_variant { rename_variant(name, rule) } else { rename_field(name, rule) };
    renamed.ok_or_else(|| Error::new(proc_macro2::Span::call_site(), format!("unknown rename_all rule {rule:?}")))
}

/// Serde's `RenameRule::apply_to_variant` for a `PascalCase` variant name, or `None` for an unknown rule.
fn rename_variant(name: &str, rule: &str) -> Option<String> {
    let snake = || {
        let mut out = String::new();
        for (i, c) in name.char_indices() {
            if i > 0 && c.is_uppercase() {
                out.push('_');
            }

            out.push(c.to_ascii_lowercase());
        }

        out
    };

    Some(match rule {
        "PascalCase" => name.to_owned(),
        "lowercase" => name.to_ascii_lowercase(),
        "UPPERCASE" => name.to_ascii_uppercase(),
        "camelCase" => lower_first(name),
        "snake_case" => snake(),
        "SCREAMING_SNAKE_CASE" => snake().to_ascii_uppercase(),
        "kebab-case" => snake().replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => snake().to_ascii_uppercase().replace('_', "-"),
        _ => return None,
    })
}

/// Serde's `RenameRule::apply_to_field` for a `snake_case` field name, or `None` for an unknown rule.
fn rename_field(name: &str, rule: &str) -> Option<String> {
    let pascal = || {
        let mut out = String::new();
        let mut capitalize = true;
        for c in name.chars() {
            if c == '_' {
                capitalize = true;
            } else if capitalize {
                out.push(c.to_ascii_uppercase());
                capitalize = false;
            } else {
                out.push(c);
            }
        }

        out
    };

    Some(match rule {
        "lowercase" | "snake_case" => name.to_owned(),
        "UPPERCASE" | "SCREAMING_SNAKE_CASE" => name.to_ascii_uppercase(),
        "PascalCase" => pascal(),
        "camelCase" => lower_first(&pascal()),
        "kebab-case" => name.replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => name.to_ascii_uppercase().replace('_', "-"),
        _ => return None,
    })
}

/// `s` with its first character in ASCII lowercase, as serde's `camelCase` does.
fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_ascii_lowercase().to_string() + c.as_str()).unwrap_or_default()
}

/// Whether `ty` is written as `Option<…>` (any path ending in `Option`).
fn is_option(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

/// Code binding `ty` to the schema type of a field of Rust type `rust_ty`, and an expression
/// telling whether the field may be absent. Honours `type`, `range`, `len`, the class and `not_null`.
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

    let not_null = if a.not_null {
        quote!(let ty = #krate::TypeRef { nullable: false, ..ty };)
    } else {
        quote!()
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

    (quote!(let ty = #base; #with #not_null), optional)
}

/// Code building a `Vec<FieldDef>` from named fields, and the schema name of each
/// field that is not skipped, by Rust field name.
fn record_fields(krate: &Ts, owner: &str, fields: &syn::FieldsNamed, rule: Option<&str>) -> syn::Result<(Ts, Vec<(String, String)>)> {
    let mut out = Vec::new();
    let mut names = Vec::new();
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

        names.push((ident, name.clone()));
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
        let doc = doc_tokens(a.doc.as_deref());
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
                doc: #doc,
            });
        }});
    }

    let code = quote! {{
        let mut fields: Vec<#krate::FieldDef> = Vec::new();
        #(#out)*
        fields
    }};

    Ok((code, names))
}

/// Code building the `Vec<RecordRule>` of a struct's `together`, `one_of` and
/// `any_of` attributes. Each Rust field name becomes its schema name; a name that
/// is not a field, or a skipped one, is an error. `novafile_schema()` rejects rules
/// that name required fields or fields with a default.
fn record_rules(krate: &Ts, c: &Container, names: &[(String, String)]) -> syn::Result<Ts> {
    let mut rules = Vec::new();
    for (rule, fields, span) in &c.rules {
        let mut mapped = Vec::new();
        for f in fields {
            let (_, name) = names
                .iter()
                .find(|(ident, _)| ident == f)
                .ok_or_else(|| Error::new(*span, format!("#[nova({rule})]: {f:?} is not a field of this struct")))?;
            mapped.push(name.clone());
        }

        let kind = match rule.as_str() {
            "together" => quote!(#krate::RuleKind::Together),
            "one_of" => quote!(#krate::RuleKind::OneOf),
            _ => quote!(#krate::RuleKind::AnyOf),
        };
        rules.push(quote!(#krate::RecordRule { kind: #kind, fields: vec![#(#mapped.to_owned()),*] }));
    }

    Ok(quote!(vec![#(#rules),*]))
}

/// Code building the `Def::Enum` of an enum: one variant per Rust variant that is not
/// skipped. Struct variants get a record definition of their own, `{def_name}{Variant}`.
fn enum_def(krate: &Ts, c: &Container, def_name: &str, rule: Option<&str>, e: &syn::DataEnum) -> syn::Result<Ts> {
    let mut variants = Vec::new();
    for v in &e.variants {
        let a = field_attrs(&v.attrs)?;
        if a.class.is_some() || a.ty.is_some() || a.not_null {
            return Err(Error::new(v.span(), "secret, personal, type and not_null apply to fields, not enum variants"));
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
                let vname = v.ident.to_string();
                let field_rule = a.rename_all.as_deref().or(c.rename_all_fields.as_deref());
                let (fields, _) = record_fields(krate, &rec, named, field_rule)?;
                quote!(Some(generator.named(#rec, &::std::format!("{}::{}", ::std::any::type_name::<Self>(), #vname), |generator| #krate::Def::Record(#krate::RecordDef { fields: #fields, open: false, rules: ::std::vec::Vec::new() }))))
            }
        };

        let id = if let Some(n) = a.id { quote!(Some(#n)) } else { quote!(None) };
        let doc = doc_tokens(a.doc.as_deref());
        variants.push(quote!(#krate::VariantDef { name: #name.to_owned(), payload: #payload, id: #id, doc: #doc }));
    }

    Ok(quote!(#krate::Def::Enum(vec![#(#variants),*])))
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
                let (fields, field_names) = record_fields(&krate, &def_name, named, rule)?;
                let rules = record_rules(&krate, &c, &field_names)?;
                quote!(#krate::Def::Record(#krate::RecordDef { fields: #fields, open: #open, rules: #rules }))
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
        Data::Enum(e) => enum_def(&krate, &c, &def_name, rule, e)?,
        Data::Union(u) => return Err(Error::new(u.union_token.span, "NovaSchema cannot be derived for unions")),
    };

    let doc = match &c.doc {
        Some(d) => quote!(generator.doc(#def_name, #d);),
        None => quote!(),
    };

    if let (Some((rule, _, span)), false) = (c.rules.first(), matches!(&input.data, Data::Struct(s) if matches!(s.fields, Fields::Named(_)))) {
        return Err(Error::new(*span, format!("#[nova({rule})] applies to structs with named fields")));
    }

    let id = match (&c.name, &c.version) {
        (Some(n), Some(v)) => quote! {
            fn nova_schema_id() -> Option<(&'static str, &'static str)> {
                Some((#n, #v))
            }
        },
        (None, None) => quote!(),
        _ => return Err(Error::new(ident.span(), "#[nova(name = …)] and #[nova(version = …)] go together")),
    };

    let imports = &c.imports;
    let imports = if imports.is_empty() {
        quote!()
    } else {
        quote! {
            fn nova_imports() -> &'static [&'static str] {
                &[#(#imports),*]
            }
        }
    };

    Ok(quote! {
        impl #krate::NovaSchema for #ident {
            fn nova_type(generator: &mut #krate::derive::SchemaGen) -> #krate::TypeRef {
                generator.named(#def_name, ::std::any::type_name::<Self>(), |generator| {
                    #doc
                    #body
                })
            }

            #id
            #imports
        }
    })
}
