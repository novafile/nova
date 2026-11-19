//! Schema catalogs and lookup (spec §6.9, proposal 0005): where the `.novs`
//! file for a reference such as `acme.example/billing/invoice@2.1.0` lives.
//!
//! Lookup is offline: built-in schemas, then the sources listed in catalogs,
//! then the fetch cache. Fetching itself needs a network client and lives in
//! the tools; [`Resolver::fetch_url`], [`Resolver::accept_fetched`] and
//! [`Resolver::store`] give them everything else.

use crate::{
    LEGACY_META_SCHEMA_NAME, META_SCHEMA_NAME, NameKind, Schema, SchemaRef, Version, meta_catalog, meta_schema, parse_schema, schema_name_kind,
};
use novafile_value::Value;
use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};

/// File name of a project catalog, looked up from a document's directory upwards.
pub const CATALOG_FILE: &str = "nova-catalog.nova";

/// The most bytes a fetched schema may have.
pub const MAX_FETCH_BYTES: u64 = 4 << 20;

/// How many directory entries the catalog sources may make lookup visit, so a
/// catalog pointing at a huge tree cannot stall every lookup.
const MAX_SOURCE_ENTRIES: usize = 100_000;

/// How deep the catalog source directories are searched.
const MAX_SOURCE_DEPTH: usize = 32;

/// Whether `name` lies in a special-use domain (RFC 2606, RFC 6761, ICANN's
/// `.internal`): such names are never fetched.
#[must_use]
pub fn is_special_use(name: &str) -> bool {
    let host = name.split('/').next().unwrap_or("");
    matches!(host, "example.com" | "example.net" | "example.org")
        || ["example", "test", "invalid", "localhost", "internal"]
            .iter()
            .any(|tld| host == *tld || host.ends_with(&format!(".{tld}")))
}

/// Whether `name` is in the namespace reserved for the NOVA project.
#[must_use]
pub fn is_reserved(name: &str) -> bool {
    name.starts_with("novafile.dev/")
}

/// One `remote` entry: where to fetch names under `prefix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// A whole number of name segments: ends with `/`, or is a complete name.
    pub prefix: String,
    /// An `https:` URL template with `{name}`, `{rest}` and `{version}`.
    pub url: String,
}

/// One `pins` entry: the full revision a version must have, wherever it is found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// `name@version`, without a fingerprint.
    pub schema: SchemaRef,
    /// `sha256:` and 64 lowercase hex digits.
    pub revision: String,
}

/// A parsed `nova-catalog.nova` (meta-schema `novafile.dev/meta/catalog`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    /// The catalog file.
    pub path: PathBuf,
    /// Source files and directories, resolved against the catalog's directory.
    pub sources: Vec<PathBuf>,
    /// Fetch locations by name prefix.
    pub remote: Vec<Remote>,
    /// Expected revisions.
    pub pins: Vec<Pin>,
    /// `fetch: true`; honoured only in the user catalog.
    pub fetch: bool,
}

