//! Evaluation of template bodies and expressions: scopes, `@def` calls, control flow, the
//! output budget and the step budget.

use crate::{Cost, Limits, TemplateError, builtins, err, ops};
use novafile_schema::{Pattern, Schema, TypeRef};
use novafile_text::ast::{BinOp, Call, Comp, Def, Entry, ForHead, Item, Key, Node, Part};
use novafile_text::{Path, Segment};
use novafile_value::{Record, Value};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Maximum nesting of `@def` calls. The call graph is acyclic, so this only matters for very
/// long chains of definitions, which would otherwise exhaust the native stack.
const MAX_CALL_DEPTH: usize = 256;

/// A `@def` ready to be called: its declaration, parsed types and lexical scope.
pub(crate) struct Func<'a> {
    /// The declaration.
    def: &'a Def,
    /// The parameter types, in order.
    params: Vec<TypeRef>,
    /// The result type, if declared.
    result: Option<TypeRef>,
    /// Number of template-level bindings visible in the body (those above the definition);
    /// set by [`Eval::define`].
    globals: usize,
    /// Number of functions visible in the body (those above the definition).
    funcs: usize,
}

impl<'a> Func<'a> {
    /// A function for `def` with its parsed parameter and result types.
    pub(crate) fn new(def: &'a Def, params: Vec<TypeRef>, result: Option<TypeRef>) -> Self {
        Func {
            def,
            params,
            result,
            globals: 0,
            funcs: 0,
        }
    }
}

/// Evaluator state for one template.
pub(crate) struct Eval<'a> {
    /// Template-level bindings (parameters, imports, `@let`s) in definition order.
    globals: Vec<(String, Value)>,
    /// How many of `globals` are visible (fewer inside a `@def` body).
    visible: usize,
    /// Local scopes, innermost last: `@def` parameters and loop variables.
    locals: Vec<Vec<(String, Value)>>,
    /// Functions defined so far, in order.
    funcs: Vec<Rc<Func<'a>>>,
    /// How many of `funcs` are visible (fewer inside a `@def` body).
    visible_funcs: usize,
    /// Output and steps so far, shared with importing templates.
    budget: &'a mut Cost,
    /// The budget maxima.
    limits: Limits,
    /// Schema for named types in `@def` signatures.
    schema: &'a Schema,
    /// Pattern compilation budget shared with parameter types.
    patterns: &'a Cell<usize>,
    /// Compiled `matches` patterns by text.
    regexes: HashMap<String, Pattern>,
    /// Current nesting of `@def` calls.
    calls: usize,
}

/// Whether `v` is a bool, or an error naming `what` needs one.
fn want_bool(v: &Value, what: &str) -> Result<bool, TemplateError> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => err(format!("{what} must be bool, found {} (compare explicitly, e.g. `$x != null`)", other.kind())),
    }
}

impl<'a> Eval<'a> {
    /// A fresh evaluator charging `budget`.
    pub(crate) fn new(budget: &'a mut Cost, limits: Limits, schema: &'a Schema, patterns: &'a Cell<usize>) -> Self {
        Eval {
            globals: Vec::new(),
            visible: usize::MAX,
            locals: Vec::new(),
            funcs: Vec::new(),
            visible_funcs: usize::MAX,
            budget,
            limits,
            schema,
            patterns,
            regexes: HashMap::new(),
            calls: 0,
        }
    }

    /// Run `f` with the shared budget (for rendering an import).
    pub(crate) fn with_budget<T>(&mut self, f: impl FnOnce(&mut Cost) -> T) -> T {
        f(self.budget)
    }

