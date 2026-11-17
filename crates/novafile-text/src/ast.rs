//! Syntax tree produced by the parser. Data documents contain only literal
//! nodes; template documents (spec §9) may additionally contain references,
//! interpolations, spreads, conditionals and loops.

use crate::path::Path;
use novafile_value::{Record, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Lit(Value),
    List(Vec<Item>),
    Tuple(Vec<Node>),
    Set(Vec<Node>),
    Map(Vec<(Node, Node)>),
    Record(Vec<Entry>),
    Variant(String, Option<Box<Node>>),
    /// `$name.path` (templates only).
    Ref(Path),
    /// `"text ${path} text"` (templates only).
    Interp(Vec<Part>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Ref(Path),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Node(Node),
    Spread(Node),
    If(Box<Cond>, Vec<Item>, Vec<Item>),
    For(ForHead, Vec<Item>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Field(Key, Node),
    Spread(Node),
    If(Box<Cond>, Vec<Entry>, Vec<Entry>),
    For(ForHead, Vec<Entry>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Lit(String),
    Interp(Vec<Part>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForHead {
    pub index: Option<String>,
    pub var: String,
    pub iter: Node,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    Test(Node),
    Eq(Node, Node),
    Ne(Node, Node),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
}

impl Node {
    /// Convert a template-free tree into a value.
    ///
    /// # Errors
    ///
    /// Returns a message if the tree contains a template construct.
    pub fn to_value(&self) -> Result<Value, String> {
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
    pub name: String,
    pub arg: String,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub optional: bool,
    /// Type expression in schema syntax (spec §6), e.g. `u16 @range(1..10)`.
    pub ty: String,
    pub default: Option<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Let {
    pub name: String,
    pub value: Node,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub path: String,
    pub alias: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TemplateHeader {
    pub params: Vec<Param>,
    pub imports: Vec<Import>,
    pub lets: Vec<Let>,
}

/// A parsed text document.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub directives: Vec<Directive>,
    /// Present iff the document starts with `@template`.
    pub template: Option<TemplateHeader>,
    pub body: Node,
}

impl Document {
    #[must_use]
    pub fn directive(&self, name: &str) -> Option<&str> {
        self.directives.iter().find(|d| d.name == name).map(|d| d.arg.as_str())
    }

    #[must_use]
    pub fn is_template(&self) -> bool {
        self.template.is_some()
    }
}