impl Catalog {
    /// Read and check the catalog at `path`.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or is not a valid catalog.
    pub fn load(path: &Path) -> Result<Catalog, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Catalog::parse(&text, path).map_err(|m| format!("{}: {m}", path.display()))
    }

    /// Parse catalog text that lives at `path`.
    ///
    /// # Errors
    ///
    /// Fails if `text` does not conform to the catalog meta-schema, or breaks a
    /// rule it cannot express: a prefix that is not whole segments, a URL template
    /// with unknown placeholders, or a pin that is not `name@version`.
    pub fn parse(text: &str, path: &Path) -> Result<Catalog, String> {
        let value = novafile_text::parse(text).map_err(|e| e.to_string())?;
        let value = meta_catalog().resolve(&value).map_err(|issues| {
            let m: Vec<String> = issues.iter().map(ToString::to_string).collect();
            format!("not a valid catalog: {}", m.join("; "))
        })?;
        let rec = value.as_record().ok_or("a catalog is a record")?;
        let list = |k: &str| rec.get(k).and_then(Value::as_list).map_or(&[][..], Vec::as_slice);
        let text_of = |v: &Value, k: &str| v.as_record().and_then(|r| r.get(k)).and_then(Value::as_str).unwrap_or("").to_owned();
        let dir = path.parent().unwrap_or(Path::new(""));

        let sources = list("schemas").iter().map(|s| dir.join(text_of(s, "path"))).collect();
        let mut remote = Vec::new();
        for r in list("remote") {
            let (prefix, url) = (text_of(r, "prefix"), text_of(r, "url"));
            let whole = prefix.strip_suffix('/').map_or(prefix.as_str(), |p| p);
            if schema_name_kind(whole).is_none() && schema_name_kind(&format!("{whole}/x")).is_none() {
                return Err(format!("remote prefix {prefix:?} is not a whole number of name segments"));
            }

            check_template(&url)?;
            remote.push(Remote { prefix, url });
        }

        let mut pins = Vec::new();
        for p in list("pins") {
            let schema = SchemaRef::parse(&text_of(p, "schema")).map_err(|m| format!("pin: {m}"))?;
            if schema.fingerprint.is_some() {
                return Err(format!("pin {schema}: write name@version; the revision is the fingerprint"));
            }

            pins.push(Pin {
                schema,
                revision: text_of(p, "revision"),
            });
        }

        Ok(Catalog {
            path: path.to_path_buf(),
            sources,
            remote,
            pins,
            fetch: matches!(rec.get("fetch"), Some(Value::Bool(true))),
        })
    }
}

/// Check a remote URL template: `https:`, and only the three placeholders.
fn check_template(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("remote URL {url:?} must be an https: URL"));
    }

    let mut rest = url;
    while let Some(i) = rest.find('{') {
        let end = rest[i..].find('}').ok_or_else(|| format!("remote URL {url:?} has an unclosed `{{`"))?;
        let name = &rest[i + 1..i + end];
        if !matches!(name, "name" | "rest" | "version") {
            return Err(format!("remote URL {url:?} has an unknown placeholder {{{name}}} ({{name}}, {{rest}}, {{version}})"));
        }

        rest = &rest[i + end + 1..];
    }

    if rest.contains('}') {
        return Err(format!("remote URL {url:?} has an unmatched `}}`"));
    }

    Ok(())
}

/// The nearest `nova-catalog.nova` in `dir` or a directory above it.
#[must_use]
pub fn project_catalog(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().map(|d| d.join(CATALOG_FILE)).find(|p| p.is_file())
}

/// The platform's user configuration directory.
fn config_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("APPDATA")
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    }
}

/// The platform's user cache directory.
fn cache_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("LOCALAPPDATA")
    } else {
        env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache")))
    }
}

/// The user catalog, `nova/catalog.nova` in the configuration directory.
#[must_use]
pub fn user_catalog() -> Option<PathBuf> {
    config_dir().map(|d| d.join("nova").join("catalog.nova")).filter(|p| p.is_file())
}

/// The directory fetched schemas are cached in.
#[must_use]
pub fn user_cache() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("nova").join("schemas"))
}

/// The `name@version` of every schema in the fetch cache `cache`.
#[must_use]
pub fn cached_versions(cache: &Path) -> Vec<SchemaRef> {
    let mut files = Vec::new();
    walk(cache, 0, &mut 0, &mut files);
    files
        .into_iter()
        .filter_map(|f| {
            let name = f.parent()?.strip_prefix(cache).ok()?.to_str()?.replace('\\', "/");
            let version = Version::parse(f.file_stem()?.to_str()?).ok()?;
            schema_name_kind(&name)?;
            Some(SchemaRef { name, version, fingerprint: None })
        })
        .collect()
}

/// Where a resolved schema came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Bundled with the implementation (the meta-schemas).
    Builtin,
    /// A `.novs` file reached through a catalog or the workspace.
    File(PathBuf),
    /// The fetch cache.
    Cache(PathBuf),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Builtin => f.write_str("built-in"),
            Origin::File(p) => write!(f, "{}", p.display()),
            Origin::Cache(p) => write!(f, "cache {}", p.display()),
        }
    }
}

