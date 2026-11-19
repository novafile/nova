//! `@pattern` (spec §6.1): I-Regexp (RFC 9485) validation and matching.
//!
//! A pattern is checked against the RFC 9485 grammar, then translated to the
//! syntax of `regex-automata` (RFC 9485 §5.4) and compiled there. Its
//! `PikeVM` matches in time linear in the input, as spec §12 requires.

use regex_automata::meta::{Config, Regex};
use std::cell::Cell;
use std::fmt::{self, Write as _};

/// Longest pattern text accepted, in characters.
const MAX_LEN: usize = 4096;

/// How deeply groups may nest, so hostile schemas cannot overflow the stack.
const MAX_DEPTH: usize = 64;

/// Upper bound on one compiled pattern, in bytes. Large enough for
/// `\p{L}{1,63}`; a short pattern can still reach it, hence [`SCHEMA_BUDGET`].
const MAX_COMPILED: usize = 4 << 20;

/// Upper bound on the compiled patterns of one schema together, in bytes, so
/// many short patterns (`a{30000}` compiles to ~0.7 MB) cannot exhaust memory.
pub const SCHEMA_BUDGET: usize = 32 << 20;

/// The general categories I-Regexp allows in `\p{…}` (RFC 9485 §3).
const CATEGORIES: &[&str] = &[
    "L", "Ll", "Lm", "Lo", "Lt", "Lu", "M", "Mc", "Me", "Mn", "N", "Nd", "Nl", "No", "P", "Pc", "Pd", "Pe", "Pf", "Pi", "Po", "Ps", "Z", "Zl", "Zp", "Zs", "S",
    "Sc", "Sk", "Sm", "So", "C", "Cc", "Cf", "Cn", "Co",
];

/// A compiled `@pattern`: the I-Regexp text and its whole-string matcher.
/// Two patterns are equal if their texts are (spec §6.5 classifies changes
/// by text).
#[derive(Clone)]
pub struct Pattern {
    /// The I-Regexp as written, after NOVA string escapes are decoded.
    text: String,
    /// The translated, anchored matcher.
    re: Regex,
}

impl Pattern {
    /// Validate `text` as an I-Regexp and compile it.
    ///
    /// # Errors
    ///
    /// Fails with a reason if `text` is not an I-Regexp, or is too long or
    /// too complex to compile within the bounds.
    pub fn new(text: &str) -> Result<Pattern, String> {
        Pattern::new_within(text, &Cell::new(SCHEMA_BUDGET))
    }

    /// Like [`Pattern::new`], but the compiled size also counts against
    /// `budget`, which is shared by every pattern of one schema and starts
    /// at [`SCHEMA_BUDGET`].
    ///
    /// # Errors
    ///
    /// As [`Pattern::new`], and if `budget` does not cover the compiled pattern.
    pub fn new_within(text: &str, budget: &Cell<usize>) -> Result<Pattern, String> {
        let fail = |m: String| format!("not a valid I-Regexp: {m}");
        if text.chars().count() > MAX_LEN {
            return Err(format!("pattern longer than {MAX_LEN} characters"));
        }

        let mut p = Translator {
            c: text.chars().collect(),
            i: 0,
            out: String::new(),
            depth: 0,
        };

        p.regexp().map_err(fail)?;
        if p.i < p.c.len() {
            return Err(fail("unmatched `)`".into()));
        }

        let limit = MAX_COMPILED.min(budget.get());
        let re = Regex::builder()
            .configure(Config::new().nfa_size_limit(Some(limit)))
            .build(&format!(r"\A(?:{})\z", p.out))
            .map_err(|e| match e.size_limit() {
                Some(_) if limit < MAX_COMPILED => format!("the patterns of this schema need more than {} MiB compiled", SCHEMA_BUDGET >> 20),
                Some(_) => "pattern too complex to compile; bound the length with @len rather than a {n,m} repetition".to_owned(),
                None => fail(e.to_string()),
            })?;

        let used = re.memory_usage();
        if used > budget.get() {
            return Err(format!("the patterns of this schema need more than {} MiB compiled", SCHEMA_BUDGET >> 20));
        }

        budget.set(budget.get() - used);
        Ok(Pattern { text: text.to_owned(), re })
    }

