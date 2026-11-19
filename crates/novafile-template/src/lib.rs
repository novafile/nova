//! NOVA templates (spec §9, template language 0.2).
//!
//! A template is a document whose first directive is `@template`. Rendering
//! turns it into a plain value; nothing downstream (schemas, hashing, binary
//! encoding, patches) ever sees template constructs. Templates substitute
//! values, never text, so there is no indentation or escaping to get wrong.
//!
//! Template language 0.2 adds an expression language (references, calls,
//! `or`/`and`/`not`, comparisons, `+`/`-`/`*` and `[for …]`/`{for …}`
//! comprehensions), user functions declared with `@def`, and a standard
//! library of 40 built-in functions ([`BUILTINS`]). The language stays
//! deliberately *total*: a function can only call built-ins and functions
//! defined above it, loops only iterate over values that already exist, and
//! every render is bounded by an output budget and a step budget
//! ([`Limits`]), so every render terminates.
//!
//! Records iterate by key in UTF-8 byte order, maps and sets in canonical
//! order (spec §2.12) and lists and tuples in order, in `@for`, in
//! comprehensions and in every built-in that walks a collection, so equal
//! arguments always render to equal values.
//!
//! `lower`, `upper` and `trim` use the Unicode tables of the Rust standard
//! library this crate is compiled with ([`char::UNICODE_VERSION`]). Template
//! language 0.2 pins Unicode 17.0.0; results differ only for code points whose
//! case mapping or `White_Space` property changed between the versions.

mod builtins;
mod eval;
mod json;
mod ops;

pub use builtins::BUILTINS;

use eval::{Eval, Func};
use novafile_schema::{SCHEMA_BUDGET, Schema, parse_type_within};
use novafile_text::ast::{Decl, Node, TemplateHeader};
use novafile_text::parse_document;
use novafile_value::{Record, Value};
use std::cell::Cell;
use std::fmt;
use std::path::{Component, PathBuf};
use std::rc::Rc;

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
    /// `@let` reuse) and the elements, fields and entries of every collection an expression
    /// builds, intermediate ones included.
    pub max_nodes: usize,
    /// Maximum total bytes of string and byte-string content produced, counting every copy
    /// (guards against strings doubled by repeated `${}` interpolation).
    pub max_bytes: usize,
    /// Maximum number of evaluation steps (operators, conditions, loop iterations and calls,
    /// counted exactly as in proposal 0006 §0006.7), across the render and its imports.
    pub max_steps: u64,
    /// Maximum nesting depth of `@import` chains.
    pub max_import_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_nodes: 1_000_000,
            max_bytes: 64 << 20,
            max_steps: 10_000_000,
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
    /// Output and step budgets and import depth.
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
/// # Errors
///
/// Returns a [`TemplateError`] if `src` is not a valid template (including a `@def` that
/// calls itself or a function defined below it); an argument is unknown, missing, or does not
/// match its `@param` type (resolved against `opts.schema`); an import fails, cycles, or nests
/// too deeply; evaluation fails; or the render exceeds the node, byte or step budget in
/// `opts.limits`.
pub fn render(src: &str, args: &Record, opts: &Options) -> Result<Value, TemplateError> {
    // Parameter patterns of the template and all its imports share one budget.
    render_inner(src, None, args, opts, &mut Vec::new(), &mut Cost::default(), &Cell::new(SCHEMA_BUDGET))
}