/// Where and how to fetch a schema version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetch {
    /// The `https:` URL.
    pub url: String,
    /// Whether the result may go into the user cache. False when the location
    /// comes from the project catalog, which arrives with a repository: what it
    /// fetches is used for this run only, so it cannot plant content that other
    /// projects would later find in the shared cache.
    pub cacheable: bool,
}

/// A schema found for a reference.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The schema.
    pub schema: Schema,
    /// Where it was found.
    pub origin: Origin,
    /// Deprecations and similar notes, such as a legacy meta-schema name.
    pub warnings: Vec<String>,
}

/// Why a reference did not resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupError {
    /// No place holds a schema that can read the reference.
    NotFound(String),
    /// Two files declare the same `name@version` with different content.
    Conflict(String),
    /// A version does not have the revision a catalog pins.
    Pin(String),
    /// The reference is malformed, or the schema found does not accept it.
    Invalid(String),
}

impl LookupError {
    /// The error kind, as the conformance suite names it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            LookupError::NotFound(_) => "not-found",
            LookupError::Conflict(_) => "conflict",
            LookupError::Pin(_) => "pin",
            LookupError::Invalid(_) => "invalid",
        }
    }
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LookupError::NotFound(m) | LookupError::Conflict(m) | LookupError::Pin(m) | LookupError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for LookupError {}

/// Which catalogs and caches a [`Resolver`] uses.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Catalogs named explicitly (`--catalog`, `NOVA_CATALOG`), searched first.
    pub catalogs: Vec<PathBuf>,
    /// The directory of the document being processed: where the project
    /// catalog search starts.
    pub start: Option<PathBuf>,
    /// Also use the user catalog and the fetch cache. Off for reproducible
    /// runs such as the conformance suite.
    pub user: bool,
}

impl Options {
    /// Options for tools: `catalogs` plus the paths in `NOVA_CATALOG`, the project
    /// catalog above `start`, the user catalog and the cache.
    #[must_use]
    pub fn from_env(catalogs: Vec<PathBuf>, start: Option<PathBuf>) -> Options {
        let mut catalogs = catalogs;
        if let Some(v) = std::env::var_os("NOVA_CATALOG") {
            catalogs.extend(std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()));
        }

        Options { catalogs, start, user: true }
    }
}

/// A named schema file, parsed.
#[derive(Clone, Debug)]
struct Found {
    /// Where it came from.
    origin: Origin,
    /// The schema.
    schema: Schema,
}

/// Finds schemas for references (spec §6.9): built-in schemas, catalog
/// sources and the fetch cache, with conflicts and pins checked.
#[derive(Debug)]
pub struct Resolver {
    /// Catalogs in search order: explicit, project, user.
    catalogs: Vec<Catalog>,
    /// Index of the project catalog in `catalogs`, if any. It arrives with a
    /// cloned repository, so it cannot enable fetching, and what its `remote`
    /// entries fetch is never written to the cache all projects share.
    project: Option<usize>,
    /// Whether the user catalog enables fetching.
    user_fetch: bool,
    /// The fetch cache, when the user's places are in use.
    cache: Option<PathBuf>,
    /// Schemas bundled with the implementation.
    builtin: Vec<Found>,
    /// Named schemas from catalog sources, in source order.
    sources: Vec<Found>,
    /// Files that could not be read or parsed, as notes.
    pub problems: Vec<String>,
}