    /// The pattern text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The pattern as an anchored ECMAScript (or PCRE) regexp, per RFC 9485
    /// §5.3: `.` outside classes becomes `[^\n\r]`, and the result is
    /// wrapped in `^(?:` and `)$`. `^` and `$` outside classes, which
    /// I-Regexp matches literally, are escaped. To be used with the `u` flag.
    #[must_use]
    pub fn to_ecmascript(&self) -> String {
        let mut out = String::from("^(?:");
        let (mut class, mut chars) = (false, self.text.chars());
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    // ECMAScript's `u` flag allows `\-` only inside a class.
                    Some('-') if !class => out.push('-'),
                    n => {
                        out.push(c);
                        out.extend(n);
                    }
                },
                '[' if !class => {
                    class = true;
                    out.push(c);
                }
                ']' if class => {
                    class = false;
                    out.push(c);
                }
                '.' if !class => out.push_str(r"[^\n\r]"),
                // Literal characters in I-Regexp, anchors in ECMAScript.
                '^' | '$' if !class => {
                    out.push('\\');
                    out.push(c);
                }
                c => out.push(c),
            }
        }

        out.push_str(")$");
        out
    }

    /// Whether the whole of `s` matches.
    #[must_use]
    pub fn is_match(&self, s: &str) -> bool {
        self.re.is_match(s)
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Pattern").field(&self.text).finish()
    }
}

/// Recursive-descent parser over the RFC 9485 grammar that writes the
/// equivalent `regex-automata` syntax as it goes.
struct Translator {
    /// The pattern's characters.
    c: Vec<char>,
    /// Index of the next character.
    i: usize,
    /// The translation so far.
    out: String,
    /// Current group nesting.
    depth: usize,
}

