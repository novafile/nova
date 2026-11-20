//! Syntax tree produced by the parser. Data documents contain only literal
//! nodes; template documents (spec §9) may additionally contain references,
//! interpolations, spreads, conditionals, loops and expressions (calls,
//! operators and comprehensions, template language 0.2).

use crate::path::Path;
use crate::version::LangVersion;
use novafile_value::{Record, Value};

/// A value node: a literal or container, or in templates a reference, interpolation or
/// expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// A scalar: keyword, number, string or scalar typed literal such as `@decimal("1.5")`.
    Lit(Value),
    /// `[...]` or a block list (tables included); items may be template spreads or control blocks.
    List(Vec<Item>),
    /// `(a, b)`: a fixed-length tuple.
    Tuple(Vec<Node>),
    /// `@set[...]`.
    Set(Vec<Node>),
    /// `@map{k => v, ...}`, as key/value node pairs in source order.
    Map(Vec<(Node, Node)>),
    /// `{...}` or a block record; entries may be template spreads or control blocks.
    Record(Vec<Entry>),
    /// `!tag` or `!tag(payload)`.
    Variant(String, Option<Box<Node>>),
    /// `$name.path` (templates only).
    Ref(Path),
    /// `"text ${expr} text"` (templates only).
    Interp(Vec<Part>),
    /// `$name(arg, …)`: a call of a built-in or a `@def` (templates only).
    Call(Call),
    /// `a OP b` (templates only).
    Binary(BinOp, Box<Node>, Box<Node>),
    /// `not a` (templates only).
    Not(Box<Node>),
    /// `[for …]` or `{for …}` (templates only).
    Comp(Box<Comp>),
}

/// A call `$name(arg, …)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// The function name.
    pub name: String,
    /// The argument expressions, in order.
    pub args: Vec<Node>,
    /// For each argument, whether it is a bare word that classified as a string (`name`, not
    /// `"name"` or `$name`); used by the `bare-argument` lint.
    pub bare: Vec<bool>,
    /// Whether the call is written inside an interpolation `${…}`.
    pub interp: bool,
    /// Byte offset of the call in the source (the `$`, or the name when the `$` is implied).
    pub offset: usize,
}

/// A binary operator (template language 0.2), loosest binding first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// `or`, short-circuit.
    Or,
    /// `and`, short-circuit.
    And,
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
}

impl BinOp {
    /// The operator as written.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Or => "or",
            BinOp::And => "and",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
        }
    }
}

/// A list comprehension `[for …: value]` or record comprehension `{for …: key: value}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Comp {
    /// Loop variables and source, as for `@for`.
    pub head: ForHead,
    /// The `if` filter, if any.
    pub filter: Option<Node>,
    /// The key expression of a record comprehension; `None` for a list comprehension.
    pub key: Option<Node>,
    /// The element (or field value) expression.
    pub value: Node,
}

/// A piece of an interpolated string.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// Literal text, with escapes already resolved.
    Text(String),
    /// `${expr}`: an expression whose scalar value is spliced in as text.
    Expr(Node),
}

/// An element of a list body.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A plain element.
    Node(Node),
    /// `...expr`: splice the elements of a list (templates only).
    Spread(Node),
    /// `@if cond:` and its `@else if` arms as `(cond, items)` in order, then the (possibly empty)
    /// `@else` items.
    If(Vec<(Node, Vec<Item>)>, Vec<Item>),
    /// `@for [index,] var in expr:` with the items produced per iteration.
    For(ForHead, Vec<Item>),
}

/// An entry of a record body.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// `key: value`.
    Field(Key, Node),
    /// `...expr`: merge the fields of a record (templates only).
    Spread(Node),
    /// `@if cond:` and its `@else if` arms as `(cond, entries)` in order, then the (possibly
    /// empty) `@else` entries.
    If(Vec<(Node, Vec<Entry>)>, Vec<Entry>),
    /// `@for [index,] var in expr:` with the entries produced per iteration.
    For(ForHead, Vec<Entry>),
}

/// A record key.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    /// A bare identifier or a quoted string without interpolation.
    Lit(String),
    /// A quoted key with `${...}` interpolation (templates only).
    Interp(Vec<Part>),
}

