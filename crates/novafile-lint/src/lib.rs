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
//! | `field-case` | schema field names, except quoted ones | warning | — |
//! | `variant-case` | schema enum variants | warning | — |
//! | `union-order` | union members an earlier member makes unreachable or captures values from (spec §6.5) | warning | — |
//! | `pattern-anchor` | `@pattern`s starting with `^` or ending with `$`, which I-Regexp matches literally | warning | — |
//! | `schema-name-version` | schema names whose last segment looks like a version (`…/v2`); versions go after `@` | warning | — |
//! | `schema-name-reserved` | schemas named under `novafile.dev/` that the specification does not define | warning | — |
//! | `unpinned-fetch` | a catalog with `remote` entries whose fetched versions (in the user cache) it does not pin | hint | — |
//! | `partial-ids` | records where only some fields have `@id` (an enum where only some variants have `@id` is a schema error) | warning | — |
//! | `shadowed-builtin` | template `@def`s with the name of a built-in function | warning | — |
//! | `bare-argument` | bare-word arguments in an interpolation that name a value in scope: `"${lower(name)}"` | warning | — |
//! | `dynamic-pattern` | `matches` patterns that are not literals and cannot be checked at load time | hint | — |
//! | `unused-def` | template `@def`s that are never called | hint | — |
//!
//! Configuration: [`Config`], or in a document or schema `@x-lint case=camel allow=quoted-key,mixed-list`.
//!
//! A document declaring `@compat json` (or `yaml`, `toml`) mirrors a contract
//! in that format: its key names are dictated externally, so `key-case` does
//! not apply, and every value the format cannot represent is reported.

mod case;
mod template;

pub use case::{Case, split_words};

use novafile_text::spans::{SpanKind, SpanMap};
use novafile_text::{Path, Segment};
use novafile_value::Value;
use std::collections::{HashMap, HashSet};

/// Severity of a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// A suggestion; does not affect the exit status of `nvf lint`.
    Hint,
    /// A convention violation; `nvf lint` exits 1 if any remains.
    Warning,
}

/// A text replacement that fixes a finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// Start byte offset of the replaced text.
    pub start: usize,
    /// End byte offset (exclusive) of the replaced text.
    pub end: usize,
    /// Text to put in its place.
    pub replacement: String,
}

/// One rule violation in a linted source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Rule name, e.g. `key-case`.
    pub rule: &'static str,
    /// Whether the finding is a warning or only a hint.
    pub level: Level,
    /// Human-readable explanation.
    pub message: String,
    /// Byte range in the source.
    pub start: usize,
    /// End of the byte range (exclusive).
    pub end: usize,
    /// A fix, if the finding can be repaired automatically.
    pub fix: Option<Fix>,
}

