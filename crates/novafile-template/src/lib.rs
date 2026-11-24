//! NOVA templates (spec §9).
//!
//! A template is a document whose first directive is `@template`. Rendering
//! turns it into a plain value; nothing downstream (schemas, hashing, binary
//! encoding, patches) ever sees template constructs. The language is
//! deliberately *total*: no recursion, no user-defined functions, no
//! arithmetic, iteration only over finite values, and an output budget — so
//! every render terminates with bounded output. Templates substitute values,
//! never text, so there is no indentation or escaping to get wrong.
//!
//! [`render_redacted`] also knows classified values (proposal 0009.7). A
//! value that comes from an imported document through a `@personal` or
//! `@secret` type is classified, and so is every value derived from it. The
//! output then holds placeholders in place of these values, unless the mode
//! reveals them. The `taint` module tells how.

mod taint;

use novafile_schema::{Class, RedactKey, Redacted, Reveal, SCHEMA_BUDGET, Schema, parse_type_within, redact};
use novafile_text::ast::{Cond, Entry, ForHead, Item, Key, Node, Part};
use novafile_text::path::step;
use novafile_text::{Path, Segment, parse_document};
use novafile_value::{Record, Value};
use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::path::{Component, PathBuf};

/// A template that failed to parse, bind its arguments, import, or evaluate; the message
/// says why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TemplateError {}

/// Shorthand for returning a [`TemplateError`] with message `m`.
fn err<T>(m: impl Into<String>) -> Result<T, TemplateError> {
    Err(TemplateError(m.into()))
}

/// Resolves `@import` paths. Hosts decide what is reachable; the reference
/// [`FsLoader`] only reads files beneath a root directory.
pub trait Loader {
    /// Load `path` as imported from the document identified by `from`.
    /// Returns a stable identifier (for cycle detection) and the source.
    ///
    /// # Errors
    ///
    /// Returns a message if `path` is not reachable from `from` or cannot be read.
    fn load(&self, path: &str, from: Option<&str>) -> Result<(String, String), String>;

    /// The schema that the `@schema` directive `reference` names, for the
    /// document identified by `from` (as [`Loader::load`] returned it, or
    /// `None` for the template itself). Only [`render_redacted`] asks for it.
    ///
    /// The default knows no schemas and returns `Ok(None)`. Then
    /// [`render_redacted`] refuses a document that declares `@schema`, unless
    /// the mode reveals everything: without the schema, nothing in that
    /// document would be redacted (0009.5.3, fail closed).
    ///
    /// # Errors
    ///
    /// Returns a message if `reference` names a schema that cannot be read.
    fn schema(&self, reference: &str, from: Option<&str>) -> Result<Option<Schema>, String> {
        let _ = (reference, from);
        Ok(None)
    }
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
    /// Directory that imports resolve against and may not escape.
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

        // The check above is lexical; a symlink beneath the root may still point out of it.
        // Read the resolved path that was checked, not `full`, so a symlink
        // swapped in between the check and the read is not followed.
        let root = self.root.canonicalize().map_err(|e| format!("template root {}: {e}", self.root.display()))?;
        let full = self.root.join(&joined).canonicalize().map_err(|e| format!("cannot import {path:?}: {e}"))?;
        if !full.starts_with(&root) {
            return Err(format!("import {path:?} escapes the template root"));
        }

        let src = std::fs::read_to_string(&full).map_err(|e| format!("cannot import {path:?}: {e}"))?;
        Ok((joined.to_string_lossy().into_owned(), src))
    }
}

/// Resource limits that keep every render bounded.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum number of value nodes produced, counting every copy (guards against exponential
    /// `@let` reuse).
    pub max_nodes: usize,
    /// Maximum total bytes of string and byte-string content produced, counting every copy
    /// (guards against strings doubled by repeated `${}` interpolation).
    pub max_bytes: usize,
    /// Maximum nesting depth of `@import` chains.
    pub max_import_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_nodes: 1_000_000,
            max_bytes: 64 << 20,
            max_import_depth: 16,
        }
    }
}

