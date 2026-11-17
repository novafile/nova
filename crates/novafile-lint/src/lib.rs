//! `nvf lint` (spec Appendix E): conventions that make NOVA documents look
//! alike, unlike JSON where every project invents its own.
//!
//! | Rule | Applies to | Level | Fix |
//! | --- | --- | --- | --- |
//! | `key-case` | record keys (documents without `@schema`) | warning | rename (unless it would collide) |
//! | `key-collision` | keys that differ only in case/separators | warning | — |
//! | `quoted-key` | keys that need quotes | hint | — |
//! | `untyped-timestamp` | plain strings that are RFC 3339 timestamps (no `@schema`) | hint | wrap in `@timestamp(...)` |
//! | `mixed-list` | lists mixing scalar kinds | hint | — |
//! | `compat-loss` | values the `@compat` format cannot represent | warning | — |
//! | `field-case` | schema field names | warning | — |
//! | `variant-case` | schema enum variants | warning | — |
//! | `partial-ids` | records where only some fields have `@id` | warning | — |
//!
//! Configuration: [`Config`], or in a document `@x-lint case=camel allow=quoted-key,mixed-list`.
//!
//! A document declaring `@compat json` (or `yaml`, `toml`) mirrors a contract
//! in that format: its key names are dictated externally, so `key-case` does
//! not apply, and every value the format cannot represent is reported.

mod case;

pub use case::{Case, split_words};

use novafile_text::spans::{SpanKind, SpanMap};
use novafile_text::{Path, Segment};
use novafile_value::Value;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Hint,
    Warning,
}

/// A text replacement that fixes a finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub level: Level,
    pub message: String,
    /// Byte range in the source.
    pub start: usize,
    pub end: usize,
    pub fix: Option<Fix>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub case: Case,
    pub allow: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            case: Case::Snake,
            allow: Vec::new(),
        }
    }
}

impl Config {
    fn on(&self, rule: &str) -> bool {
        !self.allow.iter().any(|a| a == rule)
    }

    /// Apply `@x-lint key=value …` settings from a document header.
    ///
    /// # Errors
    ///
    /// Returns a message if a setting is unknown or names an unknown case style.
    pub fn with_directive(mut self, arg: &str) -> Result<Config, String> {
        for part in arg.split_whitespace() {
            match part.split_once('=') {
                Some(("case", c)) => self.case = Case::parse(c).ok_or_else(|| format!("unknown case {c:?} (snake, camel, kebab)"))?,
                Some(("allow", rules)) => self.allow.extend(rules.split(',').map(str::to_owned)),
                _ => return Err(format!("unknown @x-lint setting {part:?} (case=…, allow=…)")),
            }
        }

        Ok(self)
    }
}

fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && (b[0].is_ascii_alphabetic() || b[0] == b'_') && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

/// Lint a NOVA text document.
///
/// # Errors
///
/// Returns a [`ParseError`](novafile_text::ParseError) if `src` is malformed,
/// its `@x-lint` directive is invalid, or it contains template constructs.
pub fn lint_document(src: &str, config: &Config) -> Result<Vec<Finding>, novafile_text::ParseError> {
    let (doc, spans) = novafile_text::parse_with_spans(src)?;
    if doc.is_template() {
        return Ok(Vec::new());
    }

    let mut config = config.clone();
    if let Some(arg) = doc.directive("x-lint") {
        config = config.with_directive(arg).map_err(|m| novafile_text::ParseError {
            line: 1,
            col: 1,
            offset: 0,
            message: m,
        })?;
    }

    let value = doc.body.to_value().map_err(|m| novafile_text::ParseError {
        line: 1,
        col: 1,
        offset: 0,
        message: m,
    })?;
    let has_schema = doc.directive("schema").is_some();
    let compat = doc.directive("compat").map(str::trim).filter(|c| !c.is_empty());
    let mut out = Vec::new();
    if let Some(format) = compat {
        let warnings: Vec<(String, String)> = match format {
            "json" => novafile_json::to_json_string(&value, false)
                .1
                .into_iter()
                .map(|w| (w.path, w.message))
                .collect(),
            "yaml" => novafile_formats::yaml::to_string(&value).1.into_iter().map(|w| (w.path, w.message)).collect(),
            "toml" => match novafile_formats::toml::to_string(&value) {
                Ok((_, w)) => w.into_iter().map(|w| (w.path, w.message)).collect(),
                Err(e) => vec![(String::new(), e)],
            },
            other => {
                return Err(novafile_text::ParseError {
                    line: 1,
                    col: 1,
                    offset: 0,
                    message: format!("unknown @compat format {other:?} (json, yaml, toml)"),
                });
            }
        };

        if config.on("compat-loss") {
            for (p, msg) in warnings {
                let path = Path::parse(p.trim_start_matches('.')).unwrap_or_default();
                let (a, b) = spans.closest(&path).map_or((0, 0), |s| (s.value_start(src).min(s.end), s.end));
                out.push(Finding {
                    rule: "compat-loss",
                    level: Level::Warning,
                    message: format!("not representable in {format}: {msg}"),
                    start: a,
                    end: b,
                    fix: None,
                });
            }
        }

        // Key names belong to the external contract.
        config.allow.push("key-case".into());
    }

    keys(src, &spans, &value, &config, has_schema, &mut out);
    scalars(src, &spans, &value, &config, has_schema, &mut out);
    out.sort_by_key(|f| (f.start, f.rule));
    Ok(out)
}