/// Render `src`, identified by `id` when imported. `stack` holds the importing documents
/// (for cycle detection) and `budget` the output and steps so far, shared across imports.
fn render_inner(
    src: &str,
    id: Option<&str>,
    args: &Record,
    opts: &Options,
    stack: &mut Vec<String>,
    budget: &mut Cost,
    patterns: &Cell<usize>,
) -> Result<Value, TemplateError> {
    let where_ = || id.map(|i| format!(" in {i}")).unwrap_or_default();
    let doc = parse_document(src).map_err(|e| TemplateError(format!("{e}{}", where_())))?;
    let Some(h) = &doc.template else {
        if !args.is_empty() {
            return err("arguments were given but the document is not a @template");
        }

        return doc.body.to_value().map_err(TemplateError);
    };

    // Load-time checks, before any argument is bound.
    check_calls(h, &doc.body).map_err(|e| TemplateError(format!("{}{}", e.0, where_())))?;
    let empty = Schema::default();
    let schema = opts.schema.unwrap_or(&empty);
    let mut funcs = Vec::new();
    for d in h.decls.iter().filter_map(|d| if let Decl::Def(d) = d { Some(d) } else { None }) {
        let ty = |t: &str, what: &str| parse_type_within(t, patterns).map_err(|m| TemplateError(format!("@def {} {what}: {m}", d.name)));
        let params = d.params.iter().map(|p| ty(&p.ty, &format!("parameter {}", p.name))).collect::<Result<_, _>>()?;
        let result = d.result.as_deref().map(|t| ty(t, "result")).transpose()?;
        funcs.push((d, params, result));
    }

    let mut ev = Eval::new(budget, opts.limits, schema, patterns);

    // Parameters.
    for k in args.keys() {
        if !h.params.iter().any(|p| &p.name == k) {
            return err(format!("unknown parameter {k:?}{}", where_()));
        }
    }

    for p in &h.params {
        let ty = parse_type_within(&p.ty, patterns).map_err(|m| TemplateError(format!("@param {}: {m}", p.name)))?;
        let v = match (args.get(&p.name), &p.default) {
            (Some(v), _) => v.clone(),
            (None, Some(d)) => ev.eval(d)?,
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

        ev.bind_global(&p.name, v)?;
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
        let v = ev.with_budget(|budget| render_inner(&isrc, Some(&iid), &Record::new(), opts, stack, budget, patterns))?;
        stack.pop();
        ev.bind_global(&imp.alias, v)?;
    }

    // Lets and defs, in order; each may use parameters, imports and earlier declarations.
    let mut funcs = funcs.into_iter();
    for d in &h.decls {
        match d {
            Decl::Let(l) => {
                let v = ev.eval(&l.value)?;
                ev.bind_global(&l.name, v)?;
            }
            Decl::Def(_) => {
                let (def, params, result) = funcs.next().expect("one entry per @def");
                ev.define(Rc::new(Func::new(def, params, result)));
            }
        }
    }

    ev.place(&doc.body)
}

/// Check every call in the header and body: a `@def` may call only built-ins and functions
/// defined above it, so a call of itself or of a later function is an error; so is a call of a
/// name that is neither, and a function defined twice.
fn check_calls(h: &TemplateHeader, body: &Node) -> Result<(), TemplateError> {
    let defs: Vec<&str> = h
        .decls
        .iter()
        .filter_map(|d| if let Decl::Def(d) = d { Some(d.name.as_str()) } else { None })
        .collect();
    // Calls in `n`, written where the first `above` functions exist, inside function `own`.
    let check = |n: &Node, above: usize, own: Option<&str>, what: &str| -> Result<(), TemplateError> {
        let mut bad = None;
        n.walk(&mut |x| {
            if let Node::Call(c) = x
                && bad.is_none()
            {
                let name = c.name.as_str();
                if own == Some(name) {
                    bad = Some(format!("{what} calls itself; recursion is not allowed"));
                } else if defs[above..].contains(&name) && !defs[..above].contains(&name) {
                    bad = Some(format!(
                        "{what} calls {name}, which is defined below it; a @def can only be called below its definition"
                    ));
                } else if !defs[..above].contains(&name) && !BUILTINS.contains(&name) {
                    bad = Some(format!("{what} calls unknown function {name}"));
                }
            }
        });
        bad.map_or(Ok(()), err)
    };

    for p in &h.params {
        if let Some(d) = &p.default {
            check(d, 0, None, &format!("@param {}", p.name))?;
        }
    }

    let mut above = 0;
    for d in &h.decls {
        match d {
            Decl::Let(l) => check(&l.value, above, None, &format!("@let {}", l.name))?,
            Decl::Def(d) => {
                if defs[..above].contains(&d.name.as_str()) {
                    return err(format!("@def {} is already defined", d.name));
                }

                let what = format!("@def {}", d.name);
                for p in &d.params {
                    if let Some(x) = &p.default {
                        check(x, above, Some(&d.name), &what)?;
                    }
                }

                check(&d.body, above, Some(&d.name), &what)?;
                above += 1;
            }
        }
    }

    check(body, above, None, "the template body")
}

/// Output produced and work done: value nodes, string/byte-string content bytes and steps.
#[derive(Clone, Copy, Default)]
pub(crate) struct Cost {
    /// Value nodes, containers and their contents alike.
    pub(crate) nodes: usize,
    /// Bytes of string and byte-string content.
    pub(crate) bytes: usize,
    /// Evaluation steps (only accumulated in the shared budget).
    pub(crate) steps: u64,
}

impl Cost {
    /// The nodes and content bytes of `v`.
    pub(crate) fn of(v: &Value) -> Cost {
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