/// Settings for [`render`].
pub struct Options<'a> {
    /// Resolves `@import` paths; [`NoImports`] by default.
    pub loader: &'a dyn Loader,
    /// Schema whose named types parameters may use (e.g. `list<Service>`).
    pub schema: Option<&'a Schema>,
    /// Output budget and import depth.
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
///
/// This function does not know classified values: it does not ask the
/// loader for schemas, and it shows every value in clear. It is the
/// rendering for deployment, which 0009.7 says must reveal anyway, and it
/// keeps its behavior from before proposal 0009. Use [`render_redacted`] for
/// output that a person or agent reads.
///
/// # Errors
///
/// Returns a [`TemplateError`] if `src` is not a valid template; an argument is
/// unknown, missing, or does not match its `@param` type (resolved against
/// `opts.schema`); an import fails, cycles, or nests too deeply; evaluation
/// fails; or the output exceeds the node or byte budget in `opts.limits`.
pub fn render(src: &str, args: &Record, opts: &Options) -> Result<Value, TemplateError> {
    let run = Run {
        opts,
        reveal: None,
        patterns: Cell::new(SCHEMA_BUDGET),
    };

    render_inner(src, None, args, &run, &mut Vec::new(), &mut Cost::default())
}

/// How [`render_redacted`] shows classified values (proposal 0009.5.1).
/// The default redacts all of them.
#[derive(Clone, Debug, Default)]
pub struct Redaction<'a> {
    /// Which classified values to show in clear. `Reveal::Paths` names
    /// paths in the rendered output.
    pub reveal: Reveal,
    /// Makes pseudonyms for personal values; without it they become a bare
    /// `@redacted`.
    pub key: Option<&'a RedactKey>,
}

/// Render like [`render`], and replace classified values by placeholders
/// as `mode` says (proposal 0009.7).
///
/// An imported data document that declares `@schema` is resolved against
/// the schema that [`Loader::schema`] gives. Its `@personal` and `@secret`
/// values are classified, and so is every value that the template derives
/// from them: a reference into them, and a string that interpolates one,
/// gets the strongest class among its parts. The template itself and its
/// arguments classify nothing.
///
/// # Errors
///
/// As [`render`]. Also fails if an imported document declares a `@schema`
/// that the loader cannot give (except when `mode` reveals everything), or
/// does not match it; and, unless `mode` shows the class, if the template
/// branches on a classified value: an `@if` condition, an `@for` or a spread
/// over a classified container, a selector that compares a classified
/// value, or a classified record key, set element or map key.
pub fn render_redacted(src: &str, args: &Record, opts: &Options, mode: &Redaction) -> Result<Redacted, TemplateError> {
    let run = Run {
        opts,
        reveal: Some(&mode.reveal),
        patterns: Cell::new(SCHEMA_BUDGET),
    };

    let v = render_inner(src, None, args, &run, &mut Vec::new(), &mut Cost::default())?;
    let mut marks = Vec::new();
    let clear = taint::strip(v, &mut marks)?;
    Ok(redact(&clear, &marks, &mode.reveal, mode.key))
}

/// Settings shared by a template and all its imports.
struct Run<'r, 'o> {
    /// The caller's settings.
    opts: &'r Options<'o>,
    /// The reader mode, or `None` if classification is off ([`render`]).
    reveal: Option<&'r Reveal>,
    /// Budget for the parameter patterns of the template and all its imports.
    patterns: Cell<usize>,
}