impl Resolver {
    /// Load the catalogs `opts` selects and index their sources.
    ///
    /// # Errors
    ///
    /// Fails if a catalog exists but is invalid; unreadable or invalid `.novs`
    /// files only become [`Resolver::problems`].
    pub fn new(opts: &Options) -> Result<Resolver, String> {
        let mut paths = opts.catalogs.clone();
        let mut project = None;
        if let Some(p) = opts.start.as_deref().and_then(project_catalog)
            && !paths.contains(&p)
        {
            project = Some(paths.len());
            paths.push(p);
        }

        let user = if opts.user { user_catalog() } else { None };
        let mut catalogs = paths.iter().map(|p| Catalog::load(p)).collect::<Result<Vec<_>, _>>()?;
        let user_fetch = match &user {
            Some(p) => {
                let c = Catalog::load(p)?;
                let f = c.fetch;
                catalogs.push(c);
                f
            }
            None => false,
        };

        let builtin = [meta_schema(), meta_catalog()]
            .into_iter()
            .map(|schema| Found {
                origin: Origin::Builtin,
                schema,
            })
            .collect();
        let mut r = Resolver {
            catalogs,
            project,
            user_fetch,
            cache: if opts.user { user_cache() } else { None },
            builtin,
            sources: Vec::new(),
            problems: Vec::new(),
        };
        r.index_sources();
        Ok(r)
    }

    /// Read every `.novs` file the catalogs' sources reach.
    fn index_sources(&mut self) {
        let (mut files, mut visited) = (Vec::new(), 0);
        for c in &self.catalogs {
            for s in &c.sources {
                if s.is_dir() {
                    walk(s, 0, &mut visited, &mut files);
                } else {
                    files.push(s.clone());
                }
            }
        }

        if visited >= MAX_SOURCE_ENTRIES {
            self.problems
                .push(format!("catalog sources hold more than {MAX_SOURCE_ENTRIES} entries; the rest were not searched"));
        }

        let mut seen = std::collections::HashSet::new();
        for f in files {
            if !seen.insert(f.clone()) {
                continue;
            }

            match read_bounded(&f).and_then(|t| parse_schema(&t).map_err(|e| e.to_string())) {
                // Anonymous schemas have no name to be found by.
                Ok(schema) if schema.reference().is_some() => self.sources.push(Found {
                    origin: Origin::File(f),
                    schema,
                }),
                Ok(_) => {}
                Err(m) => self.problems.push(format!("{}: {m}", f.display())),
            }
        }
    }

    /// Whether the user enabled fetching in the user catalog.
    #[must_use]
    pub fn fetch_enabled(&self) -> bool {
        self.user_fetch
    }

    /// The pin for `name@version`, if a catalog has one.
    fn pin(&self, name: &str, version: &Version) -> Option<&Pin> {
        self.catalogs
            .iter()
            .flat_map(|c| &c.pins)
            .find(|p| p.schema.name == name && p.schema.version.precedence(version) == Ordering::Equal)
    }

