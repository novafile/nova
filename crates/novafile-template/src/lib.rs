//! NOVA templates (spec §9).
//!
//! A template is a document whose first directive is `@template`. Rendering
//! turns it into a plain value; nothing downstream (schemas, hashing, binary
//! encoding, patches) ever sees template constructs. The language is
//! deliberately *total*: no recursion, no user-defined functions, no
//! arithmetic, iteration only over finite values, and an output budget — so
//! every render terminates with bounded output. Templates substitute values,
//! never text, so there is no indentation or escaping to get wrong.

use novafile_schema::{Schema, parse_type};
use novafile_text::ast::*;
use novafile_text::{Path, Segment, parse_document};
use novafile_value::{Record, Value};
use std::fmt;
use std::path::{Component, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TemplateError {}

fn err<T>(m: impl Into<String>) -> Result<T, TemplateError> {
    Err(TemplateError(m.into()))
}

/// Resolves `@import` paths. Hosts decide what is reachable; the reference
/// [`FsLoader`] only reads files beneath a root directory.
pub trait Loader {
    /// Load `path` as imported from the document identified by `from`.
    /// Returns a stable identifier (for cycle detection) and the source.
    fn load(&self, path: &str, from: Option<&str>) -> Result<(String, String), String>;
}

/// Imports are disabled.
pub struct NoImports;

impl Loader for NoImports {
    fn load(&self, path: &str, _: Option<&str>) -> Result<(String, String), String> {
        Err(format!("imports are disabled (tried to import {path:?})"))
    }
}

/// Reads relative paths beneath `root`; absolute paths and `..` escapes are refused.
pub struct FsLoader {
    pub root: PathBuf,
}

impl Loader for FsLoader {
    fn load(&self, path: &str, from: Option<&str>) -> Result<(String, String), String> {
        let rel = std::path::Path::new(path);
        if rel.is_absolute() || path.contains("://") {
            return Err(format!("import {path:?} must be a relative file path"));
        }
        let base = from
            .map(|f| std::path::Path::new(f).parent().unwrap_or(std::path::Path::new("")).to_path_buf())
            .unwrap_or_default();
        let mut joined = PathBuf::new();
        for c in base.join(rel).components() {
            match c {
                Component::Normal(p) => joined.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !joined.pop() {
                        return Err(format!("import {path:?} escapes the template root"));
                    }
                }
                _ => return Err(format!("import {path:?} is not a plain relative path")),
            }
        }
        let full = self.root.join(&joined);
        let src = std::fs::read_to_string(&full).map_err(|e| format!("cannot import {path:?}: {e}"))?;
        Ok((joined.to_string_lossy().into_owned(), src))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum number of value nodes produced (guards against exponential `@let` reuse).
    pub max_nodes: usize,
    pub max_import_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_nodes: 1_000_000,
            max_import_depth: 16,
        }
    }
}

pub struct Options<'a> {
    pub loader: &'a dyn Loader,
    /// Schema whose named types parameters may use (e.g. `list<Service>`).
    pub schema: Option<&'a Schema>,
    pub limits: Limits,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            loader: &NoImports,
            schema: None,
            limits: Limits::default(),
        }
    }
}

/// Render template source with the given arguments.
pub fn render(src: &str, args: &Record, opts: &Options) -> Result<Value, TemplateError> {
    render_inner(src, None, args, opts, &mut Vec::new(), &mut 0)
}

fn render_inner(src: &str, id: Option<&str>, args: &Record, opts: &Options, stack: &mut Vec<String>, budget: &mut usize) -> Result<Value, TemplateError> {
    let where_ = || id.map(|i| format!(" in {i}")).unwrap_or_default();
    let doc = parse_document(src).map_err(|e| TemplateError(format!("{e}{}", where_())))?;
    let Some(h) = &doc.template else {
        if !args.is_empty() {
            return err("arguments were given but the document is not a @template");
        }
        return doc.body.to_value().map_err(TemplateError);
    };
    let mut ev = Eval {
        scopes: vec![Vec::new()],
        budget,
        max: opts.limits.max_nodes,
    };

    // Parameters.
    let empty = Schema::default();
    let schema = opts.schema.unwrap_or(&empty);
    for k in args.keys() {
        if !h.params.iter().any(|p| &p.name == k) {
            return err(format!("unknown parameter {k:?}{}", where_()));
        }
    }
    for p in &h.params {
        let ty = parse_type(&p.ty).map_err(|m| TemplateError(format!("@param {}: {m}", p.name)))?;
        let v = match (args.get(&p.name), &p.default) {
            (Some(v), _) => v.clone(),
            (None, Some(d)) => ev.node(d)?,
            (None, None) if p.optional => Value::Null,
            (None, None) => return err(format!("missing required parameter {:?}{}", p.name, where_())),
        };
        let v = if matches!(v, Value::Null) && p.optional {
            v
        } else {
            schema.resolve_as(&v, &ty).map_err(|issues| {
                TemplateError(format!(
                    "parameter {:?} does not match `{}`: {}",
                    p.name,
                    p.ty,
                    issues.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("; ")
                ))
            })?
        };
        ev.bind(&p.name, v)?;
    }

    // Imports.
    for imp in &h.imports {
        if stack.len() >= opts.limits.max_import_depth {
            return err("import depth limit exceeded");
        }
        let (iid, isrc) = opts.loader.load(&imp.path, id).map_err(TemplateError)?;
        if stack.contains(&iid) || id == Some(iid.as_str()) {
            return err(format!("import cycle through {iid}"));
        }
        stack.push(id.unwrap_or("<root>").to_owned());
        let v = render_inner(&isrc, Some(&iid), &Record::new(), opts, stack, ev.budget)?;
        stack.pop();
        ev.bind(&imp.alias, v)?;
    }

    // Lets, in order; each may use parameters, imports and earlier lets.
    for l in &h.lets {
        let v = ev.node(&l.value)?;
        ev.bind(&l.name, v)?;
    }
    ev.node(&doc.body)
}

