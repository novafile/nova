//! Syntax tree produced by the parser. Data documents contain only literal
//! nodes; template documents (spec §9) may additionally contain references,
//! interpolations, spreads, conditionals and loops.

use crate::path::Path;
use novafile_value::{Record, Value};

/// A value node: a literal or container, or in templates a reference or interpolation.
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
    /// `"text ${path} text"` (templates only).
    Interp(Vec<Part>),
}

/// A piece of an interpolated string.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// Literal text, with escapes already resolved.
    Text(String),
    /// `${path}`: a reference whose value is spliced in.
    Ref(Path),
}

/// An element of a list body.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A plain element.
    Node(Node),
    /// `...expr`: splice the elements of a list (templates only).
    Spread(Node),
    /// `@if cond:` with the then-items and the (possibly empty) `@else` items.
    If(Box<Cond>, Vec<Item>, Vec<Item>),
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
    /// `@if cond:` with the then-entries and the (possibly empty) `@else` entries.
    If(Box<Cond>, Vec<Entry>, Vec<Entry>),
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

/// The header of a `@for [index,] var in iter:` loop.
#[derive(Clone, Debug, PartialEq)]
pub struct ForHead {
    /// Name bound to the position of the current element, if given.
    pub index: Option<String>,
    /// Name bound to the current element.
    pub var: String,
    /// Expression producing the collection to iterate over.
    pub iter: Node,
}

/// The condition of an `@if` (templates only).
#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    /// A bare operand, tested for truthiness by the template renderer.
    Test(Node),
    /// `a == b`.
    Eq(Node, Node),
    /// `a != b`.
    Ne(Node, Node),
    /// `not c`.
    Not(Box<Cond>),
    /// `a and b`.
    And(Box<Cond>, Box<Cond>),
    /// `a or b`.
    Or(Box<Cond>, Box<Cond>),
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
            Node::Ref(_) | Node::Interp(_) => return Err(TPL.to_string()),
        })
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
    /// `@param` declarations.
    pub params: Vec<Param>,
    /// `@import` declarations.
    pub imports: Vec<Import>,
    /// `@let` bindings.
    pub lets: Vec<Let>,
}

/// A parsed text document.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Header directives in source order, except `@template`, `@param`, `@let` and `@import`.
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