    /// Count `nodes` produced nodes and `bytes` content bytes against the budget; fails once
    /// either limit is exceeded.
    pub(crate) fn charge(&mut self, nodes: usize, bytes: usize) -> Result<(), TemplateError> {
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

    /// Fail if producing `nodes` more nodes and `bytes` more content bytes would exceed the
    /// output budget, without charging them: built-ins call this before building a result that
    /// can be much larger than their arguments, so the budget also bounds memory.
    pub(crate) fn reserve(&self, nodes: usize, bytes: usize) -> Result<(), TemplateError> {
        if self.budget.nodes.saturating_add(nodes) > self.limits.max_nodes {
            return err(format!("template output exceeds the limit of {} nodes", self.limits.max_nodes));
        }

        if self.budget.bytes.saturating_add(bytes) > self.limits.max_bytes {
            return err(format!(
                "template output exceeds the limit of {} bytes of string and bytes content",
                self.limits.max_bytes
            ));
        }

        Ok(())
    }

    /// Count `n` evaluation steps; fails once the step budget is exceeded.
    pub(crate) fn step(&mut self, n: u64) -> Result<(), TemplateError> {
        self.budget.steps = self.budget.steps.saturating_add(n);
        if self.budget.steps > self.limits.max_steps {
            return err(format!("template evaluation exceeds the limit of {} steps", self.limits.max_steps));
        }

        Ok(())
    }

    /// Bind a template-level `name`; rebinding a name is an error.
    pub(crate) fn bind_global(&mut self, name: &str, v: Value) -> Result<(), TemplateError> {
        if self.globals.iter().any(|(n, _)| n == name) {
            return err(format!("{name:?} is already defined"));
        }

        self.globals.push((name.to_owned(), v));
        Ok(())
    }

    /// Bind `name` in the innermost local scope; redefining a name in the same scope is an error.
    fn bind_local(&mut self, name: &str, v: Value) -> Result<(), TemplateError> {
        let scope = self.locals.last_mut().expect("a local scope");
        if scope.iter().any(|(n, _)| n == name) {
            return err(format!("{name:?} is already defined"));
        }

        scope.push((name.to_owned(), v));
        Ok(())
    }

    /// Make `f` callable by later declarations and the body; it sees what is defined so far.
    pub(crate) fn define(&mut self, mut f: Rc<Func<'a>>) {
        let me = Rc::get_mut(&mut f).expect("a new function");
        me.globals = self.globals.len();
        me.funcs = self.funcs.len();
        self.funcs.push(f);
    }

    /// Compile (once) and match the `matches` pattern `pat` against `s`.
    pub(crate) fn is_match(&mut self, pat: &str, s: &str) -> Result<bool, String> {
        if !self.regexes.contains_key(pat) {
            let p = Pattern::new_within(pat, self.patterns)?;
            self.regexes.insert(pat.to_owned(), p);
        }

        Ok(self.regexes[pat].is_match(s))
    }

    /// Resolve a `$name.path` reference, searching local scopes from the innermost outwards,
    /// then the visible template-level bindings. Reading a value copies it, so a reference
    /// costs [`weight`] steps: nothing for a scalar, but a reference to a large value read in a
    /// loop cannot do unbounded work.
    fn lookup(&mut self, p: &Path) -> Result<Value, TemplateError> {
        let v = self.find(p)?;
        self.step(weight(&v))?;
        Ok(v)
    }

    /// [`Self::lookup`] without the step charge.
    fn find(&self, p: &Path) -> Result<Value, TemplateError> {
        let Some((Segment::Field(name), rest)) = p.0.split_first() else {
            return err(format!("invalid reference ${p}"));
        };

        let globals = &self.globals[..self.visible.min(self.globals.len())];
        let base = self
            .locals
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .chain(globals.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
            .ok_or_else(|| TemplateError(format!("undefined reference ${name}")))?;
        Ok(Path(rest.to_vec()).get(base).map_err(|e| TemplateError(format!("${p}: {e}")))?.clone())
    }

    /// Evaluate an expression.
    pub(crate) fn eval(&mut self, n: &Node) -> Result<Value, TemplateError> {
        self.eval_owned(n).map(|(v, _)| v)
    }

    /// Evaluate `n` where its value is placed (into a collection or the output): a value that
    /// was not built here, such as a referenced one, is a copy and counts against the output
    /// budget now.
    pub(crate) fn place(&mut self, n: &Node) -> Result<Value, TemplateError> {
        let (v, owned) = self.eval_owned(n)?;
        if !owned {
            let c = Cost::of(&v);
            self.charge(c.nodes, c.bytes)?;
        }

        Ok(v)
    }

    /// Evaluate `n`; the flag says whether the value's nodes were already charged as built.
    fn eval_owned(&mut self, n: &Node) -> Result<(Value, bool), TemplateError> {
        Ok(match n {
            Node::Lit(v) => {
                let c = Cost::of(v);
                self.charge(c.nodes, c.bytes)?;
                (v.clone(), true)
            }
            Node::List(items) => {
                self.charge(1, 0)?;
                let mut out = Vec::new();
                self.items(items, &mut out)?;
                (Value::List(out), true)
            }
            Node::Tuple(items) => {
                self.charge(1, 0)?;
                (Value::Tuple(items.iter().map(|n| self.place(n)).collect::<Result<_, _>>()?), true)
            }
            Node::Set(items) => {
                self.charge(1, 0)?;
                let items = items.iter().map(|n| self.place(n)).collect::<Result<_, _>>()?;
                (Value::set(items).map_err(TemplateError)?, true)
            }
            Node::Map(entries) => {
                self.charge(1, 0)?;
                let entries = entries
                    .iter()
                    .map(|(k, v)| Ok((self.place(k)?, self.place(v)?)))
                    .collect::<Result<_, TemplateError>>()?;
                (Value::map(entries).map_err(TemplateError)?, true)
            }
            Node::Record(entries) => {
                self.charge(1, 0)?;
                let mut r = Record::new();
                self.entries(entries, &mut r, &mut HashSet::new())?;
                (Value::Record(r), true)
            }
            Node::Variant(t, p) => {
                self.charge(1, 0)?;
                (Value::Variant(t.clone(), p.as_ref().map(|p| self.place(p).map(Box::new)).transpose()?), true)
            }
            Node::Ref(p) => (self.lookup(p)?, false),
            Node::Interp(parts) => {
                self.charge(1, 0)?;
                (Value::String(self.text(parts)?), true)
            }
            Node::Call(c) => self.call(c)?,
            Node::Binary(op, a, b) => (self.binary(*op, a, b)?, false),
            Node::Not(a) => {
                self.step(1)?;
                let v = self.eval(a)?;
                (Value::Bool(!want_bool(&v, "the operand of `not`")?), false)
            }
            Node::Comp(c) => (self.comp(c)?, true),
        })
    }

    /// Apply a binary operator; `and` and `or` short-circuit.
    fn binary(&mut self, op: BinOp, a: &Node, b: &Node) -> Result<Value, TemplateError> {
        if matches!(op, BinOp::And | BinOp::Or) {
            self.step(1)?;
            let what = format!("the operands of `{}`", op.as_str());
            let l = want_bool(&self.eval(a)?, &what)?;
            if l == (op == BinOp::Or) {
                return Ok(Value::Bool(l));
            }

            return Ok(Value::Bool(want_bool(&self.eval(b)?, &what)?));
        }

        let (lhs, rhs) = (self.eval(a)?, self.eval(b)?);
        // Comparing walks both operands, so a comparison also costs their weight.
        let w = if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
            0
        } else {
            weight(&lhs) + weight(&rhs)
        };
        self.step(1 + w)?;
        ops::binary(op, &lhs, &rhs).map_err(TemplateError)
    }

    /// Call a `@def` visible here, or else a built-in.
    fn call(&mut self, c: &Call) -> Result<(Value, bool), TemplateError> {
        let args = c.args.iter().map(|a| self.eval(a)).collect::<Result<Vec<_>, _>>()?;
        let visible = self.visible_funcs.min(self.funcs.len());
        if let Some(f) = self.funcs[..visible].iter().rev().find(|f| f.def.name == c.name).cloned() {
            return self.call_def(&f, args);
        }

        let v = builtins::call(self, &c.name, args)?;
        let owned = matches!(
            v,
            Value::String(_) | Value::Bytes(_) | Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::Record(_) | Value::Map(_)
        );
        Ok((v, owned))
    }

    /// Call `f`: check the arguments against the parameter types (without converting them),
    /// evaluate the body in the function's lexical scope and check the result.
    fn call_def(&mut self, f: &Func<'a>, args: Vec<Value>) -> Result<(Value, bool), TemplateError> {
        self.step(1)?;
        let (name, params) = (&f.def.name, &f.def.params);
        let required = params.iter().filter(|p| p.default.is_none()).count();
        if args.len() < required || args.len() > params.len() {
            let n = if required == params.len() {
                format!("{required}")
            } else {
                format!("{required} to {}", params.len())
            };
            return err(format!("{name} takes {n} argument(s), but {} were given", args.len()));
        }

        if self.calls >= MAX_CALL_DEPTH {
            return err(format!("@def calls nest deeper than {MAX_CALL_DEPTH}"));
        }

        let saved = (std::mem::replace(&mut self.locals, vec![Vec::new()]), self.visible, self.visible_funcs);
        self.visible = f.globals;
        self.visible_funcs = f.funcs;
        self.calls += 1;
        let r = self.run_def(f, args);
        self.calls -= 1;
        (self.locals, self.visible, self.visible_funcs) = saved;
        r
    }

    /// Bind the parameters of `f` and evaluate its body (scope already switched).
    fn run_def(&mut self, f: &Func<'a>, args: Vec<Value>) -> Result<(Value, bool), TemplateError> {
        let name = &f.def.name;
        let mut args = args.into_iter();
        for (p, ty) in f.def.params.iter().zip(&f.params) {
            let v = match args.next() {
                Some(v) => v,
                None => self.eval(p.default.as_ref().expect("checked arity"))?,
            };

            self.schema
                .check_as(&v, ty)
                .map_err(|is| TemplateError(format!("argument {:?} of {name} does not match `{}`: {}", p.name, p.ty, issues(&is))))?;
            self.bind_local(&p.name, v)?;
        }

        let (v, owned) = self.eval_owned(&f.def.body)?;
        if let (Some(t), Some(text)) = (&f.result, &f.def.result) {
            self.schema
                .check_as(&v, t)
                .map_err(|is| TemplateError(format!("the result of {name} does not match `{text}`: {}", issues(&is))))?;
        }

        Ok((v, owned))
    }

    /// Interpolate a string: expressions must be scalars and are inserted as their text.
    fn text(&mut self, parts: &[Part]) -> Result<String, TemplateError> {
        let mut s = String::new();
        for p in parts {
            let t = match p {
                Part::Text(t) => t.clone(),
                Part::Expr(n) => {
                    let v = self.eval(n)?;
                    ops::scalar_text(&v).ok_or_else(|| {
                        let at = if let Node::Ref(r) = n { format!(" (${r})") } else { String::new() };
                        TemplateError(format!("cannot interpolate a {}{at}; only scalars can appear in strings", v.kind()))
                    })?
                }
            };

            self.charge(0, t.len())?;
            s.push_str(&t);
        }

        Ok(s)
    }

    /// Evaluate an `@if` or `@else if` condition, which must be a bool.
    fn cond(&mut self, c: &Node) -> Result<bool, TemplateError> {
        self.step(1)?;
        want_bool(&self.eval(c)?, "conditions")
    }

    /// The body of the first `@if` / `@else if` arm whose condition holds, else `els`.
    fn pick<'n, T>(&mut self, arms: &'n [(Node, Vec<T>)], els: &'n [T]) -> Result<&'n [T], TemplateError> {
        for (c, body) in arms {
            if self.cond(c)? {
                return Ok(body);
            }
        }

        Ok(els)
    }

    /// Run `body` once per element of the source, in iteration order (§0006.6), with the loop
    /// variables in a new scope; `None` results are skipped.
    fn each<T>(&mut self, head: &ForHead, what: &str, mut body: impl FnMut(&mut Self) -> Result<Option<T>, TemplateError>) -> Result<Vec<T>, TemplateError> {
        let src = self.eval(&head.iter)?;
        let kind = src.kind();
        let pairs = ops::iterate(src).ok_or_else(|| TemplateError(format!("{what} needs a list, set, tuple, record or map, found {kind}")))?;
        let mut out = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            self.step(1)?;
            self.locals.push(Vec::new());
            let r = (|| {
                if let Some(i) = &head.index {
                    self.bind_local(i, k)?;
                }

                self.bind_local(&head.var, v)?;
                body(self)
            })();
            self.locals.pop();
            if let Some(x) = r? {
                out.push(x);
            }
        }

        Ok(out)
    }

