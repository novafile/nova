//! The positional profile (spec §4.12): schema-driven transport text that
//! drops field names.
//!
//! A record of a named record type is written as a tuple of its field
//! values in declared order; an absent optional field is the reserved
//! marker `!_`. The document root, when it is a record, is written as a
//! block list (one field per line) for readability. Everything else is
//! unchanged, so a positional document is ordinary NOVA text: the parser,
//! writer, escaping and tokenization rules are the same. Only its meaning
//! depends on the schema, which is why readers must hold exactly the
//! schema identified by the document's fingerprint.

use crate::model::{Def, Schema, TypeKind, TypeRef};
use novafile_value::{Record, Value};

/// Tag of the `!_` marker that stands for an absent optional field.
const ABSENT: &str = "_";

/// The `!_` marker.
fn absent() -> Value {
    Value::Variant(ABSENT.into(), None)
}

impl Schema {
    /// Convert a value (resolved against the root type first) into its positional form.
    ///
    /// # Errors
    ///
    /// Fails if the schema has no `@root`, or if `v` does not resolve against it
    /// (the issues are joined into one message).
    pub fn to_positional(&self, v: &Value) -> Result<Value, String> {
        let root = self.root_type().ok_or("the positional profile needs a schema with @root")?;
        let resolved = self
            .resolve_as(v, &root)
            .map_err(|i| i.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("; "))?;
        let out = self.pos_out(&resolved, &root, "")?;
        Ok(match out {
            Value::Tuple(items) => Value::List(items),
            other => other,
        })
    }

    /// Convert a positional value back into a normal (named) value, validated against the schema.
    ///
    /// # Errors
    ///
    /// Fails if the schema has no `@root`, if `v` does not have the positional
    /// shape, or if the named value does not resolve against the schema.
    pub fn from_positional(&self, v: &Value) -> Result<Value, String> {
        let root = self.root_type().ok_or("the positional profile needs a schema with @root")?;
        let named = self.pos_in(v, &root, "")?;
        self.resolve_as(&named, &root)
            .map_err(|i| i.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("; "))
    }

    /// Named to positional form for a value of type `t`; `at` is the path used
    /// in error messages. Fails on undeclared record fields and on the reserved
    /// `_` tag.
    fn pos_out(&self, v: &Value, t: &TypeRef, at: &str) -> Result<Value, String> {
        if matches!(v, Value::Variant(tag, _) if tag == ABSENT) {
            return Err(format!("{at}: the variant tag `_` is reserved in the positional profile"));
        }

        let (t, def) = self.deref(t);
        Ok(match (v, def, &t.kind) {
            (Value::Record(r), Some(Def::Record(rd)), _) => {
                if let Some(extra) = r.keys().find(|k| rd.field(k).is_none()) {
                    return Err(format!("{at}.{extra}: unknown fields cannot be written positionally"));
                }

                let items = rd
                    .fields
                    .iter()
                    .map(|f| match r.get(&f.name) {
                        Some(x) => self.pos_out(x, &f.ty, &format!("{at}.{}", f.name)),
                        None => Ok(absent()),
                    })
                    .collect::<Result<_, _>>()?;
                Value::Tuple(items)
            }
            (Value::Variant(tag, Some(p)), Some(Def::Enum(vs)), _) => {
                let pt = vs.iter().find(|x| &x.name == tag).and_then(|x| x.payload.as_ref());
                let p = match pt {
                    Some(pt) => self.pos_out(p, pt, &format!("{at}!{tag}"))?,
                    None => (**p).clone(),
                };

                Value::Variant(tag.clone(), Some(Box::new(p)))
            }
            (Value::List(l), _, TypeKind::List(et)) => Value::List(self.each_out(l, et, at)?),
            (Value::Set(l), _, TypeKind::Set(et)) => Value::Set(self.each_out(l, et, at)?),
            (Value::Tuple(l), _, TypeKind::Tuple(ts)) => Value::Tuple(
                l.iter()
                    .zip(ts)
                    .enumerate()
                    .map(|(i, (x, t))| self.pos_out(x, t, &format!("{at}[{i}]")))
                    .collect::<Result<_, _>>()?,
            ),
            (Value::Map(m), _, TypeKind::Map(_, vt)) => Value::Map(
                m.iter()
                    .map(|(k, x)| Ok((k.clone(), self.pos_out(x, vt, &format!("{at}[…]"))?)))
                    .collect::<Result<_, String>>()?,
            ),
            _ => v.clone(),
        })
    }

    /// [`Self::pos_out`] for each element of a list or set.
    fn each_out(&self, l: &[Value], et: &TypeRef, at: &str) -> Result<Vec<Value>, String> {
        l.iter().enumerate().map(|(i, x)| self.pos_out(x, et, &format!("{at}[{i}]"))).collect()
    }

    /// Positional to named form for a value of type `t`; the inverse of
    /// [`Self::pos_out`]. Fails if a record tuple has the wrong number of fields.
    fn pos_in(&self, v: &Value, t: &TypeRef, at: &str) -> Result<Value, String> {
        let (t, def) = self.deref(t);
        Ok(match (v, def, &t.kind) {
            (Value::Tuple(items) | Value::List(items), Some(Def::Record(rd)), _) => {
                if items.len() != rd.fields.len() {
                    return Err(format!("{at}: expected {} positional fields, found {}", rd.fields.len(), items.len()));
                }

                let mut r = Record::new();
                for (f, x) in rd.fields.iter().zip(items) {
                    if matches!(x, Value::Variant(tag, None) if tag == ABSENT) {
                        continue;
                    }

                    r.insert(f.name.clone(), self.pos_in(x, &f.ty, &format!("{at}.{}", f.name))?)
                        .map_err(|e| e.to_string())?;
                }

                Value::Record(r)
            }
            (Value::Variant(tag, Some(p)), Some(Def::Enum(vs)), _) => {
                let pt = vs.iter().find(|x| &x.name == tag).and_then(|x| x.payload.as_ref());
                let p = match pt {
                    Some(pt) => self.pos_in(p, pt, &format!("{at}!{tag}"))?,
                    None => (**p).clone(),
                };

                Value::Variant(tag.clone(), Some(Box::new(p)))
            }
            (Value::List(l), _, TypeKind::List(et)) => Value::List(self.each_in(l, et, at)?),
            (Value::Set(l) | Value::List(l), _, TypeKind::Set(et)) => Value::set(self.each_in(l, et, at)?)?,
            (Value::Tuple(l) | Value::List(l), _, TypeKind::Tuple(ts)) => Value::Tuple(
                l.iter()
                    .zip(ts)
                    .enumerate()
                    .map(|(i, (x, t))| self.pos_in(x, t, &format!("{at}[{i}]")))
                    .collect::<Result<_, _>>()?,
            ),
            (Value::Map(m), _, TypeKind::Map(_, vt)) => Value::Map(
                m.iter()
                    .map(|(k, x)| Ok((k.clone(), self.pos_in(x, vt, &format!("{at}[…]"))?)))
                    .collect::<Result<_, String>>()?,
            ),
            _ => v.clone(),
        })
    }

    /// [`Self::pos_in`] for each element of a list or set.
    fn each_in(&self, l: &[Value], et: &TypeRef, at: &str) -> Result<Vec<Value>, String> {
        l.iter().enumerate().map(|(i, x)| self.pos_in(x, et, &format!("{at}[{i}]"))).collect()
    }
}