/// Lint settings: naming convention and disabled rules.
#[derive(Clone, Debug)]
pub struct Config {
    /// Expected naming convention (default snake case).
    pub case: Case,
    /// Names of disabled rules.
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
    /// Whether `rule` is enabled.
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

/// Whether `s` is a bare key that needs no quotes.
fn is_ident(s: &str) -> bool {
    novafile_text::key_text(s) == s
}

/// Lint a NOVA text document.
///
/// # Errors
///
/// Returns a [`ParseError`](novafile_text::ParseError) if `src` is malformed,
/// or its `@x-lint` directive is invalid. A `@template` document gets only the
/// template rules (`shadowed-builtin`, `bare-argument`, `dynamic-pattern`,
/// `unused-def`).
pub fn lint_document(src: &str, config: &Config) -> Result<Vec<Finding>, novafile_text::ParseError> {
    let (doc, spans) = novafile_text::parse_with_spans(src)?;
    let mut config = config.clone();
    if let Some(arg) = doc.directive("x-lint") {
        config = config.with_directive(arg).map_err(|m| novafile_text::ParseError {
            line: 1,
            col: 1,
            offset: 0,
            message: m,
            fix: None,
        })?;
    }

    if doc.is_template() {
        return Ok(template::lint(src, &doc, &config));
    }

    let value = doc.body.to_value().map_err(|m| novafile_text::ParseError {
        line: 1,
        col: 1,
        offset: 0,
        message: m,
        fix: None,
    })?;

    let has_schema = doc.directive("schema").is_some();
    let is_catalog = doc
        .directive("schema")
        .is_some_and(|r| r.starts_with(&format!("{}@", novafile_schema::META_CATALOG_NAME)));
    let mut catalog_hints = if is_catalog && config.on("unpinned-fetch") {
        unpinned_fetches(src)
    } else {
        Vec::new()
    };
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
                    fix: None,
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

    keys(&spans, &config, has_schema, &mut out);
    scalars(src, &spans, &value, &config, has_schema, &mut out);
    out.append(&mut catalog_hints);
    out.sort_by_key(|f| (f.start, f.rule));
    Ok(out)
}

/// Key rules: `key-collision` and `quoted-key` within each record, and `key-case`
/// (with a fix when the new name is free) unless the document has a schema.
fn keys(spans: &SpanMap, config: &Config, has_schema: bool, out: &mut Vec<Finding>) {
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

    for entries in by_parent.values() {
        let names: HashSet<&str> = entries.iter().map(|(_, k)| k.as_str()).collect();
        // How many keys `key-case` would rename to each name; two renames to one name collide.
        let mut renamed: HashMap<String, usize> = HashMap::new();
        for (_, k) in entries {
            if (!config.on("quoted-key") || is_ident(k)) && !config.case.matches(k) {
                *renamed.entry(config.case.convert(k)).or_default() += 1;
            }
        }

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
            let note = if names.contains(want.as_str()) {
                " (not fixed: that key exists)"
            } else if renamed[&want] > 1 {
                " (not fixed: another key would get that name too)"
            } else {
                ""
            };
            out.push(Finding {
                rule: "key-case",
                level: Level::Warning,
                message: format!("key `{k}` is not {}: `{want}`{note}", config.case.name()),
                start: a,
                end: b,
                fix: (note.is_empty() && is_ident(&want)).then_some(Fix {
                    start: a,
                    end: b,
                    replacement: want,
                }),
            });
        }
    }
}

/// Value rules: `untyped-timestamp` (no schema) and `mixed-list`.
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

/// `unpinned-fetch` hints for the catalog source `src`: versions in the user cache
/// under one of its `remote` prefixes that it does not pin (spec §6.9: trust on
/// first use, then pinned).
fn unpinned_fetches(src: &str) -> Vec<Finding> {
    use novafile_schema::catalog::{CATALOG_FILE, Catalog, cached_versions, user_cache};
    let (Ok(c), Some(cache)) = (Catalog::parse(src, std::path::Path::new(CATALOG_FILE)), user_cache()) else {
        return Vec::new();
    };

    let at = src.find("remote:").map_or((0, 0), |i| (i, i + "remote".len()));
    cached_versions(&cache)
        .into_iter()
        .filter(|v| {
            c.remote.iter().any(|m| {
                if m.prefix.ends_with('/') {
                    v.name.starts_with(&m.prefix)
                } else {
                    v.name == m.prefix
                }
            })
        })
        .filter(|v| !c.pins.iter().any(|p| p.schema.name == v.name && p.schema.version == v.version))
        .map(|v| Finding {
            rule: "unpinned-fetch",
            level: Level::Hint,
            message: format!("{v} was fetched but is not pinned here; run `nvf schema fetch {v} --pin`"),
            start: at.0,
            end: at.1,
            fix: None,
        })
        .collect()
}