    /// Evaluate a list or record comprehension.
    fn comp(&mut self, c: &Comp) -> Result<Value, TemplateError> {
        self.charge(1, 0)?;
        let fields = self.each(&c.head, "a comprehension", |ev| {
            if let Some(f) = &c.filter {
                ev.step(1)?;
                if !want_bool(&ev.eval(f)?, "the comprehension filter")? {
                    return Ok(None);
                }
            }

            let key = match &c.key {
                None => None,
                Some(k) => match ev.eval(k)? {
                    Value::String(s) => Some(s),
                    other => return err(format!("the key of a record comprehension must be a string, found {}", other.kind())),
                },
            };

            Ok(Some((key, ev.place(&c.value)?)))
        })?;

        if c.key.is_none() {
            return Ok(Value::List(fields.into_iter().map(|(_, v)| v).collect()));
        }

        let mut r = Record::with_capacity(fields.len());
        for (k, v) in fields {
            let k = k.expect("record comprehension key");
            if r.contains_key(&k) {
                return err(format!("duplicate key {k:?} produced by a comprehension"));
            }

            r.set(k, v);
        }

        Ok(Value::Record(r))
    }

    /// Evaluate list items into `out`, expanding spreads, `@if` and `@for`.
    fn items(&mut self, items: &[Item], out: &mut Vec<Value>) -> Result<(), TemplateError> {
        for it in items {
            match it {
                Item::Node(n) => out.push(self.place(n)?),
                Item::Spread(n) => match self.place(n)? {
                    v @ (Value::List(_) | Value::Tuple(_) | Value::Set(_)) => {
                        out.extend(ops::iterate(v).into_iter().flatten().map(|(_, v)| v));
                    }
                    other => return err(format!("cannot spread a {} into a list", other.kind())),
                },
                Item::If(arms, e) => {
                    let branch = self.pick(arms, e)?;
                    self.items(branch, out)?;
                }
                Item::For(h, body) => {
                    for chunk in self.each(h, "@for", |ev| {
                        let mut v = Vec::new();
                        ev.items(body, &mut v)?;
                        Ok(Some(v))
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
                        Key::Interp(parts) => self.text(parts)?,
                    };

                    if explicit.contains(&key) {
                        return err(format!("duplicate key {key:?}"));
                    }

                    let v = self.place(n)?;
                    out.set(key.clone(), v);
                    explicit.insert(key);
                }
                Entry::Spread(n) => match self.place(n)? {
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
                    let parts = self.each(h, "@for", |ev| {
                        let mut r = Record::new();
                        let mut ex = HashSet::new();
                        ev.entries(body, &mut r, &mut ex)?;
                        Ok(Some(r))
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
}

/// The step weight of reading or comparing `v`, beyond the step of the operation itself: one per
/// node below the top (elements, fields, entries and payloads, recursively) and one per full 64
/// bytes of string and byte-string content. Scalars other than long strings weigh nothing, so
/// templates that do not handle large values count steps exactly as in proposal 0006 §0006.7.
fn weight(v: &Value) -> u64 {
    let c = Cost::of(v);
    (c.nodes - 1 + c.bytes / 64) as u64
}

/// Schema issues as one message.
fn issues(is: &[novafile_schema::Issue]) -> String {
    is.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("; ")
}