impl Translator {
    /// The next character, if any.
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    /// Consume and return the next character.
    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.i += usize::from(c.is_some());
        c
    }

    /// Append `c` as a literal character.
    fn lit(&mut self, c: char) {
        match c {
            '\n' => self.out.push_str(r"\n"),
            '\r' => self.out.push_str(r"\r"),
            '\t' => self.out.push_str(r"\t"),
            c => {
                // The meta characters of regex-syntax, set operators included.
                if r"\.+*?()|[]{}^$#&-~".contains(c) {
                    self.out.push('\\');
                }

                self.out.push(c);
            }
        }
    }

    /// `i-regexp = branch *( "|" branch )`
    fn regexp(&mut self) -> Result<(), String> {
        self.branch()?;
        while self.peek() == Some('|') {
            self.i += 1;
            self.out.push('|');
            self.branch()?;
        }

        Ok(())
    }

    /// `branch = *piece`, `piece = atom [ quantifier ]`
    fn branch(&mut self) -> Result<(), String> {
        while !matches!(self.peek(), None | Some('|' | ')')) {
            self.atom()?;
            self.quantifier()?;
        }

        Ok(())
    }

    /// `atom = NormalChar / charClass / ( "(" i-regexp ")" )`
    fn atom(&mut self) -> Result<(), String> {
        match self.next().expect("branch checked for input") {
            '(' => {
                if self.depth == MAX_DEPTH {
                    return Err(format!("groups nested deeper than {MAX_DEPTH} levels"));
                }

                self.depth += 1;
                self.out.push_str("(?:");
                self.regexp()?;
                if self.next() != Some(')') {
                    return Err("unclosed group".into());
                }

                self.out.push(')');
                self.depth -= 1;
            }
            '.' => self.out.push_str(r"[^\n\r]"),
            '[' => self.class()?,
            '\\' => self.escape()?,
            c @ ('*' | '+' | '?') => return Err(format!("`{c}` has nothing to repeat")),
            c @ ('{' | '}' | ']') => return Err(format!("`{c}` must be escaped")),
            c => self.lit(c),
        }

        Ok(())
    }

    /// `quantifier = "*" / "+" / "?" / "{" n [ "," [ m ] ] "}"`
    fn quantifier(&mut self) -> Result<(), String> {
        match self.peek() {
            Some(c @ ('*' | '+' | '?')) => {
                self.i += 1;
                self.out.push(c);
            }
            Some('{') => {
                self.i += 1;
                let n = self.number()?.ok_or("`{` must start a quantifier such as {2} or {1,3}")?;
                let q = if self.peek() == Some(',') {
                    self.i += 1;
                    match self.number()? {
                        Some(m) if m < n => return Err(format!("quantifier {{{n},{m}}} has its bounds reversed")),
                        Some(m) => format!("{{{n},{m}}}"),
                        None => format!("{{{n},}}"),
                    }
                } else {
                    format!("{{{n}}}")
                };

                if self.next() != Some('}') {
                    return Err("unclosed quantifier".into());
                }

                self.out.push_str(&q);
            }
            _ => {}
        }

        Ok(())
    }

    /// `QuantExact = 1*DIGIT`, if present.
    fn number(&mut self) -> Result<Option<u32>, String> {
        let st = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }

        if st == self.i {
            return Ok(None);
        }

        let s: String = self.c[st..self.i].iter().collect();
        s.parse().map(Some).map_err(|_| format!("quantifier bound {s} is too large"))
    }

    /// After `\` outside a class: `SingleCharEsc` or `charClassEsc`.
    fn escape(&mut self) -> Result<(), String> {
        if let Some('p' | 'P') = self.peek() {
            return self.category();
        }

        let c = self.single_escape()?;
        self.lit(c);
        Ok(())
    }

    /// After `\`: a `SingleCharEsc`, returned as the character it stands for.
    fn single_escape(&mut self) -> Result<char, String> {
        match self.next() {
            Some('n') => Ok('\n'),
            Some('r') => Ok('\r'),
            Some('t') => Ok('\t'),
            Some(c @ ('(' | ')' | '*' | '+' | '-' | '.' | '?' | '[' | '\\' | ']' | '^' | '{' | '|' | '}')) => Ok(c),
            Some('1'..='9') => Err("backreferences are not supported".into()),
            Some(c) => Err(format!("\\{c} is not supported")),
            None => Err("trailing `\\`".into()),
        }
    }

    /// After `\`: `p{Cat}` or `P{Cat}` with an RFC 9485 category.
    fn category(&mut self) -> Result<(), String> {
        let p = self.next().expect("caller peeked p or P");
        if self.next() != Some('{') {
            return Err(format!("expected `{{` after \\{p}"));
        }

        let st = self.i;
        while self.peek().is_some_and(|c| c != '}') {
            self.i += 1;
        }

        let name: String = self.c[st..self.i].iter().collect();
        if self.next() != Some('}') {
            return Err(format!("unclosed \\{p}{{"));
        }

        if !CATEGORIES.contains(&name.as_str()) {
            return Err(format!("unknown category \\{p}{{{name}}}"));
        }

        let _ = write!(self.out, "\\{p}{{{name}}}");
        Ok(())
    }

    /// After `[`: `[ "^" ] ( "-" / CCE1 ) *CCE1 [ "-" ] "]"`.
    fn class(&mut self) -> Result<(), String> {
        self.out.push('[');
        if self.peek() == Some('^') {
            self.i += 1;
            self.out.push('^');
        }

        let mut first = true;
        loop {
            match self.peek() {
                None => return Err("unclosed character class".into()),
                Some(']') if first => return Err("empty character class".into()),
                Some(']') => {
                    self.i += 1;
                    self.out.push(']');
                    return Ok(());
                }
                Some('-') if first || self.c.get(self.i + 1) == Some(&']') => {
                    self.i += 1;
                    self.lit('-');
                }
                Some('-') => return Err("`-` in a class must be first, last or escaped".into()),
                Some('\\') if matches!(self.c.get(self.i + 1), Some('p' | 'P')) => {
                    self.i += 1;
                    self.category()?;
                }
                Some(_) => {
                    let lo = self.class_char()?;
                    self.lit(lo);
                    if self.peek() == Some('-') && !matches!(self.c.get(self.i + 1), None | Some(']')) {
                        self.i += 1;
                        let hi = self.class_char()?;
                        if hi < lo {
                            return Err(format!("class range {lo}-{hi} is reversed"));
                        }

                        self.out.push('-');
                        self.lit(hi);
                    }
                }
            }

            first = false;
        }
    }

    /// `CCchar`: any character but `-`, `[`, `\` and `]`, or a `SingleCharEsc`.
    fn class_char(&mut self) -> Result<char, String> {
        match self.next() {
            Some('\\') => self.single_escape(),
            Some('[') => Err("`[` in a class must be escaped".into()),
            Some('-') => Err("`-` in a class must be first, last or escaped".into()),
            Some(c) => Ok(c),
            None => Err("unclosed character class".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(p: &str) -> Pattern {
        Pattern::new(p).unwrap_or_else(|e| panic!("{p:?}: {e}"))
    }

    #[test]
    fn whole_string_and_literal_anchors() {
        let p = ok("[a-z0-9]([a-z0-9-]*[a-z0-9])?");
        assert!(p.is_match("abc-1"));
        assert!(!p.is_match("ab_c"));
        assert!(!p.is_match("abc-"));
        let caret = ok("^[a-z]+$");
        assert!(caret.is_match("^abc$"));
        assert!(!caret.is_match("abc"));
    }

    #[test]
    fn dot_excludes_line_breaks() {
        let p = ok("a.c");
        assert!(p.is_match("a☃c"));
        assert!(!p.is_match("a\nc"));
        assert!(!p.is_match("a\rc"));
        assert!(ok("[.]").is_match("."));
        assert!(!ok("[.]").is_match("x"));
    }

    #[test]
    fn every_category_compiles() {
        for c in CATEGORIES {
            ok(&format!("\\p{{{c}}}"));
            ok(&format!("[\\P{{{c}}}x]"));
        }

        assert!(ok("\\p{Lu}\\p{Ll}*").is_match("Αθήνα"));
    }

    #[test]
    fn class_set_operators_are_literal() {
        let p = ok("[a&&b]+");
        assert!(p.is_match("a&b"));
        assert!(ok("[~-]").is_match("-"));
        assert!(ok("[-a]").is_match("-"));
        assert!(ok("[^-]").is_match("x"));
    }

    #[test]
    fn rejects_non_i_regexp() {
        for (p, why) in [
            ("(a", "unclosed group"),
            ("a)", "unmatched"),
            ("(a)\\1", "backreferences"),
            ("\\d+", "\\d is not supported"),
            ("\\w", "\\w"),
            ("(?:a)", "nothing to repeat"),
            ("a{,3}", "quantifier"),
            ("a{3,2}", "reversed"),
            ("[]", "empty"),
            ("[^]", "empty"),
            ("[a-]b-c]", "must be escaped"),
            ("[z-a]", "reversed"),
            ("\\p{IsGreek}", "unknown category"),
            ("\\p{Cs}", "unknown category"),
            ("a{", "quantifier"),
            ("*", "nothing to repeat"),
        ] {
            let e = Pattern::new(p).expect_err(p);
            assert!(e.contains(why), "{p:?}: {e}");
        }
    }

    #[test]
    fn ecmascript_translation() {
        assert_eq!(ok(r"a.[.]\.b").to_ecmascript(), r"^(?:a[^\n\r][.]\.b)$");
        assert_eq!(ok(r"[\].]").to_ecmascript(), r"^(?:[\].])$");
        assert_eq!(ok(r"^a$[^$]\-[\-]").to_ecmascript(), r"^(?:\^a\$[^$]-[\-])$");
    }

    #[test]
    fn bounds_work() {
        assert!(Pattern::new(&"(".repeat(100)).unwrap_err().contains("nested"));
        assert!(Pattern::new("(((a{1000}){1000}){1000})").unwrap_err().contains("@len"));
        // Unicode identifiers with a length cap fit one pattern's bound.
        ok(r"\p{L}{1,63}");
        // Many short but large patterns exhaust the shared budget.
        let budget = Cell::new(SCHEMA_BUDGET);
        let e = (0..100).find_map(|_| Pattern::new_within("a{30000}", &budget).err()).expect("budget runs out");
        assert!(e.contains("MiB"), "{e}");
        assert!(Pattern::new(&"a".repeat(MAX_LEN + 1)).unwrap_err().contains("longer"));
    }
}
