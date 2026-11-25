//! Schema imports (proposal 0008): linking the definitions of imported
//! schemas into a schema, and the bundled standard library
//! `novafile.dev/std`.
//!
//! Linking copies every definition of an imported schema under the name
//! `alias.Name`, and rewrites the references inside it the same way. The
//! resolver, the compatibility check and the writers then see imported
//! types as ordinary definitions. The canonical text keeps only the
//! `@import` lines, with their fingerprints, so a schema's fingerprint pins
//! the content of everything it imports.

use crate::model::{Def, Schema, TypeKind, TypeRef};
use crate::parse::{SchemaError, check_refs, parse_schema_unlinked};
use crate::version::SchemaRef;
use std::sync::OnceLock;

/// The most imported definitions one schema may hold, its imports' own
/// imports included. Each alias copies the definitions below it, so a chain
/// of schemas that import each other under several aliases could otherwise
/// grow exponentially.
const MAX_IMPORTED_DEFS: usize = 10_000;

/// The name of the bundled standard library of types (proposal 0008).
pub const STD_NAME: &str = "novafile.dev/std";

/// The source of every bundled version of the standard library, oldest first.
pub const STD_SRCS: &[&str] = &[include_str!("../meta/std.novs")];

/// Every bundled version of the standard library, oldest first.
///
/// # Panics
///
/// Never in a released build: the bundled sources are valid schemas, and a
/// unit test checks that.
#[must_use]
pub fn std_schemas() -> &'static [Schema] {
    /// The parsed bundled versions, built on first use.
    static STD: OnceLock<Vec<Schema>> = OnceLock::new();
    STD.get_or_init(|| {
        STD_SRCS
            .iter()
            .map(|src| {
                let mut s = parse_schema_unlinked(src).expect("the bundled standard library parses");
                s.link(&|_| Err(String::new())).expect("the bundled standard library has no imports");
                s
            })
            .collect()
    })
}

/// The bundled schema with exactly the name and version of `r`, if any.
#[must_use]
pub fn bundled(r: &SchemaRef) -> Option<&'static Schema> {
    if r.name != STD_NAME {
        return None;
    }

    std_schemas()
        .iter()
        .find(|s| s.version.as_ref().is_some_and(|v| v.precedence(&r.version) == std::cmp::Ordering::Equal))
}

impl Schema {
    /// Link the imports: resolve each one, check its fingerprint, copy its
    /// definitions in as `alias.Name`, then check every reference of the
    /// schema. Bundled schemas resolve without `load`; `load` must return
    /// other imports already linked. Linking again replaces the result of an
    /// earlier link.
    ///
    /// # Errors
    ///
    /// Fails if an import does not resolve, declares a different name or
    /// version, does not match its fingerprint, or has an alias that is also
    /// a definition name; and on the reference checks of
    /// [`crate::parse_schema`]: unknown types, alias cycles, invalid
    /// defaults, misplaced patterns and classes.
    pub fn link(&mut self, load: &dyn Fn(&SchemaRef) -> Result<Schema, String>) -> Result<(), SchemaError> {
        let fail = |message: String| SchemaError { line: 0, message };
        self.imported.clear();
        self.def_classes.retain(|(n, _)| !n.contains('.'));
        self.def_docs.retain(|(n, _)| !n.contains('.'));
        for idx in 0..self.imports.len() {
            let import = &self.imports[idx];
            let dep = match bundled(&import.reference) {
                Some(s) => s.clone(),
                None => load(&import.reference).map_err(|m| fail(format!("@import {}: {m}", import.reference)))?,
            };
            let fingerprint = check_import(&import.reference, &dep).map_err(|m| fail(format!("@import {}: {m}", import.reference)))?;
            let alias = import.alias.clone();
            self.imports[idx].reference.fingerprint = Some(fingerprint);
            if self.imported.len() + dep.defs.len() + dep.imported.len() > MAX_IMPORTED_DEFS {
                return Err(fail(format!("imports bring in more than {MAX_IMPORTED_DEFS} definitions")));
            }

            self.copy_definitions(&alias, &dep);
        }

        if let Some(i) = self.imports.iter().find(|i| self.defs.iter().any(|(n, _)| *n == i.alias)) {
            return Err(fail(format!("import alias {} is also a definition name", i.alias)));
        }

        check_refs(self).map_err(fail)
    }

    /// Copy the definitions, definition classes and definition docs of `dep`, its own imports
    /// included, under `alias.`.
    fn copy_definitions(&mut self, alias: &str, dep: &Schema) {
        for (name, def) in dep.defs.iter().chain(&dep.imported) {
            let mut def = def.clone();
            qualify_def(&mut def, alias);
            self.imported.push((format!("{alias}.{name}"), def));
        }

        for (name, class) in &dep.def_classes {
            self.def_classes.push((format!("{alias}.{name}"), *class));
        }

        for (name, doc) in &dep.def_docs {
            self.def_docs.push((format!("{alias}.{name}"), doc.clone()));
        }
    }
}

/// Check that `dep` is the schema `r` names, and return its fingerprint.
/// Fails on another name or version, or a fingerprint that does not match.
fn check_import(r: &SchemaRef, dep: &Schema) -> Result<String, String> {
    let declared = dep.reference().ok_or("the imported schema has no @schema line")?;
    if declared.name != r.name || declared.version.precedence(&r.version) != std::cmp::Ordering::Equal {
        return Err(format!("found {declared} instead"));
    }

    let actual = dep.fingerprint_hex();
    match &r.fingerprint {
        Some(fp) if *fp != actual => Err(format!("fingerprint mismatch: the import pins #{fp}, the schema is #{actual}")),
        _ => Ok(actual),
    }
}

/// Prefix every type name that `def` references with `alias.`.
fn qualify_def(def: &mut Def, alias: &str) {
    match def {
        Def::Alias(t) => qualify(t, alias),
        Def::Enum(vs) => vs.iter_mut().filter_map(|v| v.payload.as_mut()).for_each(|t| qualify(t, alias)),
        Def::Record(r) => r.fields.iter_mut().for_each(|f| qualify(&mut f.ty, alias)),
    }
}

/// Prefix every type name in `t` with `alias.`. The depth of `t` is bounded
/// by the parser's nesting limit, so the recursion is bounded too.
fn qualify(t: &mut TypeRef, alias: &str) {
    match &mut t.kind {
        TypeKind::Named(n) => *n = format!("{alias}.{n}"),
        TypeKind::List(e) | TypeKind::Set(e) => qualify(e, alias),
        TypeKind::Map(k, v) => {
            qualify(k, alias);
            qualify(v, alias);
        }
        TypeKind::Tuple(ts) | TypeKind::Union(ts) => ts.iter_mut().for_each(|t| qualify(t, alias)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled standard library version parses and links.
    #[test]
    fn std_versions_parse() {
        assert_eq!(std_schemas().len(), STD_SRCS.len());
        assert!(std_schemas().iter().all(|s| s.name.as_deref() == Some(STD_NAME)));
    }
}