    /// Fetched versions of `name` in the cache.
    fn cached(&self, name: &str) -> Vec<Found> {
        let Some(dir) = self.cache.as_ref().map(|c| c.join(name)) else { return Vec::new() };
        let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
        rd.flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "novs"))
            .filter_map(|p| {
                let schema = parse_schema(&read_bounded(&p).ok()?).ok()?;
                (schema.name.as_deref() == Some(name)).then_some(Found {
                    origin: Origin::Cache(p),
                    schema,
                })
            })
            .collect()
    }

    /// Resolve `reference` (`name@version[#fingerprint]`).
    ///
    /// # Errors
    ///
    /// [`LookupError::NotFound`] if no place has a schema that can read the
    /// version, [`LookupError::Conflict`] if two files disagree about one
    /// version, [`LookupError::Pin`] if the chosen version breaks a pin, and
    /// [`LookupError::Invalid`] for a malformed reference or one the chosen
    /// schema does not accept (a fingerprint mismatch at the same version).
    pub fn resolve(&self, reference: &str) -> Result<Resolved, LookupError> {
        let mut r = SchemaRef::parse(reference).map_err(LookupError::Invalid)?;
        let mut warnings = Vec::new();
        if r.name == LEGACY_META_SCHEMA_NAME {
            warnings.push(format!("`{LEGACY_META_SCHEMA_NAME}` is now `{META_SCHEMA_NAME}`; update the @schema line (spec §6.8)"));
            META_SCHEMA_NAME.clone_into(&mut r.name);
        }

        self.check_conflicts(&r.name)?;
        let places: [Vec<Found>; 3] = [
            self.builtin.iter().filter(|f| f.schema.name.as_deref() == Some(r.name.as_str())).cloned().collect(),
            self.sources.iter().filter(|f| f.schema.name.as_deref() == Some(r.name.as_str())).cloned().collect(),
            self.cached(&r.name),
        ];

        for place in places {
            if let Some(found) = choose(place, &r) {
                self.check_pin(&found)?;
                found
                    .schema
                    .accepts_reference(&r.to_string())
                    .map_err(|m| LookupError::Invalid(format!("{}: {m}", found.origin)))?;
                return Ok(Resolved {
                    schema: found.schema,
                    origin: found.origin,
                    warnings,
                });
            }
        }

        Err(LookupError::NotFound(format!("no schema found that can read {r}")))
    }

    /// Two files (or a file and a built-in schema) declaring the same version of
    /// `name` must have the same canonical text.
    fn check_conflicts(&self, name: &str) -> Result<(), LookupError> {
        let all: Vec<&Found> = self.builtin.iter().chain(&self.sources).filter(|f| f.schema.name.as_deref() == Some(name)).collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                let (va, vb) = (a.schema.version.as_ref().unwrap(), b.schema.version.as_ref().unwrap());
                if va.precedence(vb) == Ordering::Equal && a.schema.revision() != b.schema.revision() {
                    return Err(LookupError::Conflict(format!(
                        "{name}@{va} is declared with different content by {} and {}",
                        a.origin, b.origin
                    )));
                }
            }
        }

        Ok(())
    }

    /// A pinned version must have the pinned revision.
    fn check_pin(&self, found: &Found) -> Result<(), LookupError> {
        let s = &found.schema;
        let (name, version) = (s.name.as_deref().unwrap_or(""), s.version.as_ref().expect("named schemas have a version"));
        match self.pin(name, version) {
            Some(p) if p.revision != s.revision() => Err(LookupError::Pin(format!(
                "{name}@{version} from {} has revision {}, but a catalog pins {}",
                found.origin,
                s.revision(),
                p.revision
            ))),
            _ => Ok(()),
        }
    }

    /// Where to fetch exactly `r`'s version. A catalog's `remote` entry with the
    /// longest matching prefix wins over the default location
    /// `https://{name}/{version}.novs`.
    ///
    /// # Errors
    ///
    /// Local names, special-use domains and bundled schemas are never fetched.
    pub fn fetch_url(&self, r: &SchemaRef) -> Result<Fetch, String> {
        if schema_name_kind(&r.name) != Some(NameKind::Qualified) || is_special_use(&r.name) || r.name.starts_with("novafile.dev/meta/") {
            return Err(format!("{r} is never fetched: only qualified names outside special-use domains are"));
        }

        let remote = self
            .catalogs
            .iter()
            .enumerate()
            .flat_map(|(i, c)| c.remote.iter().map(move |m| (i, m)))
            .filter(|(_, m)| if m.prefix.ends_with('/') { r.name.starts_with(&m.prefix) } else { r.name == m.prefix })
            .max_by_key(|(_, m)| m.prefix.len());
        // Anything the repository's own catalog redirects stays out of the shared
        // cache: its pins and the document's fingerprint come from the same source.
        let cacheable = remote.is_none_or(|(i, _)| Some(i) != self.project);
        let remote = remote.map(|(_, m)| m);
        let url = match remote {
            Some(m) => m
                .url
                .replace("{name}", &r.name)
                .replace("{rest}", r.name.strip_prefix(&m.prefix).unwrap_or(""))
                .replace("{version}", &r.version.to_string()),
            None => format!("https://{}/{}.novs", r.name, r.version),
        };

        Ok(Fetch { url, cacheable })
    }

    /// Check a fetched schema text for `r`: it must declare exactly `r`'s name and
    /// version, and match `r`'s fingerprint and any pin.
    ///
    /// # Errors
    ///
    /// Fails, and the text must be discarded, if any check fails.
    pub fn accept_fetched(&self, r: &SchemaRef, text: &str) -> Result<Schema, LookupError> {
        let schema = parse_schema(text).map_err(|e| LookupError::Invalid(format!("fetched {r}: not a valid schema: {e}")))?;
        let declared = schema.reference().ok_or_else(|| LookupError::Invalid(format!("fetched {r}: the file is an anonymous schema")))?;
        if declared.name != r.name || declared.version.precedence(&r.version) != Ordering::Equal || declared.version.build != r.version.build {
            return Err(LookupError::Invalid(format!("fetched {r}: the file declares {declared}")));
        }

        if let Some(fp) = &r.fingerprint
            && *fp != schema.fingerprint_hex()
        {
            return Err(LookupError::Invalid(format!("fetched {r}: fingerprint is #{}", schema.fingerprint_hex())));
        }

        self.check_pin(&Found {
            origin: Origin::Builtin,
            schema: schema.clone(),
        })?;
        Ok(schema)
    }

    /// Store a fetched schema in the cache as `{cache}/{name}/{version}.novs`.
    ///
    /// # Errors
    ///
    /// Fails if there is no cache directory, it cannot be written, or a different
    /// revision of the same version is already cached (versions are immutable).
    pub fn store(&self, schema: &Schema, text: &str) -> Result<PathBuf, LookupError> {
        let r = schema.reference().ok_or_else(|| LookupError::Invalid("anonymous schemas are not cached".into()))?;
        let dir = self.cache.as_ref().ok_or_else(|| LookupError::Invalid("no user cache directory".into()))?.join(&r.name);
        let path = dir.join(format!("{}.novs", r.version));
        if let Ok(old) = std::fs::read_to_string(&path).map_err(|_| ()).and_then(|t| parse_schema(&t).map_err(|_| ()))
            && old.revision() != schema.revision()
        {
            return Err(LookupError::Conflict(format!(
                "{r} is cached at {} with revision {}, but the fetched file has {}",
                path.display(),
                old.revision(),
                schema.revision()
            )));
        }

        std::fs::create_dir_all(&dir).map_err(|e| LookupError::Invalid(format!("{}: {e}", dir.display())))?;
        std::fs::write(&path, text).map_err(|e| LookupError::Invalid(format!("{}: {e}", path.display())))?;
        Ok(path)
    }
}