/// The header of a `@for [index,] var in iter:` loop or of a comprehension.
#[derive(Clone, Debug, PartialEq)]
pub struct ForHead {
    /// Name bound to the position (or key) of the current element, if given.
    pub index: Option<String>,
    /// Name bound to the current element.
    pub var: String,
    /// Expression producing the collection to iterate over.
    pub iter: Node,
}

impl Node {
    /// Convert a template-free tree into a value.
    ///
    /// # Errors
    ///
    /// Returns a message if the tree contains a template construct.
    pub fn to_value(&self) -> Result<Value, String> {
        /// Error message for a template construct, which has no plain value.
        const TPL: &str = "template construct in a non-template document";
        Ok(match self {
            Node::Lit(v) => v.clone(),
            Node::List(items) => Value::List(
                items
                    .iter()
                    .map(|i| match i {
                        Item::Node(n) => n.to_value(),
                        _ => Err(TPL.to_string()),
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Node::Tuple(items) => Value::Tuple(items.iter().map(Node::to_value).collect::<Result<_, _>>()?),
            Node::Set(items) => Value::set(items.iter().map(Node::to_value).collect::<Result<_, _>>()?)?,
            Node::Map(entries) => Value::map(entries.iter().map(|(k, v)| Ok((k.to_value()?, v.to_value()?))).collect::<Result<_, String>>()?)?,
            Node::Record(entries) => {
                let mut r = Record::new();
                for e in entries {
                    match e {
                        Entry::Field(Key::Lit(k), n) => r.insert(k.clone(), n.to_value()?).map_err(|e| e.to_string())?,
                        _ => return Err(TPL.to_string()),
                    }
                }

                Value::Record(r)
            }
            Node::Variant(tag, p) => Value::Variant(tag.clone(), p.as_ref().map(|p| p.to_value().map(Box::new)).transpose()?),
            Node::Ref(_) | Node::Interp(_) | Node::Call(_) | Node::Binary(..) | Node::Not(_) | Node::Comp(_) => return Err(TPL.to_string()),
        })
    }

    /// Call `f` on this node and on every node nested in it, in source order (pre-order): the
    /// elements of collections, the arms and bodies of `@if` and `@for`, interpolated parts and
    /// keys, call arguments, operands and the parts of comprehensions.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        f(self);
        match self {
            Node::Lit(_) | Node::Ref(_) => {}
            Node::List(items) => items.iter().for_each(|i| i.walk(f)),
            Node::Tuple(ns) | Node::Set(ns) => ns.iter().for_each(|n| n.walk(f)),
            Node::Map(es) => es.iter().for_each(|(k, v)| {
                k.walk(f);
                v.walk(f);
            }),
            Node::Record(es) => es.iter().for_each(|e| e.walk(f)),
            Node::Variant(_, p) => p.iter().for_each(|p| p.walk(f)),
            Node::Interp(parts) => walk_parts(parts, f),
            Node::Call(c) => c.args.iter().for_each(|a| a.walk(f)),
            Node::Binary(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
            Node::Not(a) => a.walk(f),
            Node::Comp(c) => {
                c.head.iter.walk(f);
                c.filter.iter().chain(&c.key).for_each(|n| n.walk(f));
                c.value.walk(f);
            }
        }
    }
}

/// [`Node::walk`] over the expressions of interpolated parts.
fn walk_parts<'a>(parts: &'a [Part], f: &mut dyn FnMut(&'a Node)) {
    for p in parts {
        if let Part::Expr(n) = p {
            n.walk(f);
        }
    }
}

impl Item {
    /// [`Node::walk`] over the nodes of a list item.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        match self {
            Item::Node(n) | Item::Spread(n) => n.walk(f),
            Item::If(arms, els) => {
                for (c, body) in arms {
                    c.walk(f);
                    for i in body {
                        i.walk(f);
                    }
                }

                for i in els {
                    i.walk(f);
                }
            }
            Item::For(h, body) => {
                h.iter.walk(f);
                for i in body {
                    i.walk(f);
                }
            }
        }
    }
}

impl Entry {
    /// [`Node::walk`] over the nodes of a record entry, including interpolated keys.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        match self {
            Entry::Field(k, n) => {
                if let Key::Interp(parts) = k {
                    walk_parts(parts, f);
                }

                n.walk(f);
            }
            Entry::Spread(n) => n.walk(f),
            Entry::If(arms, els) => {
                for (c, body) in arms {
                    c.walk(f);
                    for e in body {
                        e.walk(f);
                    }
                }

                for e in els {
                    e.walk(f);
                }
            }
            Entry::For(h, body) => {
                h.iter.walk(f);
                for e in body {
                    e.walk(f);
                }
            }
        }
    }
}