fn keys(_src: &str, spans: &SpanMap, value: &Value, config: &Config, has_schema: bool, out: &mut Vec<Finding>) {
    // Group record entries by their parent record.
    let mut by_parent: HashMap<String, Vec<(&novafile_text::spans::Span, String)>> = HashMap::new();
    for s in spans.iter() {
        if let (Some(Segment::Field(k)), Some(_)) = (s.path.0.last(), s.key)
            && matches!(s.kind, SpanKind::BlockEntry | SpanKind::FlowEntry)
        {
            let parent = Path(s.path.0[..s.path.0.len() - 1].to_vec()).to_string();
            by_parent.entry(parent).or_default().push((s, k.clone()));
        }
    }

    let _ = value;
    for entries in by_parent.values() {
        let names: Vec<&str> = entries.iter().map(|(_, k)| k.as_str()).collect();
        // key-collision: same words, different spelling.
        if config.on("key-collision") {
            let mut seen: HashMap<Vec<String>, &str> = HashMap::new();
            for (s, k) in entries {
                let w = split_words(k);
                if let Some(other) = seen.get(&w) {
                    let (a, b) = s.key.unwrap();
                    out.push(Finding {
                        rule: "key-collision",
                        level: Level::Warning,
                        message: format!("`{k}` and `{other}` name the same thing in different spellings"),
                        start: a,
                        end: b,
                        fix: None,
                    });
                } else {
                    seen.insert(w, k);
                }
            }
        }

        for (s, k) in entries {
            let (a, b) = s.key.unwrap();
            if config.on("quoted-key") && !is_ident(k) {
                let suggestion = config.case.convert(k);
                out.push(Finding {
                    rule: "quoted-key",
                    level: Level::Hint,
                    message: if is_ident(&suggestion) {
                        format!("key needs quotes; consider `{suggestion}`")
                    } else {
                        "key needs quotes".into()
                    },
                    start: a,
                    end: b,
                    fix: None,
                });
                continue;
            }

            if has_schema || !config.on("key-case") || config.case.matches(k) {
                continue;
            }

            let want = config.case.convert(k);
            let collides = names.contains(&want.as_str());
            out.push(Finding {
                rule: "key-case",
                level: Level::Warning,
                message: format!(
                    "key `{k}` is not {}: `{want}`{}",
                    config.case.name(),
                    if collides { " (not fixed: that key exists)" } else { "" }
                ),
                start: a,
                end: b,
                fix: (!collides && is_ident(&want)).then_some(Fix {
                    start: a,
                    end: b,
                    replacement: want,
                }),
            });
        }
    }
}