/// Lint a schema (`.novs`) source. Findings are located at whole lines.
/// An `@x-lint` directive in the schema adjusts `config` for it.
///
/// # Errors
///
/// Returns a [`SchemaError`](novafile_schema::SchemaError) if `src` is not a
/// valid schema or its `@x-lint` directive is invalid.
#[expect(clippy::too_many_lines, reason = "one pass per schema rule, in rule order")]
pub fn lint_schema(src: &str, config: &Config) -> Result<Vec<Finding>, novafile_schema::SchemaError> {
    let schema = novafile_schema::parse_schema(src)?;
    let mut config = config.clone();
    for e in &schema.extensions {
        if let Some(arg) = e.strip_prefix("x-lint")
            && (arg.is_empty() || arg.starts_with(' '))
        {
            config = config.with_directive(arg).map_err(|message| novafile_schema::SchemaError {
                line: src.lines().position(|l| l.starts_with("@x-lint")).map_or(0, |i| i + 1),
                message,
            })?;
        }
    }

    let config = &config;
    let mut out = Vec::new();
    if let Some(name) = &schema.name {
        let at = src.find("@schema").map_or((0, 0), |i| (i, i + src[i..].find('\n').unwrap_or(src.len() - i)));
        let last = name.rsplit('/').next().unwrap_or(name);
        let versionish = last.strip_prefix('v').unwrap_or(last);
        if config.on("schema-name-version") && !versionish.is_empty() && versionish.split('.').all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit())) {
            out.push(Finding {
                rule: "schema-name-version",
                level: Level::Warning,
                message: format!("schema name {name:?} ends in a version; versions belong after `@` (spec §6.7)"),
                start: at.0,
                end: at.1,
                fix: None,
            });
        }

        if config.on("schema-name-reserved")
            && novafile_schema::catalog::is_reserved(name)
            && ![novafile_schema::META_SCHEMA_NAME, novafile_schema::META_CATALOG_NAME].contains(&name.as_str())
        {
            out.push(Finding {
                rule: "schema-name-reserved",
                level: Level::Warning,
                message: format!("{name:?} is in the novafile.dev/ namespace, which is reserved for schemas the specification defines"),
                start: at.0,
                end: at.1,
                fix: None,
            });
        }
    }

    let line_of = |needle: &str, after: usize| -> (usize, usize) {
        let i = src[after..].find(needle).map_or(0, |i| after + i);
        (i, i + needle.len())
    };

    for (name, def) in &schema.defs {
        let def_at = line_of(&format!(" {name}"), 0).0;
        match def {
            novafile_schema::Def::Record(r) => {
                if config.on("field-case") {
                    // Quoted names exist for external contracts and are exempt (spec Appendix E).
                    for f in r.fields.iter().filter(|f| novafile_text::key_text(&f.name) == f.name) {
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

    if config.on("union-order") {
        for w in schema.union_warnings() {
            let find = |needle: &str, after: usize| src[after..].find(needle).map(|i| (after + i, after + i + needle.len()));
            let at = match &w.def {
                Some(d) => ["record ", "type ", "enum "]
                    .iter()
                    .flat_map(|k| [":", " "].map(|end| format!("{k}{d}{end}")))
                    .find_map(|n| find(&n, 0).map(|(a, b)| (a, b - 1)))
                    .and_then(|def| match &w.member {
                        // The name may be written bare or quoted.
                        Some(m) => find(&format!("  {}", novafile_text::key_text(m)), def.0).or_else(|| find(&format!("  {}", novafile_text::quote(m)), def.0)),
                        None => Some(def),
                    }),
                None => find("@root", 0),
            };
            let (a, b) = at.unwrap_or((0, 0));

            out.push(Finding {
                rule: "union-order",
                level: Level::Warning,
                message: w.to_string(),
                start: a,
                end: b,
                fix: None,
            });
        }
    }

    if config.on("pattern-anchor") {
        let mut patterns = Vec::new();
        let types = schema.root.iter().chain(schema.defs.iter().flat_map(|(_, d)| match d {
            novafile_schema::Def::Alias(t) => vec![t],
            novafile_schema::Def::Record(r) => r.fields.iter().map(|f| &f.ty).collect(),
            novafile_schema::Def::Enum(vs) => vs.iter().filter_map(|v| v.payload.as_ref()).collect(),
        }));
        for t in types {
            collect_patterns(t, &mut patterns);
        }

        for p in patterns {
            let p = p.as_str();
            if p.starts_with('^') || p.ends_with('$') {
                let q = novafile_text::quote(p);
                let (a, b) = src.find(&q).map_or((0, 0), |i| (i, i + q.len()));
                out.push(Finding {
                    rule: "pattern-anchor",
                    level: Level::Warning,
                    message: format!("@pattern({q}): `^` and `$` are literal characters in I-Regexp, and the match is always whole-string; drop them"),
                    start: a,
                    end: b,
                    fix: None,
                });
            }
        }
    }

    out.sort_by_key(|f| (f.start, f.rule));
    Ok(out)
}

/// Every `@pattern` written in `t`, outermost first.
fn collect_patterns<'a>(t: &'a novafile_schema::TypeRef, out: &mut Vec<&'a novafile_schema::Pattern>) {
    use novafile_schema::TypeKind;
    out.extend(&t.constraints.pattern);
    match &t.kind {
        TypeKind::List(e) | TypeKind::Set(e) => collect_patterns(e, out),
        TypeKind::Map(k, v) => {
            collect_patterns(k, out);
            collect_patterns(v, out);
        }
        TypeKind::Tuple(ts) | TypeKind::Union(ts) => ts.iter().for_each(|e| collect_patterns(e, out)),
        _ => {}
    }
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