/// The candidate in one place for `r`: the version `r` names with its
/// fingerprint, if present, else the newest that can read `r`'s version.
fn choose(place: Vec<Found>, r: &SchemaRef) -> Option<Found> {
    let readable = place.into_iter().filter(|f| f.schema.version.as_ref().is_some_and(|v| v.can_read(&r.version)));
    let mut readable: Vec<Found> = readable.collect();
    if let Some(fp) = &r.fingerprint
        && let Some(i) = readable.iter().position(|f| {
            f.schema.version.as_ref().is_some_and(|v| v.precedence(&r.version) == Ordering::Equal) && f.schema.fingerprint_hex() == *fp
        })
    {
        return Some(readable.swap_remove(i));
    }

    readable
        .into_iter()
        .max_by(|a, b| a.schema.version.as_ref().unwrap().precedence(b.schema.version.as_ref().unwrap()))
}

/// Collect `.novs` files below `dir`, without following symlinks, visiting at
/// most [`MAX_SOURCE_ENTRIES`] entries and [`MAX_SOURCE_DEPTH`] levels.
fn walk(dir: &Path, depth: usize, visited: &mut usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SOURCE_DEPTH {
        return;
    }

    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries = Vec::new();
    for e in rd.flatten() {
        *visited += 1;
        if *visited >= MAX_SOURCE_ENTRIES {
            break;
        }

        entries.push(e);
    }

    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk(&p, depth + 1, visited, out);
        } else if ft.is_file() && p.extension().is_some_and(|x| x == "novs") {
            out.push(p);
        }
    }
}

/// Read a schema source of at most [`MAX_FETCH_BYTES`], so a catalog cannot
/// point lookup at a huge or endless file (`/dev/zero`).
fn read_bounded(p: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }

    let mut text = String::new();
    f.take(MAX_FETCH_BYTES + 1).read_to_string(&mut text).map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_FETCH_BYTES {
        return Err(format!("larger than {MAX_FETCH_BYTES} bytes"));
    }

    Ok(text)
}