fn scalars(src: &str, spans: &SpanMap, value: &Value, config: &Config, has_schema: bool, out: &mut Vec<Finding>) {
    for s in spans.iter() {
        let Ok(v) = s.path.get(value) else { continue };
        let vs = s.value_start(src);
        match v {
            Value::String(text)
                if !has_schema
                    && config.on("untyped-timestamp")
                    && matches!(
                        s.kind,
                        SpanKind::BlockEntry | SpanKind::BlockItem | SpanKind::TableCell | SpanKind::FlowEntry | SpanKind::FlowItem
                    ) =>
            {
                if novafile_value::Timestamp::parse(text).is_ok() {
                    let raw = &src[vs..s.end];
                    out.push(Finding {
                        rule: "untyped-timestamp",
                        level: Level::Hint,
                        message: "this is a string, not a timestamp; write @timestamp(\"…\") or declare the type in a schema".into(),
                        start: vs,
                        end: s.end,
                        fix: (raw == text || raw.trim_matches('"') == text).then(|| Fix {
                            start: vs,
                            end: s.end,
                            replacement: format!("@timestamp(\"{text}\")"),
                        }),
                    });
                }
            }
            Value::List(items) if config.on("mixed-list") && items.len() > 1 => {
                let kinds: Vec<&str> = items
                    .iter()
                    .filter(|i| !i.is_container() && !matches!(i, Value::Null))
                    .map(Value::kind)
                    .collect();
                let mut uniq = kinds.clone();
                uniq.sort_unstable();
                uniq.dedup();
                if uniq.len() > 1 && kinds.len() == items.iter().filter(|i| !matches!(i, Value::Null)).count() {
                    out.push(Finding {
                        rule: "mixed-list",
                        level: Level::Hint,
                        message: format!("list mixes {}; is that intended?", uniq.join(" and ")),
                        start: vs,
                        end: s.end,
                        fix: None,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Lint a schema (`.novs`) source. Findings are located at whole lines.
///
/// # Errors
///
/// Returns a [`SchemaError`](novafile_schema::SchemaError) if `src` is not a valid schema.
pub fn lint_schema(src: &str, config: &Config) -> Result<Vec<Finding>, novafile_schema::SchemaError> {
    let schema = novafile_schema::parse_schema(src)?;
    let mut out = Vec::new();
    let line_of = |needle: &str, after: usize| -> (usize, usize) {
        let i = src[after..].find(needle).map_or(0, |i| after + i);
        (i, i + needle.len())
    };

    for (name, def) in &schema.defs {
        let def_at = line_of(&format!(" {name}"), 0).0;
        match def {
            novafile_schema::Def::Record(r) => {
                if config.on("field-case") {
                    for f in &r.fields {
                        if !config.case.matches(&f.name) {
                            let (a, b) = line_of(&format!("  {}", f.name), def_at);
                            out.push(Finding {
                                rule: "field-case",
                                level: Level::Warning,
                                message: format!("field `{name}.{}` is not {}: `{}`", f.name, config.case.name(), config.case.convert(&f.name)),
                                start: a + 2,
                                end: b,
                                fix: None,
                            });
                        }
                    }
                }

                let with_ids = r.fields.iter().filter(|f| f.id.is_some()).count();
                if config.on("partial-ids") && with_ids > 0 && with_ids < r.fields.len() {
                    let (a, b) = line_of(&format!("record {name}"), 0);
                    out.push(Finding {
                        rule: "partial-ids",
                        level: Level::Warning,
                        message: format!(
                            "record {name}: {with_ids} of {} fields have @id; give every field an id (Protobuf and positional encodings need them)",
                            r.fields.len()
                        ),
                        start: a,
                        end: b,
                        fix: None,
                    });
                }
            }
            novafile_schema::Def::Enum(vs) if config.on("variant-case") => {
                for v in vs {
                    if !config.case.matches(&v.name) {
                        let (a, b) = line_of(&v.name, def_at);
                        out.push(Finding {
                            rule: "variant-case",
                            level: Level::Warning,
                            message: format!("variant `{name}.{}` is not {}: `{}`", v.name, config.case.name(), config.case.convert(&v.name)),
                            start: a,
                            end: b,
                            fix: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    out.sort_by_key(|f| (f.start, f.rule));
    Ok(out)
}

/// Apply all fixes (non-overlapping, back to front).
#[must_use]
pub fn apply_fixes(src: &str, findings: &[Finding]) -> String {
    let mut fixes: Vec<&Fix> = findings.iter().filter_map(|f| f.fix.as_ref()).collect();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.start));
    let mut out = src.to_owned();
    let mut last = usize::MAX;
    for f in fixes {
        if f.end <= last {
            out.replace_range(f.start..f.end, &f.replacement);
            last = f.start;
        }
    }

    out
}