/// Render `src`, identified by `id` when imported. `stack` holds the importing documents
/// (for cycle detection) and `budget` the output produced so far, shared across imports.
fn render_inner(src: &str, id: Option<&str>, args: &Record, run: &Run, stack: &mut Vec<String>, budget: &mut Cost) -> Result<Value, TemplateError> {
    let opts = run.opts;
    let where_ = || id.map(|i| format!(" in {i}")).unwrap_or_default();
    let doc = parse_document(src).map_err(|e| TemplateError(format!("{e}{}", where_())))?;
    let Some(h) = &doc.template else {
        if !args.is_empty() {
            return err("arguments were given but the document is not a @template");
        }

        let v = doc.body.to_value().map_err(TemplateError)?;
        return match (run.reveal, doc.directive("schema")) {
            (Some(reveal), Some(reference)) => classified(v, reference, id, opts.loader, reveal),
            _ => Ok(v),
        };
    };

    let mut ev = Eval {
        scopes: vec![Vec::new()],
        budget,
        limits: opts.limits,
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
        let ty = parse_type_within(&p.ty, &run.patterns).map_err(|m| TemplateError(format!("@param {}: {m}", p.name)))?;
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
                    issues.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("; ")
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
        // Every import is read and parsed again, so its source counts like output;
        // otherwise templates importing each other repeatedly grow exponentially.
        ev.charge(0, isrc.len())?;
        if stack.contains(&iid) || id == Some(iid.as_str()) {
            return err(format!("import cycle through {iid}"));
        }

        stack.push(id.unwrap_or("<root>").to_owned());
        let v = render_inner(&isrc, Some(&iid), &Record::new(), run, stack, ev.budget)?;
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

/// The data document `v`, identified by `id`, resolved against the schema
/// that its `@schema reference` names. Its classified values that `reveal`
/// does not show everywhere get a taint wrapper.
fn classified(mut v: Value, reference: &str, id: Option<&str>, loader: &dyn Loader, reveal: &Reveal) -> Result<Value, TemplateError> {
    let name = id.unwrap_or("the template");
    let schema = match loader.schema(reference, id) {
        Ok(Some(s)) => s,
        _ if *reveal == Reveal::All => return Ok(v),
        Ok(None) => {
            return err(format!(
                "{name} declares @schema {reference}, which cannot be resolved here; nothing in it would be redacted"
            ));
        }
        Err(m) => return err(format!("{name}: @schema {reference}: {m}")),
    };

    let marks;
    (v, marks) = schema.classify(&v).map_err(|issues| {
        TemplateError(format!(
            "{name} does not match @schema {reference}: {}",
            issues.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("; ")
        ))
    })?;

    let mut marks: Vec<_> = marks.into_iter().filter(|(_, c)| !taint::shows(reveal, *c)).collect();
    marks.sort_by_key(|(p, _)| std::cmp::Reverse(p.0.len()));
    for (p, c) in marks {
        taint::wrap_at(&mut v, &p.0, c);
    }

    Ok(v)
}

/// The error for a template that branches on, or selects by, a classified
/// value (0009.7). The message names the construct, never the value.
fn branches_on_classified<T>(what: &str) -> Result<T, TemplateError> {
    err(format!(
        "{what} depends on a classified value; this is not allowed when classified values are redacted"
    ))
}

/// Evaluator state for one template.
struct Eval<'b> {
    /// Bindings, innermost scope last; `@for` pushes a scope per iteration.
    scopes: Vec<Vec<(String, Value)>>,
    /// Output produced so far, shared with importing templates.
    budget: &'b mut Cost,
    /// The node and byte maxima.
    limits: Limits,
}

/// Output produced: value nodes and string/byte-string content bytes.
#[derive(Clone, Copy, Default)]
struct Cost {
    /// Value nodes, containers and their contents alike.
    nodes: usize,
    /// Bytes of string and byte-string content.
    bytes: usize,
}

impl Cost {
    /// The nodes and content bytes of `v`.
    fn of(v: &Value) -> Cost {
        let mut c = Cost::default();
        c.add(v);
        c
    }

    /// Add the nodes and content bytes of `v`.
    fn add(&mut self, v: &Value) {
        self.nodes += 1;
        match v {
            Value::String(s) => self.bytes += s.len(),
            Value::Bytes(b) => self.bytes += b.len(),
            Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.iter().for_each(|v| self.add(v)),
            Value::Record(r) => r.iter().for_each(|(_, v)| self.add(v)),
            Value::Map(m) => m.iter().for_each(|(k, v)| {
                self.add(k);
                self.add(v);
            }),
            Value::Variant(_, Some(p)) => self.add(p),
            _ => {}
        }
    }
}

impl Eval<'_> {
    /// Count `nodes` produced nodes and `bytes` content bytes against the budget; fails once
    /// either limit is exceeded.
    fn charge(&mut self, nodes: usize, bytes: usize) -> Result<(), TemplateError> {
        self.budget.nodes += nodes;
        self.budget.bytes += bytes;
        if self.budget.nodes > self.limits.max_nodes {
            return err(format!("template output exceeds the limit of {} nodes", self.limits.max_nodes));
        }

        if self.budget.bytes > self.limits.max_bytes {
            return err(format!(
                "template output exceeds the limit of {} bytes of string and bytes content",
                self.limits.max_bytes
            ));
        }

        Ok(())
    }

    /// Bind `name` in the innermost scope; redefining a name in the same scope is an error.
    fn bind(&mut self, name: &str, v: Value) -> Result<(), TemplateError> {
        let scope = self.scopes.last_mut().unwrap();
        if scope.iter().any(|(n, _)| n == name) {
            return err(format!("{name:?} is already defined"));
        }

        scope.push((name.to_owned(), v));
        Ok(())
    }

    /// Resolve a `$name.path` reference, searching from the innermost scope outwards. The
    /// result gets the strongest class of the classified values on the way to it.
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
        let mut cur = base;
        let mut class = None;
        for (i, seg) in rest.iter().enumerate() {
            if let Some((c, inner)) = taint::tainted(cur) {
                class = class.max(Some(c));
                cur = inner;
            }

            // Inside a classified container, a selector that compares values (or map keys) is
            // an equality test on classified data: a hit and a miss would differ.
            let compares = matches!(seg, Segment::Match(..) | Segment::Key(_)) || matches!(cur, Value::Map(_));
            if (class.is_some() && compares) || taint::selects_classified(cur, seg) {
                return branches_on_classified(&format!("the selector in ${p}"));
            }

            let here = || Path(rest[..i].to_vec());
            let upto = || Path(rest[..=i].to_vec());
            cur = match seg {
                Segment::All | Segment::Filter(..) | Segment::Project(_) => {
                    return err(format!("${p}: {}", novafile_text::PathError::NotSingular(Path(rest.to_vec()))));
                }
                _ => match step(cur, seg, &here, &upto) {
                    Ok(next) => next,
                    // Inside a classified container, a missing field must look like a present one:
                    // an error would tell which fields are set (proposal 0009.5.2).
                    Err(novafile_text::PathError::NotFound(_)) if class.is_some() => return self.charged(taint::taint_opt(Value::Null, class)),
                    Err(e) => return err(format!("${p}: {e}")),
                },
            };
        }

        self.charged(taint::taint_opt(cur.clone(), class))
    }

    /// Charge the cost of the value `v` that a reference yields, and return it.
    fn charged(&mut self, v: Value) -> Result<Value, TemplateError> {
        let c = Cost::of(&v);
        self.charge(c.nodes, c.bytes)?;
        Ok(v)
    }

    /// Interpolate a string: references must be scalars and are inserted in their text form.
    /// Also returns the strongest class among the references.
    fn text(&mut self, parts: &[Part]) -> Result<(String, Option<Class>), TemplateError> {
        let mut s = String::new();
        let mut class = None;
        for p in parts {
            match p {
                Part::Text(t) => {
                    // Referenced values are charged by `lookup`; literal text is charged here.
                    self.charge(0, t.len())?;
                    s.push_str(t);
                }
                Part::Ref(r) => {
                    let (v, own) = taint::untaint(self.lookup(r)?);
                    class = class.max(own);
                    match &v {
                        Value::String(x) => s.push_str(x),
                        Value::Int(i) => s.push_str(&i.to_string()),
                        Value::Float(f) => {
                            let _ = write!(s, "{f:?}");
                        }
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

        Ok((s, class))
    }

    /// Evaluate `n` as an operand of an `@if` condition, which must not be classified.
    fn operand(&mut self, n: &Node) -> Result<Value, TemplateError> {
        let v = self.node(n)?;
        if taint::class_in(&v).is_some() {
            return branches_on_classified("an @if condition");
        }

        Ok(v)
    }

    /// For a comparison with the literal `null`, whether the other side is present: a missing
    /// field counts as absent. Presence is not classified (proposal 0009.6.1), so a classified
    /// value may be tested this way, but not a field inside a classified container. `None` for
    /// every other comparison.
    fn presence(&mut self, a: &Node, b: &Node) -> Result<Option<bool>, TemplateError> {
        let ((Node::Lit(Value::Null), other) | (other, Node::Lit(Value::Null))) = (a, b) else {
            return Ok(None);
        };

        if let Node::Ref(p) = other
            && let Some((Segment::Field(last), parent)) = p.0.split_last()
            && !parent.is_empty()
        {
            // Which fields a classified container holds is classified, like the container.
            let parent = self.lookup(&Path(parent.to_vec()))?;
            if taint::tainted(&parent).is_some() {
                return branches_on_classified("a presence test inside a classified value");
            }

            if let Value::Record(r) = &parent
                && r.get(last).is_none()
            {
                return Ok(Some(false));
            }
        }

        let v = self.node(other)?;
        let inner = taint::tainted(&v).map_or(&v, |(_, inner)| inner);
        Ok(Some(*inner != Value::Null))
    }

    /// Evaluate an `@if` condition; plain tests must be bools.
    fn cond(&mut self, c: &Cond) -> Result<bool, TemplateError> {
        Ok(match c {
            Cond::Test(n) => match self.operand(n)? {
                Value::Bool(b) => b,
                other => {
                    return err(format!(
                        "conditions must be bool, found {} (compare explicitly, e.g. `$x != null`)",
                        other.kind()
                    ));
                }
            },
            Cond::Eq(a, b) => match self.presence(a, b)? {
                Some(present) => !present,
                None => self.operand(a)? == self.operand(b)?,
            },
            Cond::Ne(a, b) => match self.presence(a, b)? {
                Some(present) => present,
                None => self.operand(a)? != self.operand(b)?,
            },
            Cond::Not(c) => !self.cond(c)?,
            Cond::And(a, b) => self.cond(a)? && self.cond(b)?,
            Cond::Or(a, b) => self.cond(a)? || self.cond(b)?,
        })
    }

    /// The body of the first `@if` / `@else if` arm whose condition holds, else `els`.
    fn pick<'a, T>(&mut self, arms: &'a [(Cond, Vec<T>)], els: &'a [T]) -> Result<&'a [T], TemplateError> {
        for (c, body) in arms {
            if self.cond(c)? {
                return Ok(body);
            }
        }

        Ok(els)
    }

    /// Run `body` once per element of the iterable, with loop variables in a new scope.
    fn each<T>(&mut self, head: &ForHead, mut body: impl FnMut(&mut Self) -> Result<T, TemplateError>) -> Result<Vec<T>, TemplateError> {
        let iter = self.node(&head.iter)?;
        if taint::tainted(&iter).is_some() {
            return branches_on_classified("an @for loop");
        }

        let pairs: Vec<(Value, Value)> = match iter {
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

    /// Evaluate list items into `out`, expanding spreads, `@if` and `@for`.
    fn items(&mut self, items: &[Item], out: &mut Vec<Value>) -> Result<(), TemplateError> {
        for it in items {
            match it {
                Item::Node(n) => out.push(self.node(n)?),
                Item::Spread(n) => match self.spread(n)? {
                    Value::List(l) | Value::Tuple(l) | Value::Set(l) => out.extend(l),
                    other => return err(format!("cannot spread a {} into a list", other.kind())),
                },
                Item::If(arms, e) => {
                    let branch = self.pick(arms, e)?;
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
    fn entries(&mut self, entries: &[Entry], out: &mut Record, explicit: &mut HashSet<String>) -> Result<(), TemplateError> {
        for e in entries {
            match e {
                Entry::Field(k, n) => {
                    let key = match k {
                        Key::Lit(s) => s.clone(),
                        Key::Interp(parts) => match self.text(parts)? {
                            (_, Some(_)) => return branches_on_classified("a record key"),
                            (k, None) => k,
                        },
                    };

                    if explicit.contains(&key) {
                        return err(format!("duplicate key {key:?}"));
                    }

                    let v = self.node(n)?;
                    out.set(key.clone(), v);
                    explicit.insert(key);
                }
                Entry::Spread(n) => match self.spread(n)? {
                    Value::Record(r) => {
                        for (k, v) in r.into_entries() {
                            explicit.remove(&k);
                            out.set(k, v);
                        }
                    }
                    other => return err(format!("cannot spread a {} into a record", other.kind())),
                },
                Entry::If(arms, e) => {
                    let branch = self.pick(arms, e)?;
                    self.entries(branch, out, explicit)?;
                }
                Entry::For(h, body) => {
                    // Each iteration contributes entries to the same record.
                    let parts = self.each(h, |ev| {
                        let mut r = Record::new();
                        let mut ex = HashSet::new();
                        ev.entries(body, &mut r, &mut ex)?;
                        Ok(r)
                    })?;

                    for r in parts {
                        for (k, v) in r.into_entries() {
                            if !explicit.insert(k.clone()) {
                                return err(format!("duplicate key {k:?} produced by @for"));
                            }

                            out.set(k, v);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Evaluate the operand of a spread, which must not be classified as a whole: its length
    /// or keys would show in the output.
    fn spread(&mut self, n: &Node) -> Result<Value, TemplateError> {
        let v = self.node(n)?;
        if taint::tainted(&v).is_some() {
            return branches_on_classified("a spread");
        }

        Ok(v)
    }

    /// Evaluate a set element or map key, which must not be classified: a duplicate
    /// is an error, so the result would tell whether two values are equal.
    fn unique(&mut self, n: &Node, what: &str) -> Result<Value, TemplateError> {
        let v = self.node(n)?;
        if taint::class_in(&v).is_some() {
            return branches_on_classified(what);
        }

        Ok(v)
    }

    /// Evaluate a node to a value, charging every produced node against the budget.
    fn node(&mut self, n: &Node) -> Result<Value, TemplateError> {
        self.charge(1, 0)?;
        Ok(match n {
            Node::Lit(v) => {
                let c = Cost::of(v);
                self.charge(c.nodes - 1, c.bytes)?;
                v.clone()
            }
            Node::List(items) => {
                let mut out = Vec::new();
                self.items(items, &mut out)?;
                Value::List(out)
            }
            Node::Tuple(items) => Value::Tuple(items.iter().map(|n| self.node(n)).collect::<Result<_, _>>()?),
            Node::Set(items) => Value::set(items.iter().map(|n| self.unique(n, "a set element")).collect::<Result<_, _>>()?).map_err(TemplateError)?,
            Node::Map(entries) => Value::map(
                entries
                    .iter()
                    .map(|(k, v)| Ok((self.unique(k, "a map key")?, self.node(v)?)))
                    .collect::<Result<_, TemplateError>>()?,
            )
            .map_err(TemplateError)?,
            Node::Record(entries) => {
                let mut r = Record::new();
                self.entries(entries, &mut r, &mut HashSet::new())?;
                Value::Record(r)
            }
            Node::Variant(t, p) => Value::Variant(t.clone(), p.as_ref().map(|p| self.node(p).map(Box::new)).transpose()?),
            Node::Ref(p) => self.lookup(p)?,
            Node::Interp(parts) => {
                let (s, class) = self.text(parts)?;
                taint::taint_opt(Value::String(s), class)
            }
        })
    }
}