/// A header directive line such as `@schema infrastructure@1.0.0`.
#[derive(Clone, Debug, PartialEq)]
pub struct Directive {
    /// Directive name without the `@`.
    pub name: String,
    /// Rest of the line after the name, trimmed and without a trailing comment.
    pub arg: String,
    /// 1-based source line of the directive.
    pub line: usize,
}

/// A `@param name[?]: type [= default]` declaration in a template header.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// Parameter name.
    pub name: String,
    /// Declared with `?`: the parameter may be omitted.
    pub optional: bool,
    /// Type expression in schema syntax (spec §6), e.g. `u16 @range(1..10)`.
    pub ty: String,
    /// Value used when the parameter is not supplied.
    pub default: Option<Node>,
}

/// A `@let name = value` (or `@let name:` with a block) binding in a template header.
#[derive(Clone, Debug, PartialEq)]
pub struct Let {
    /// Bound name.
    pub name: String,
    /// Bound expression.
    pub value: Node,
}

/// A `@def name(params)[: result] = expr` function (or its block form) in a template header.
#[derive(Clone, Debug, PartialEq)]
pub struct Def {
    /// Function name.
    pub name: String,
    /// Parameters in order; there is at least one, and those with defaults come last.
    pub params: Vec<DefParam>,
    /// Result type in schema syntax; `None` means `any`.
    pub result: Option<String>,
    /// The body: an expression, or a block parsed like a template body.
    pub body: Node,
    /// Byte offset of the `@def` directive in the source.
    pub offset: usize,
}

/// A parameter of a `@def`: `name: type [= default]`.
#[derive(Clone, Debug, PartialEq)]
pub struct DefParam {
    /// Parameter name.
    pub name: String,
    /// Type expression in schema syntax (spec §6).
    pub ty: String,
    /// Value used when the argument is not given.
    pub default: Option<Node>,
}

/// A `@let` or `@def` declaration; they are processed in source order.
#[derive(Clone, Debug, PartialEq)]
pub enum Decl {
    /// A `@let` binding.
    Let(Let),
    /// A `@def` function.
    Def(Def),
}

/// An `@import "path" as alias` declaration in a template header.
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    /// The import path, unescaped.
    pub path: String,
    /// Name the imported document is bound to.
    pub alias: String,
}

/// Declarations following `@template`, each kind in source order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TemplateHeader {
    /// The declared template language version, if `@template` names one.
    pub version: Option<LangVersion>,
    /// `@param` declarations.
    pub params: Vec<Param>,
    /// `@import` declarations.
    pub imports: Vec<Import>,
    /// `@let` bindings and `@def` functions, in source order.
    pub decls: Vec<Decl>,
}

/// A parsed text document.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Header directives in source order, except `@template`, `@param`, `@let`, `@def` and
    /// `@import`.
    pub directives: Vec<Directive>,
    /// Present iff the document starts with `@template`.
    pub template: Option<TemplateHeader>,
    /// The body; an empty body is an empty record.
    pub body: Node,
}

impl Document {
    /// The argument of the first directive called `name` (without `@`), if any.
    #[must_use]
    pub fn directive(&self, name: &str) -> Option<&str> {
        self.directives.iter().find(|d| d.name == name).map(|d| d.arg.as_str())
    }

    /// Whether this is a `@template` document.
    #[must_use]
    pub fn is_template(&self) -> bool {
        self.template.is_some()
    }
}