struct Eval<'b> {
    scopes: Vec<Vec<(String, Value)>>,
    budget: &'b mut usize,
    max: usize,
}

fn size(v: &Value) -> usize {
    1 + match v {
        Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.iter().map(size).sum(),
        Value::Record(r) => r.iter().map(|(_, v)| size(v)).sum(),
        Value::Map(m) => m.iter().map(|(k, v)| size(k) + size(v)).sum(),
        Value::Variant(_, Some(p)) => size(p),
        _ => 0,
    }
}

impl<'b> Eval<'b> {
    fn charge(&mut self, n: usize) -> Result<(), TemplateError> {
        *self.budget += n;
        if *self.budget > self.max {
            return err(format!("template output exceeds the limit of {} nodes", self.max));
        }
        Ok(())
    }

    fn bind(&mut self, name: &str, v: Value) -> Result<(), TemplateError> {
        let scope = self.scopes.last_mut().unwrap();
        if scope.iter().any(|(n, _)| n == name) {
            return err(format!("{name:?} is already defined"));
        }
        scope.push((name.to_owned(), v));
        Ok(())
    }

    fn lookup(&mut self, p: &Path) -> Result<Value, TemplateError> {
        let Some((Segment::Field(name), rest)) = p.0.split_first() else {
            return err(format!("invalid reference ${p}"));
        };
        let base = self
            .scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
            .ok_or_else(|| TemplateError(format!("undefined reference ${name}")))?;
        let v = Path(rest.to_vec()).get(base).map_err(|e| TemplateError(format!("${p}: {e}")))?.clone();
        self.charge(size(&v))?;
        Ok(v)
    }

    fn text(&mut self, parts: &[Part]) -> Result<String, TemplateError> {
        let mut s = String::new();
        for p in parts {
            match p {
                Part::Text(t) => s.push_str(t),
                Part::Ref(r) => {
                    let v = self.lookup(r)?;
                    match &v {
                        Value::String(x) => s.push_str(x),
                        Value::Int(i) => s.push_str(&i.to_string()),
                        Value::Float(f) => s.push_str(&format!("{f:?}")),
                        Value::Decimal(d) => s.push_str(&d.to_string()),
                        Value::Rational(r) => s.push_str(&r.to_string()),
                        Value::Bool(b) => s.push_str(&b.to_string()),
                        Value::Timestamp(t) => s.push_str(&t.to_string()),
                        Value::Duration(d) => s.push_str(&d.to_string()),
                        Value::Variant(t, None) => s.push_str(t),
                        other => return err(format!("cannot interpolate a {} (${r}); only scalars can appear in strings", other.kind())),
                    }
                }
            }
        }
        Ok(s)
    }

    fn cond(&mut self, c: &Cond) -> Result<bool, TemplateError> {
        Ok(match c {
            Cond::Test(n) => match self.node(n)? {
                Value::Bool(b) => b,
                other => {
                    return err(format!(
                        "conditions must be bool, found {} (compare explicitly, e.g. `$x != null`)",
                        other.kind()
                    ));
                }
            },
            Cond::Eq(a, b) => self.node(a)? == self.node(b)?,
            Cond::Ne(a, b) => self.node(a)? != self.node(b)?,
            Cond::Not(c) => !self.cond(c)?,
            Cond::And(a, b) => self.cond(a)? && self.cond(b)?,
            Cond::Or(a, b) => self.cond(a)? || self.cond(b)?,
        })
    }

