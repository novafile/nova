//! Lints for `@template` documents (proposal 0006 §0006.8): `shadowed-builtin`,
//! `bare-argument`, `dynamic-pattern` and `unused-def`.

use crate::{Config, Finding, Level};
use novafile_template::BUILTINS;
use novafile_text::Document;
use novafile_text::ast::{Call, Decl, Node};
use novafile_value::Value;
use std::collections::HashSet;

/// Lint the header and body of a template document.
pub(crate) fn lint(src: &str, doc: &Document, config: &Config) -> Vec<Finding> {
    let Some(h) = &doc.template else { return Vec::new() };
    let mut roots: Vec<&Node> = h.params.iter().filter_map(|p| p.default.as_ref()).collect();
    // Names that may be in scope somewhere: parameters, imports, lets, def parameters and
    // comprehension variables. Loop variables of `@for` are not collected, so `bare-argument`
    // can miss a bare word that names one.
    let mut names: HashSet<&str> = h
        .params
        .iter()
        .map(|p| p.name.as_str())
        .chain(h.imports.iter().map(|i| i.alias.as_str()))
        .collect();
    for d in &h.decls {
        match d {
            Decl::Let(l) => {
                names.insert(&l.name);
                roots.push(&l.value);
            }
            Decl::Def(d) => {
                for p in &d.params {
                    names.insert(&p.name);
                    roots.extend(&p.default);
                }

                roots.push(&d.body);
            }
        }
    }

    roots.push(&doc.body);
    let mut calls: Vec<&Call> = Vec::new();
    for r in roots {
        r.walk(&mut |n| match n {
            Node::Call(c) => calls.push(c),
            Node::Comp(c) => {
                names.insert(&c.head.var);
                names.extend(c.head.index.as_deref());
            }
            _ => {}
        });
    }

    let mut out = Vec::new();
    let mut push = |rule: &'static str, level, message: String, start: usize, len: usize| {
        if config.on(rule) {
            out.push(Finding {
                rule,
                level,
                message,
                start,
                end: start + len,
                fix: None,
            });
        }
    };

    for d in h.decls.iter().filter_map(|d| if let Decl::Def(d) = d { Some(d) } else { None }) {
        let len = "@def ".len() + d.name.len();
        if BUILTINS.contains(&d.name.as_str()) {
            push(
                "shadowed-builtin",
                Level::Warning,
                format!("@def {} has the name of a built-in function, which calls below it no longer reach", d.name),
                d.offset,
                len,
            );
        }

        if !calls.iter().any(|c| c.name == d.name) {
            push("unused-def", Level::Hint, format!("@def {} is never called", d.name), d.offset, len);
        }
    }

    for c in &calls {
        // The `$` is implied at the head of an interpolation.
        let len = c.name.len() + usize::from(src.as_bytes().get(c.offset) == Some(&b'$'));
        if c.interp {
            for (a, bare) in c.args.iter().zip(&c.bare) {
                if let (true, Node::Lit(Value::String(w))) = (bare, a)
                    && names.contains(w.as_str())
                {
                    push(
                        "bare-argument",
                        Level::Warning,
                        format!("`{w}` is passed to {} as the string \"{w}\"; write `${w}` for the value of {w}", c.name),
                        c.offset,
                        len,
                    );
                }
            }
        }

        if c.name == "matches" && c.args.get(1).is_some_and(|p| !matches!(p, Node::Lit(Value::String(_)))) {
            push(
                "dynamic-pattern",
                Level::Hint,
                "the pattern of `matches` is not a literal, so it cannot be checked when the template is loaded".into(),
                c.offset,
                len,
            );
        }
    }

    out.sort_by_key(|f| (f.start, f.rule));
    out
}
