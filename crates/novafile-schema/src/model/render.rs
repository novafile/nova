//! Schema text rendering: the canonical text (spec §6.6), which fingerprints
//! hash, and the source text, which also keeps the documentation comments.

use super::{Def, FieldDef, LenUnit, RecordDef, Schema, TypeKind, TypeRef, VariantDef};
use std::fmt;

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TypeKind::Any => f.write_str("any")?,
            TypeKind::Null => f.write_str("null")?,
            TypeKind::Bool => f.write_str("bool")?,
            TypeKind::Int(k) => f.write_str(k.name())?,
            TypeKind::Float => f.write_str("float")?,
            TypeKind::Decimal => f.write_str("decimal")?,
            TypeKind::Rational => f.write_str("rational")?,
            TypeKind::String => f.write_str("string")?,
            TypeKind::Bytes => f.write_str("bytes")?,
            TypeKind::Timestamp => f.write_str("timestamp")?,
            TypeKind::Duration => f.write_str("duration")?,
            TypeKind::List(t) => write!(f, "list<{t}>")?,
            TypeKind::Set(t) => write!(f, "set<{t}>")?,
            TypeKind::Map(k, v) => write!(f, "map<{k}, {v}>")?,
            TypeKind::Tuple(ts) => {
                f.write_str("(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }

                    write!(f, "{t}")?;
                }

                if ts.len() == 1 {
                    f.write_str(",")?;
                }

                f.write_str(")")?;
            }
            TypeKind::Named(n) => f.write_str(n)?,
            TypeKind::Literal(s) => f.write_str(&novafile_text::quote(s))?,
            TypeKind::Union(ms) => {
                for (i, m) in ms.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }

                    write!(f, "{m}")?;
                }
            }
        }

        if self.nullable {
            f.write_str("?")?;
        }

        let b = |o: Option<String>| o.unwrap_or_default();
        if let Some((lo, hi)) = &self.constraints.range {
            write!(
                f,
                " @range({}..{})",
                b(lo.as_ref().map(std::string::ToString::to_string)),
                b(hi.as_ref().map(std::string::ToString::to_string))
            )?;
        }

        if let Some((lo, hi)) = &self.constraints.len {
            write!(f, " @len({}..{}", b(lo.map(|n| n.to_string())), b(hi.map(|n| n.to_string())))?;
            if self.constraints.len_unit != LenUnit::Scalars {
                write!(f, ", {}", self.constraints.len_unit.name())?;
            }

            f.write_str(")")?;
        }

        if let Some(p) = &self.constraints.pattern {
            write!(f, " @pattern({})", novafile_text::quote(p.as_str()))?;
        }

        if let Some(c) = self.constraints.class {
            write!(f, " @{c}")?;
        }

        Ok(())
    }
}

/// The source text: the canonical text with the documentation of
/// definitions, fields and variants written as `#` comment lines above them.
impl fmt::Display for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Text { schema: self, docs: true }.fmt(f)
    }
}

impl Schema {
    /// The canonical text (spec §6.6): the source text without documentation
    /// comments, so a change of documentation keeps the fingerprint.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        Text { schema: self, docs: false }.to_string()
    }
}

/// A schema written as text, with or without documentation comments.
struct Text<'a> {
    /// The schema to write.
    schema: &'a Schema,
    /// Write the documentation as `#` comment lines. The canonical text has none.
    docs: bool,
}

impl Text<'_> {
    /// Write `doc` as `#` comment lines with the given indentation, if docs are on.
    fn doc(&self, f: &mut fmt::Formatter<'_>, indent: &str, doc: Option<&str>) -> fmt::Result {
        let Some(doc) = doc.filter(|_| self.docs) else { return Ok(()) };
        for line in doc.lines().map(str::trim_end) {
            if line.is_empty() {
                writeln!(f, "{indent}#")?;
            } else {
                writeln!(f, "{indent}# {line}")?;
            }
        }

        Ok(())
    }

    /// Write the variants of an enum definition, one per line.
    fn variants(&self, f: &mut fmt::Formatter<'_>, vs: &[VariantDef]) -> fmt::Result {
        for v in vs {
            self.doc(f, "  ", v.doc.as_deref())?;
            match &v.payload {
                Some(t) => write!(f, "  {}({t})", v.name)?,
                None => write!(f, "  {}", v.name)?,
            }

            match v.id {
                Some(id) => writeln!(f, " @id({id})")?,
                None => writeln!(f)?,
            }
        }

        Ok(())
    }

    /// Write the fields of a record definition, one per line.
    fn fields(&self, f: &mut fmt::Formatter<'_>, r: &RecordDef) -> fmt::Result {
        for fd in &r.fields {
            self.doc(f, "  ", fd.doc.as_deref())?;
            field(f, fd)?;
        }

        Ok(())
    }
}

/// Write one field line of a record definition.
fn field(f: &mut fmt::Formatter<'_>, fd: &FieldDef) -> fmt::Result {
    write!(f, "  {}{}: {}", novafile_text::key_text(&fd.name), if fd.optional { "?" } else { "" }, fd.ty)?;
    if fd.key {
        f.write_str(" @key")?;
    }

    if let Some(id) = fd.id {
        write!(f, " @id({id})")?;
    }

    if fd.deprecated {
        f.write_str(" @deprecated")?;
    }

    if let Some(d) = &fd.default {
        write!(f, " = {}", novafile_text::flow_string(d))?;
    }

    writeln!(f)
}

impl fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.schema;
        if let (Some(n), Some(v)) = (&s.name, &s.version) {
            writeln!(f, "@schema {n}@{v}")?;
        }

        for i in &s.imports {
            writeln!(f, "@import {} as {}", i.reference, i.alias)?;
        }

        if let Some(r) = &s.root {
            writeln!(f, "@root {r}")?;
        }

        let mut defs: Vec<_> = s.defs.iter().collect();
        defs.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, def) in defs {
            writeln!(f)?;
            self.doc(f, "", s.def_doc(name))?;
            let class = s.def_class(name).map(|c| format!(" @{c}")).unwrap_or_default();
            match def {
                Def::Alias(t) => writeln!(f, "type {name} = {t}")?,
                Def::Enum(vs) => {
                    writeln!(f, "enum {name}{class}:")?;
                    self.variants(f, vs)?;
                }
                Def::Record(r) => {
                    write!(f, "record {name}{}", if r.open { " @open" } else { "" })?;
                    for rule in &r.rules {
                        write!(f, " {rule}")?;
                    }

                    writeln!(f, "{class}:")?;
                    self.fields(f, r)?;
                }
            }
        }

        Ok(())
    }
}