    /// Run `body` once per element of the iterable, with loop variables in a new scope.
    fn each<T>(&mut self, head: &ForHead, mut body: impl FnMut(&mut Self) -> Result<T, TemplateError>) -> Result<Vec<T>, TemplateError> {
        let pairs: Vec<(Value, Value)> = match self.node(&head.iter)? {
            Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.into_iter().enumerate().map(|(i, v)| (Value::Int(i as i128), v)).collect(),
            Value::Record(r) => r.into_entries().into_iter().map(|(k, v)| (Value::String(k), v)).collect(),
            Value::Map(m) => m,
            other => return err(format!("@for needs a list, set, tuple, record or map, found {}", other.kind())),
        };
        let mut out = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            self.scopes.push(Vec::new());
            if let Some(i) = &head.index {
                self.bind(i, k)?;
            }
            self.bind(&head.var, v)?;
            let r = body(self);
            self.scopes.pop();
            out.push(r?);
        }
        Ok(out)
    }

    fn items(&mut self, items: &[Item], out: &mut Vec<Value>) -> Result<(), TemplateError> {
        for it in items {
            match it {
                Item::Node(n) => out.push(self.node(n)?),
                Item::Spread(n) => match self.node(n)? {
                    Value::List(l) | Value::Tuple(l) | Value::Set(l) => out.extend(l),
                    other => return err(format!("cannot spread a {} into a list", other.kind())),
                },
                Item::If(c, t, e) => {
                    let branch = if self.cond(c)? { t } else { e };
                    self.items(branch, out)?;
                }
                Item::For(h, body) => {
                    for chunk in self.each(h, |ev| {
                        let mut v = Vec::new();
                        ev.items(body, &mut v)?;
                        Ok(v)
                    })? {
                        out.extend(chunk);
                    }
                }
            }
        }
        Ok(())
    }

    /// Later entries win, except that two explicit fields may not share a key.
    fn entries(&mut self, entries: &[Entry], out: &mut Record, explicit: &mut Vec<String>) -> Result<(), TemplateError> {
        for e in entries {
            match e {
                Entry::Field(k, n) => {
                    let key = match k {
                        Key::Lit(s) => s.clone(),
                        Key::Interp(parts) => self.text(parts)?,
                    };
                    if explicit.contains(&key) {
                        return err(format!("duplicate key {key:?}"));
                    }
                    let v = self.node(n)?;
                    out.set(key.clone(), v);
                    explicit.push(key);
                }
                Entry::Spread(n) => match self.node(n)? {
                    Value::Record(r) => {
                        for (k, v) in r.into_entries() {
                            explicit.retain(|x| *x != k);
                            out.set(k, v);
                        }
                    }
                    other => return err(format!("cannot spread a {} into a record", other.kind())),
                },
                Entry::If(c, t, e) => {
                    let branch = if self.cond(c)? { t } else { e };
                    self.entries(branch, out, explicit)?;
                }
                Entry::For(h, body) => {
                    // Each iteration contributes entries to the same record.
                    let parts = self.each(h, |ev| {
                        let mut r = Record::new();
                        let mut ex = Vec::new();
                        ev.entries(body, &mut r, &mut ex)?;
                        Ok(r)
                    })?;
                    for r in parts {
                        for (k, v) in r.into_entries() {
                            if explicit.contains(&k) {
                                return err(format!("duplicate key {k:?} produced by @for"));
                            }
                            out.set(k.clone(), v);
                            explicit.push(k);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn node(&mut self, n: &Node) -> Result<Value, TemplateError> {
        self.charge(1)?;
        Ok(match n {
            Node::Lit(v) => {
                self.charge(size(v) - 1)?;
                v.clone()
            }
            Node::List(items) => {
                let mut out = Vec::new();
                self.items(items, &mut out)?;
                Value::List(out)
            }
            Node::Tuple(items) => Value::Tuple(items.iter().map(|n| self.node(n)).collect::<Result<_, _>>()?),
            Node::Set(items) => Value::set(items.iter().map(|n| self.node(n)).collect::<Result<_, _>>()?).map_err(TemplateError)?,
            Node::Map(entries) => Value::map(
                entries
                    .iter()
                    .map(|(k, v)| Ok((self.node(k)?, self.node(v)?)))
                    .collect::<Result<_, TemplateError>>()?,
            )
            .map_err(TemplateError)?,
            Node::Record(entries) => {
                let mut r = Record::new();
                self.entries(entries, &mut r, &mut Vec::new())?;
                Value::Record(r)
            }
            Node::Variant(t, p) => Value::Variant(t.clone(), p.as_ref().map(|p| self.node(p).map(Box::new)).transpose()?),
            Node::Ref(p) => self.lookup(p)?,
            Node::Interp(parts) => Value::String(self.text(parts)?),
        })
    }
}
