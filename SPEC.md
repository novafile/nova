# NOVA — Normalized Object and Value Architecture

**Specification, version 0.1 (draft)**
Status: working draft accompanied by a reference implementation in Rust (this repository).

---

## Contents

0. [Conventions](#0-conventions)
1. [Overview and design principles](#1-overview-and-design-principles)
2. [The value model](#2-the-value-model)
3. [Representations and fidelity](#3-representations-and-fidelity)
4. [Text syntax](#4-text-syntax)
5. [Binary encoding](#5-binary-encoding)
6. [Schemas](#6-schemas)
7. [Paths, queries and patches](#7-paths-queries-and-patches)
8. [The agent interaction model](#8-the-agent-interaction-model)
9. [Templates and composition](#9-templates-and-composition)
10. [Token-aware representation selection](#10-token-aware-representation-selection)
11. [Interoperability mappings](#11-interoperability-mappings)
12. [Security considerations](#12-security-considerations)
13. [Conformance](#13-conformance)
14. [Reference implementation](#14-reference-implementation)
- [Appendix A — Grammar summary](#appendix-a--grammar-summary)
- [Appendix B — Comparison with existing formats](#appendix-b--comparison-with-existing-formats)
- [Appendix C — Open questions and future work](#appendix-c--open-questions-and-future-work)

---

## 0. Conventions

The key words MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT, MAY and OPTIONAL are to be interpreted as described in RFC 2119 and RFC 8174 when, and only when, they appear in all capitals.

*Non-normative* sections and notes explain intent; they impose no requirements.

Grammar fragments use an EBNF variant: `"x"` is a literal, `[a]` is optional, `a*` is zero or more, `a+` is one or more, `a | b` is alternation, and `'a'..'z'` is a character range. `SP` is U+0020, `NL` is U+000A (optionally preceded by U+000D).

---

## 1. Overview and design principles

NOVA is **one precise semantic model with several interchangeable representations**, each optimized for a different consumer:

| Consumer | Representation | Optimized for |
| --- | --- | --- |
| People editing configuration | Human text (§4, profile *human*) | Readability, diffs, comments |
| LLMs reading data | Compact text (§4.9, profile *compact*) | Few tokens, explicit structure |
| Services, storage | Binary (§5) | Size, speed, streaming |
| Agents changing state | Queries and patches (§7) | Reading and writing *parts* of a document |
| Authors reusing structure | Templates (§9) | Composition without text templating |

Design principles:

1. **Semantics first.** Every representation maps to the same value model (§2). Converting between representations never changes a value unless the conversion is explicitly declared lossy (§11).
2. **No surprising conversions.** The only bare words with special meaning are `true`, `false`, `null`, and tokens that match the strict number grammar (§4.5). Everything else is a string. Richer types are written explicitly or supplied by a schema (§6.3).
3. **Partial information is a first-class concept.** Agents read projections (§7.3) and write conditional, atomically applied patches (§7.4) rather than whole documents.
4. **Data is not code.** Data documents contain no expressions, references or directives that execute. Composition lives in a separate, total template layer (§9) that is rendered *before* any consumer sees the data.
5. **Determinism.** Every value has exactly one canonical binary encoding and one canonical text form, from which equality, ordering and revisions are derived (§2.12–2.13).
6. **Reuse proven techniques.** The binary encoding borrows varints, zigzag encoding and length prefixes from Protobuf and CBOR; tables borrow from TOON and CSV; patches borrow from JSON Patch. Novelty is reserved for where existing formats fall short.

---

## 2. The value model

A NOVA **value** is one of the following kinds. The model is a tree: there are no references, aliases or cycles.

| Kind | Description |
| --- | --- |
| `null` | The absence of a value. |
| `bool` | `true` or `false`. |
| `int` | A mathematical integer. |
| `float` | An IEEE 754 binary64 number. |
| `decimal` | An exact base-10 number. |
| `rational` | An exact fraction. |
| `string` | A sequence of Unicode scalar values. |
| `bytes` | A sequence of octets. |
| `timestamp` | An instant on the UTC time line. |
| `duration` | A signed, exact span of time. |
| `list` | An ordered sequence of values. |
| `tuple` | A fixed-length ordered sequence of values; distinct from `list`. |
| `set` | An unordered collection of distinct values. |
| `record` | A collection of string-keyed fields with unique keys. |
| `map` | A collection of entries with unique keys of any kind. |
| `variant` | A tagged value: a tag and an optional payload. |

### 2.1 Integers

An `int` is an arbitrary mathematical integer. Implementations MUST support at least the range [−2^63, 2^64 − 1] and MUST reject (not truncate or round) integers outside their supported range. The reference implementation supports the signed 128-bit range.

### 2.2 Floats

A `float` is an IEEE 754 binary64 value, including −0.0, +∞, −∞ and NaN.

- `+0.0` and `−0.0` are **distinct** values.
- All NaN values are **equal** to each other and are canonicalized to the bit pattern `0x7FF8000000000000`.
- `int` and `float` are distinct kinds: `1` ≠ `1.0`.

### 2.3 Decimals

A `decimal` is `c × 10^e` with integer coefficient `c` and integer exponent `e` (32-bit signed). Decimals are **normalized**: `c` has no trailing decimal zeros, and zero is `0 × 10^0`. Hence `1.50` and `1.5` are the same value. Scale is not preserved; schemas that need a fixed scale constrain it separately. Implementations MUST support at least 38 significant digits.

### 2.4 Rationals

A `rational` is `n / d` with integers `n` and `d`, `d > 0`, normalized to lowest terms (`gcd(|n|, d) = 1`; zero is `0/1`). Hence `2/4` and `1/2` are the same value. `n/1` is still a rational, distinct from the integer `n`. Implementations MUST support numerators and denominators of at least 64 bits; the reference implementation supports signed 128-bit components.

> *Note (non-normative).* Rationals exist so that exact ratios — aspect ratios, sampling rates, probabilities, musical intervals, `22/7` — survive serialization. Converting a rational to a float is lossy and is therefore never performed implicitly (§6.3). Language bindings MAY expose rationals as a native float when the application opts in to that loss.

### 2.5 Numeric kinds are distinct

`int`, `float`, `decimal` and `rational` are separate kinds. Values of different numeric kinds are never equal (`1`, `1.0`, `@decimal("1")` and `1/1` are four different values), and no implementation performs implicit numeric conversion. The only conversions are the lossless ones that a schema requests (§6.3).

### 2.6 Timestamps

A `timestamp` is an instant: whole seconds since 1970-01-01T00:00:00Z plus nanoseconds in [0, 10^9). Leap seconds are not represented. Implementations MUST support years 0001–9999 with nanosecond precision.

A timestamp carries **no time zone or offset**. An offset in the text form (§4.8) is presentation and is applied when parsing; it is not preserved.

### 2.7 Durations

A `duration` is a signed number of nanoseconds. There are no calendar units: a day is exactly 86 400 seconds. Implementations MUST support magnitudes up to (2^63 − 1) seconds.

### 2.8 Strings and bytes

A `string` is a sequence of Unicode scalar values (surrogate code points are excluded). Strings are compared by code point; **no Unicode normalization** is performed by NOVA. A `bytes` value is an arbitrary octet sequence.

### 2.9 Lists, tuples and sets

A `list` is ordered and may contain duplicates. A `tuple` is ordered with a fixed length; `()` and `(x,)` are valid tuples. A list and a tuple with the same elements are different values. A `set` contains no two equal elements (§2.12); duplicates MUST be rejected when constructing, parsing or decoding a set. Set element order is not part of the value.

### 2.10 Records

A `record` maps string keys to values. Keys MUST be unique; duplicate keys MUST be rejected by every parser and decoder. **Field order is not part of the value**: two records with the same fields are equal regardless of order. Implementations SHOULD retain the order in which fields were written so that human-oriented output follows the author's order.

### 2.11 Maps and variants

A `map` maps keys of any kind to values. Keys MUST be unique (by §2.12 equality). Entry order is not part of the value.

A `variant` is a tag — a name matching `[A-Za-z_][A-Za-z0-9_-]*` — and an optional payload value. `!active` and `!failed("disk full")` are variants. A schema's `enum` (§6.1) constrains which tags and payloads are permitted. Variants express both C-style enums (no payload) and tagged unions (payload).

### 2.12 Equality and ordering

Two values are **equal** if and only if their canonical binary encodings (§5.2) are byte-for-byte identical. Consequently:

- values of different kinds are never equal;
- record, set and map equality ignore order;
- NaN equals NaN; `0.0` does not equal `-0.0`.

The **canonical order** of values is the lexicographic order of their canonical encodings. It is used to order set elements and map keys in canonical form. It is a total order, but it is not numeric order and SHOULD NOT be presented to users as one.

### 2.13 Revisions

The **revision** of a value is the SHA-256 digest of its canonical binary encoding, written `sha256:` followed by 64 lowercase hexadecimal digits. Revisions identify document states for optimistic concurrency (§7.5). Where a revision is given as a precondition it MAY be abbreviated to a prefix of at least 8 hexadecimal digits.

> *Note.* A revision covers values only. Comments, formatting and field order do not affect it, so reformatting a document never invalidates a pending patch.

---

## 3. Representations and fidelity

A **representation** maps values to octets and back. NOVA defines:

| Representation | Section | Self-describing | Needs schema |
| --- | --- | --- | --- |
| Text, profile *human* | §4 | yes | no |
| Text, profile *compact* | §4.9 | yes | no |
| Text, profile *canonical* | §4.10 | yes | no |
| Binary value encoding and frames | §5 | yes | no |
| Positional text (experimental) | Appendix C | no | yes |

Conversions are classified by **fidelity level**:

- **Value-preserving** — the decoded value equals the original (§2.12). All conversions among NOVA representations MUST be value-preserving.
- **Document-preserving** — additionally preserves comments, field order, blank lines, and the author's choice among equivalent spellings. This applies to tools that edit text in place (formatters, refactoring tools, schema migrations). A conforming *editor* (§13) MUST be document-preserving for edits it did not intend to make.

The canonical form of a value preserves the value and nothing else. Comments are presentation metadata: they belong to a document, not to its value, and are discarded by value-preserving conversions such as text → binary.

---

## 4. Text syntax

### 4.1 Encoding and lines

A text document is UTF-8. A leading byte-order mark is ignored. Lines end with LF or CRLF. Tab characters MUST NOT appear in indentation; they MAY appear inside strings and comments, and as inline whitespace between tokens.

A **comment** begins with `#` at a token boundary — at the start of a line (after indentation), after whitespace, or after a complete flow value — and extends to the end of the line. Within a plain scalar (§4.4), only `#` preceded by whitespace starts a comment.

### 4.2 Documents and directives

```
document  = directive* body
directive = "@" name [SP argument] NL        ; at column 1, before the body
```

A directive is `@` immediately followed by a name and then whitespace or end of line. (`@timestamp(...)` is a typed literal, not a directive, because a `(` follows the name.)

Directives defined for **data documents**:

| Directive | Meaning |
| --- | --- |
| `@nova <version>` | The NOVA version the document targets. OPTIONAL. |
| `@schema <ref>` | The schema the document claims to conform to: a name, optionally with a fingerprint (`infrastructure/v1#8c21a0f3d5e6b7c8`). Informational; parsers MUST NOT fetch it. |
| `@revision <rev>` | The revision of the document the body was taken from (used in query responses, §7.3). |
| `@scope <path>` | The location within that document the body was taken from (§7.3). |
| `@envelope <kind>` | Marks an agent-result envelope (§8.2). |

Template documents (§9), patch documents (§7.4) and query documents (§7.3) define their own directives. Directive names beginning with `x-` are reserved for extensions and MUST be ignored by implementations that do not understand them. Any other unknown directive MUST be rejected.

The **body** is one of:

- empty — the document's value is the empty record `{}`;
- a block record (§4.3), if the first line is a key;
- a block list (§4.3), if the first line starts with `- `;
- a top-level table (§4.9), if the first line is `[N]{...}:`;
- a single flow value (§4.7) on its own.

Content after the body is an error.

### 4.3 Block structure

Block structure is expressed by indentation. **Indentation is fixed by the grammar**: the children of a line are indented by *exactly two spaces* more than that line. Any other indentation is an error. There is no context-dependent interpretation of indentation.

**Block records.** Each line is an entry:

```
key: value              # value on the same line (§4.6)
key:                    # value is the indented block below
  nested: 1
key[N]{a,b}:            # a table (§4.9)
  1, 2
```

Keys are bare identifiers `[A-Za-z_][A-Za-z0-9_-]*` or quoted strings (§4.6). The key MUST be followed by `:` and then a space, a comment, or the end of the line. Duplicate keys are an error.

A key followed by nothing MUST be followed by an indented block; writing `key:` with no value and no block is an error (write `null`, `{}` or `[]` explicitly).

**Block lists.** Each item starts with `- ` at the list's indentation:

```
services:
  - id: auth            # an item that is a record: first field on the dash line,
    port: 8080          #   further fields indented two spaces past the dash
  - plain string item
  - [1, 2]              # a flow value
  -                     # a nested block on the following lines
    - nested list item
```

A list item MUST NOT start another list item on the same line (`- - x`); use a nested block or a flow list.

### 4.4 Plain scalars and token classification

After `key: ` or `- `, a value that does not begin with one of `" [ { ( @ ! $ |` is a **plain scalar**: the rest of the line, up to a comment, with trailing whitespace removed.

A plain scalar MUST NOT

- begin with any **reserved character** ``! " # $ % & ' * , : < = > ? @ [ \ ] ^ ` { | } ~ ( )``,
- begin with `...` or with `- `, or be exactly `-`,
- contain `: ` (colon followed by space),
- end with `:`,
- contain control characters other than tab.

Violations are errors that ask the author to quote the string. This removes the most common ambiguities of YAML (`a: b: c`, `key: - x`).

Every **bare token** — a plain scalar or a flow word (§4.7) — is classified by exactly these rules:

1. `true`, `false` → `bool`; `null` → `null`.
2. A token that matches the number grammar (§4.5) exactly → `int`, `float` or `rational`.
3. Anything else → `string`.

There are no other implicit conversions. `yes`, `NO`, `on`, `1.2.3`, `08080` and `2026-11-17T16:00:00Z` are all strings. A schema MAY resolve a string into a richer type (§6.3); without a schema, write a typed literal (§4.8).

### 4.5 Numbers

```
int      = ["-"] ("0" | '1'..'9' digit*)
float    = int ( frac [exp] | exp )
frac     = "." digit+
exp      = ("e" | "E") ["+" | "-"] digit+
rational = int "/" '1'..'9' digit*
```

- No leading `+`, no leading zeros, no leading or trailing `.`, no digit separators, no hexadecimal.
- `-0` is the integer 0; `-0.0` is the float −0.0.
- An `int` outside the implementation's range is an error.
- A `float` literal whose value overflows to infinity is an error. Infinities and NaN are written `@float("inf")`, `@float("-inf")`, `@float("nan")`.
- A `rational` is normalized on parsing (`2/4` reads as `1/2`). `1/0`, `1/02` and `1/-2` do not match the grammar and are therefore strings.

> *Note.* Because `11/17` matches the rational grammar, it is a rational, not a string. Authors who mean the string must quote it; writers quote such strings automatically. This is the same rule that makes `8080` an integer.

### 4.6 Strings

**Quoted strings** use double quotes and the JSON escapes `\" \\ \/ \b \f \n \r \t \uXXXX` (with surrogate pairs), plus `\u{X…}` (1–6 hex digits) and `\$` (a literal `$`). Raw control characters, including line breaks, MUST NOT appear inside quoted strings.

**Block strings** hold multi-line text:

```
script: |               # keep: content plus exactly one final newline
  echo "hello"
    indented line
notes: |-               # strip: no final newline
  first
  second
```

The content lines are indented exactly two spaces more than the line holding `|`; that indentation is removed and everything after it, including further spaces, `#` and quotes, is content. A whitespace-only line shorter than the content indentation is an empty content line. The block ends at the first non-blank line with less indentation. Trailing empty lines are discarded. `|` appends one `\n` to non-empty content; `|-` appends nothing.

Strings that cannot be represented this way — for example, ones with several trailing newlines — are written quoted.

### 4.7 Flow values

Flow values can appear anywhere a value can. Inside brackets, newlines and comments are whitespace, so flow values can span lines.

```
[1, two, "three"]              list          (trailing comma allowed)
{id: auth, "odd key": 1}       record
(1, 2)  (1,)  ()               tuples        ((x) without a comma is an error)
@set[a, b]                     set
@map{1 => one, (1, 2) => pair} map           (keys are any flow value)
!active  !failed("disk full")  variants      (payload in parentheses)
```

A **flow word** is a bare token in flow context. It ends at whitespace, at any of `, [ ] { } ( ) "`, or at a `:` that is followed by whitespace, a delimiter or the end of input. It MUST NOT begin with a reserved character (§4.4) or with `...`. Flow words are classified as in §4.4; for example `ghcr.io/example/auth:v2` is a single string.

Record keys in flow context are identifiers or quoted strings, followed by `:`.

### 4.8 Typed literals

| Literal | Kind | Argument |
| --- | --- | --- |
| `@timestamp("2026-11-17T16:00:00Z")` | timestamp | RFC 3339 date-time: `YYYY-MM-DDTHH:MM:SS[.F{1,9}](Z\|±HH:MM)`. Uppercase `T` and `Z`. |
| `@duration("1h30m")` | duration | `-?(Nd)?(Nh)?(Nm)?(N[.F{1,9}]s)?` — at least one component, units in this order. |
| `@decimal("12.50")` | decimal | `-?D+(.D+)?([eE][+-]?D+)?` |
| `@rational("22/7")` | rational | as §4.5 |
| `@bytes("AP8H")` | bytes | Standard base64 with padding (RFC 4648 §4). |
| `@float("nan")` | float | `nan`, `inf`, `-inf`, or a number. |

The argument MUST be a quoted string literal.

### 4.9 Tables

A list of records with scalar fields can be written as a table. This is the core of the compact profile, and is also allowed in human text.

```
services[2]{id,image,port,replicas,enabled}:
  auth, ghcr.io/example/auth:v2, 8080, 3, true
  api,  ghcr.io/example/api:v5,  3000, 5, true
```

- The header is `key[N]{col, …}:` where `N` is the exact row count. Columns are identifiers or quoted strings and MUST be unique.
- Rows follow, indented two spaces past the header. There MUST be exactly `N` rows; a missing or extra row is an error, which catches truncated LLM output.
- Each row is a comma-separated sequence of cells, one per column. A cell is a flow value, so commas, quotes and newlines in text require quoting, exactly as elsewhere. **Rows are never split on raw commas.**
- An **empty cell** means the field is *absent* from that row's record. Write `null` for a present null field.
- Each row denotes a record whose fields are the non-empty cells, in column order.
- At the top level, the header is `[N]{col, …}:`.

### 4.10 Writer profiles

Writers MUST produce text that parses to a value equal to the input. The reference writer defines three profiles.

- **human** — block layout; fields in stored order; short scalar lists in flow form; multi-line strings as block strings; no tables.
- **compact** — like *human*, but a list of at least two records whose field values are all scalars, and that is at least half dense, is written as a table; flow lists use `,` without a space.
- **canonical** — *human* applied after sorting record keys by UTF-8 bytes and ordering set elements and map keys canonically (§2.12). Two equal values have byte-identical canonical text.

Canonical scalar spellings, which all writers SHOULD use:

| Kind | Canonical text |
| --- | --- |
| int | Decimal digits, `-` for negatives. |
| float | The shortest digit string that round-trips. Scientific notation (`1e300`, `1e-7`) when the decimal exponent is < −4 or ≥ 16, otherwise positional; the output always contains `.` or `e` (`1.0`). Non-finite values as `@float(...)`. |
| decimal | Positional when 0 ≤ e ≤ 6 (`1500`), or when e < 0 and at most five zeros follow the point (`0.0001`); otherwise `<coefficient>e<exponent>` (`12e-21`). |
| rational | `n/d` in lowest terms (`-22/7`, `2/1`). |
| timestamp | UTC with `Z`; the fraction is omitted when zero, otherwise trailing zeros are trimmed. |
| duration | Largest units first (`d h m s`), zero components omitted, `0s` for zero, fraction trimmed. |
| bytes | Standard base64 with padding. |
| string | A plain scalar / flow word if that spelling re-parses to the same string; otherwise quoted, with the minimal escapes. |

### 4.11 Text streams

A text stream is a sequence of documents separated by lines consisting exactly of `---`. Each document is parsed independently; an error in one document does not prevent reading the others.

---

## 5. Binary encoding

### 5.1 Value encoding

Every value is encoded as a one-byte **type tag** followed by a payload.

Notation:

- `varint(u)` — unsigned LEB128, minimal length. Decoders MUST reject non-minimal encodings, for example a trailing `0x00` continuation byte.
- `zigzag(i)` — `(i << 1) XOR (i >> 127)`, which maps signed integers to unsigned ones (0, −1, 1, −2, …).
- `str` — a string payload (§5.3).

| Tag | Kind | Payload |
| --- | --- | --- |
| `0x00` | null | — |
| `0x01` | false | — |
| `0x02` | true | — |
| `0x03` | int | `varint(zigzag(i))` |
| `0x04` | float | 8 bytes, IEEE 754 binary64, big-endian; NaN as `7FF8000000000000` |
| `0x05` | decimal | `varint(zigzag(c))` `varint(zigzag(e))`, normalized (§2.3) |
| `0x06` | string | `str` |
| `0x07` | bytes | `varint(len)` bytes |
| `0x08` | timestamp | `varint(zigzag(seconds))` `varint(nanos)` |
| `0x09` | duration | `varint(zigzag(nanoseconds))` |
| `0x0A` | list | `varint(n)` value×n |
| `0x0B` | tuple | `varint(n)` value×n |
| `0x0C` | set | `varint(n)` value×n |
| `0x0D` | record | `varint(n)` (`str` key, value)×n |
| `0x0E` | map | `varint(n)` (key value, value)×n |
| `0x0F` | unit variant | `str` tag |
| `0x10` | variant with payload | `str` tag, value |
| `0x11` | rational | `varint(zigzag(n))` `varint(d)`, normalized (§2.4) |

Decoders MUST reject:

- unknown tags;
- truncated input;
- element counts that exceed the remaining input;
- invalid UTF-8;
- duplicate record keys, set elements or map keys;
- non-normalized decimals or rationals;
- out-of-range timestamps or durations;
- nesting deeper than their limit (at least 64; the reference implementation uses 128).

### 5.2 Canonical encoding

The **canonical encoding** of a value is the encoding above with:

- record fields sorted by the UTF-8 bytes of their keys;
- set elements, and map entries by key, sorted by the bytewise order of their canonical encodings;
- no string back-references (§5.3);
- NaN canonicalized.

It is the basis of equality, ordering and revisions (§2.12–2.13).

### 5.3 String payloads and back-references

A string payload starts with `h = varint(...)`:

- if `h` is even, a literal of `h >> 1` UTF-8 bytes follows;
- if `h` is odd, the string is entry `h >> 1` of the **string table**.

The string table is empty at the start of each top-level value. Every **literal** string payload — record keys, string values and variant tags — is appended to it in encoding order, by encoders and decoders alike. An encoder MAY replace any later occurrence of a string with a back-reference, and SHOULD do so only when the reference is shorter than the literal. Repeated record keys in lists of records therefore cost one or two bytes each. Decoders MAY bound the table size; the reference implementation allows 2^20 entries.

### 5.4 Frames

A frame wraps one value for transport or storage:

```
frame = magic  "NOVA" (4E 4F 56 41)
        version 0x01
        flags   u8
        [schema: varint(len) UTF-8]        if flags & 0x01
        length  varint
        payload length bytes               (one §5.1 value, exactly)
        [crc32: u32 big-endian]            if flags & 0x04
```

| Flag bit | Meaning |
| --- | --- |
| `0x01` SCHEMA | A schema reference follows (§6.6). On a negotiated connection it MAY be omitted. |
| `0x02` CANONICAL | The payload is in canonical form; the receiver can hash it directly. |
| `0x04` CRC32 | A CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`) of the payload follows it. |
| `0x08` DEDUPE | The payload may contain back-references. Informational. |

Unknown flag bits and unknown versions MUST be rejected.

### 5.5 Binary streams

A binary stream is a concatenation of frames. A reader in **recovering** mode that encounters a malformed frame MUST report it, then resume at the next occurrence of the magic bytes after the start of the bad frame. Writers that need reliable recovery SHOULD set the CRC32 flag.

---

## 6. Schemas

Schemas are **optional**. Schema-free documents are fully self-describing. A schema adds types, constraints, defaults, stable field identities and evolution rules.

### 6.1 Schema language

Schemas are written in their own small line-oriented language (file extension `.novs`):

```
@schema infrastructure/v1          # name (used in fingerprints and @schema references)
@root Infrastructure               # the type of a whole document

type Port = u16 @range(1..65535)   # alias

record Service:
  id: string @key @id(1)
  image: string @len(1..) @id(2)
  port: Port @id(3)
  replicas: u16 @range(0..100) @id(4)
  enabled: bool @id(5) = true      # default
  tags?: set<string> @id(6)        # optional field

enum Status:                       # also: enum Status = active | draining | failed(string)
  active
  draining
  failed(string)                   # variant with a payload

record Infrastructure:
  environment: string
  deployed_at: timestamp
  status: Status = !active
  services: Service[]
```

- Definition names start with an uppercase letter. Fields are indented exactly two spaces.
- A field line is `name[?]: type attributes* [= default]`, where the default is a flow value.
- `#` starts a comment when it is at the start of a line or preceded by a space, outside quotes.

**Types**

| Syntax | Meaning |
| --- | --- |
| `any` | any value |
| `null` `bool` `float` `decimal` `rational` `string` `bytes` `timestamp` `duration` | the corresponding kind |
| `int` `i8` `i16` `i32` `i64` `u8` `u16` `u32` `u64` | integers, range-limited by name |
| `list<T>`, `T[]` | list |
| `set<T>` | set |
| `map<K, V>` | map |
| `(A, B, …)` | tuple |
| `T?` | `T` or `null` (nullable) |
| `Name` | a `record`, `enum` or `type` definition |

`name?:` (an optional field, which may be absent) and `T?` (a nullable type, where the field is present but may be `null`) are independent.

**Attributes**

| Attribute | Applies to | Meaning |
| --- | --- | --- |
| `@range(lo..hi)` | numeric types | Inclusive bounds; either side may be omitted. |
| `@len(lo..hi)` | string (counted in Unicode scalar values), bytes, list, set, map, record | Inclusive length bounds. |
| `@key` | field | Part of the record's identity: values of this record in the same list MUST differ in their key fields. Key fields MUST be required and non-nullable. |
| `@id(n)` | field | Stable numeric identity for evolution checks and positional encodings. Unique per record. |
| `@deprecated` | field | Informational. |
| `@open` | record (after the name) | Unknown fields are accepted and preserved. Records are **closed** by default. |

### 6.2 `@key` and selectors

`@key` guarantees that a selector such as `services[id=api]` (§7.1) identifies at most one element of any valid document. This is what makes keyed patches safe.

### 6.3 Schema-directed resolution

**Resolution** checks a value against a type and applies a fixed set of **lossless conversions**. These exist so that human documents can stay free of type markers when a schema is available. Resolution MUST perform no conversions other than those listed:

| Target type | Accepted input | Rule |
| --- | --- | --- |
| `timestamp` | string | RFC 3339 (§4.8). |
| `duration` | string | Duration syntax (§4.8). |
| `decimal` | string, int, float, rational | Decimal syntax; an int exactly; a finite float via its shortest round-trip digits; a rational only if its denominator has no prime factors other than 2 and 5. |
| `rational` | string, int, decimal | Rational syntax; `n` becomes `n/1`; a decimal exactly. |
| `float` | int | Only if \|i\| ≤ 2^53. |
| `bytes` | string | Base64 (§4.8). |
| `set<T>` | list | Only if there are no duplicates. |
| `(A, …)` | list | Only if the lengths match. |
| `map<K, V>` | record | Keys become strings. |
| enum type | string | Only if the string names a variant without a payload: `active` becomes `!active`. |

Notably, resolution never converts a float to a rational or integer, a rational to a float, a number to a string, or a string to a number or boolean.

### 6.4 Validation

Resolving a value against a record type:

1. Every present field is resolved against its declared type.
2. A missing field with a default receives the default, during resolution only.
3. A missing optional field stays absent.
4. Any other missing field is an error.
5. Unknown fields are errors unless the record is `@open`.
6. `@range`, `@len` and `@key` constraints are checked.

Implementations SHOULD report **all** violations, each with the path (§7.1) of the offending value, rather than stopping at the first.

**Validate** checks the same rules without producing a resolved value. **Resolve** returns the value with conversions and defaults applied. The revision of a document is computed from the document as stored, not from its resolution.

### 6.5 Evolution

A new schema version is **backward compatible** with an old one if every value valid under the old schema resolves under the new one — in other words, new readers can read old data. Tools SHOULD report changes as compatible or breaking. The rules:

| Change | Classification |
| --- | --- |
| Add an optional field, or a field with a default | compatible |
| Add a required field without a default | breaking |
| Remove a field | breaking (old data contains it), unless the record is `@open` |
| Close an `@open` record | breaking |
| Widen a type: integer to wider integer, integer to `decimal`/`rational`, `decimal` to `rational`, small integer to `float`, `T` to `T?`, `T` to `any`, wider `@range`/`@len` | compatible |
| Narrow or change a type | breaking |
| Make an optional field required (without a default) | breaking |
| Add a variant | compatible (old readers reject the new variant: this is a forward-compatibility concern) |
| Remove a variant, or change its payload incompatibly | breaking |
| Add `@key` | breaking (existing data may contain duplicates) |
| Rename a field that keeps the same `@id` | breaking for name-based representations (all text forms); compatible only for positional or ID-based encodings |

### 6.6 Fingerprints

The **canonical schema text** renders:

1. `@schema`;
2. `@root`;
3. every definition, sorted by name, with fields in declared order and types in their long form (`list<T>`, not `T[]`).

The **fingerprint** is `<name>#<h>`, where `<h>` is the first 16 hexadecimal digits of the revision (§2.13) of the canonical text, taken as a NOVA string value. Fingerprints are unaffected by comments and formatting. A `@schema` directive or frame MAY carry a fingerprint so that the receiver can confirm it holds the exact schema.

---

## 7. Paths, queries and patches

### 7.1 Paths

A **path** addresses a location within a value. The path language is deliberately small and **not Turing-complete**: it has no expressions beyond equality, no functions, and no recursion.

```
path       = "" | "." | first ( "." name | "." projection | "[" selector "]" )*
first      = name | "[" selector "]"
name       = identifier | quoted-string
selector   = "*" | int | identifier "=" flow-value | flow-value
projection = "{" name ("," name)* "}"
```

| Segment | Applied to | Selects |
| --- | --- | --- |
| `.name` | record | That field. |
| | map | The entry whose key is that string. |
| `[n]` | list, tuple | Element `n`; negative values count from the end. |
| | map | The entry whose key is the int `n`. |
| `[field=v]` | list of records | The unique element whose `field` equals `v`. Zero matches is *not found*; more than one is *ambiguous*; both are errors. |
| `[v]` | map | The entry with key `v`. |
| | set | The element `v`. |
| | list | The unique element equal to `v`. |
| | record (`v` is a string) | That field. |
| `[*]` | list, tuple, set, record, map | Every element or value. Queries only. |
| `.{a,b}` | record, or list of records | Only these fields (missing fields are omitted). Last segment only; queries only. |

The root path is written `.` or as the empty string. A path without `[*]` and without a projection is **singular**: it addresses at most one location.

### 7.2 Path errors

Resolving a path fails with one of these errors, each naming the path prefix at which it occurred:

- **not found**;
- **ambiguous** (a `[field=v]` or `[v]` selector matched more than one element);
- **type mismatch** (for example, `.name` applied to a list);
- **not singular** (`[*]` or a projection used where a single location is required).

### 7.3 Queries

A **query document** is a single line, `@get <path>` (`@select` is a synonym). Its **response** is a text document:

```
@revision sha256:91cf16db…        # revision of the whole queried document
@scope services[id=api]           # only when the path, without a final projection, is singular
replicas: 5
port: 3000
```

The body is the selected value: with `[*]` it is a list of the results, and with a projection only the named fields are kept. Responses SHOULD use the compact profile, so a projected list of uniform records becomes a table:

```
@get services[*].{id,replicas}
→
@revision sha256:91cf16db…
[2]{id,replicas}:
  auth,3
  api,5
```

Responses MUST be valid NOVA documents. Hosts SHOULD offer queries so that agents never need to load whole documents into context.

### 7.4 Patch documents

```
patch     = "@patch" [SP schema-ref] [SP base] NL
            ("@base" SP revision NL)?
            ("@scope" SP path NL)?
            op*
op        = path ":" SP old SP "->" SP new NL    ; replace, only if current = old
          | "=" SP path ":" SP value NL           ; set
          | "+" SP path ":" SP value NL           ; add
          | "-" SP path [":" SP old] NL           ; remove (optionally only if current = old)
          | "?" SP path [":" SP value] NL         ; test
```

- Values are flow values and may span lines inside brackets.
- Paths in operations MUST be singular.
- A `@patch` argument beginning with `sha256:` is the base revision; any other argument is the schema reference.
- `#` comments and blank lines are allowed between operations.
- Directives MUST precede operations.

Example:

```
@patch infrastructure/v1 sha256:91cf16db2f4a
@scope services[id=api]
replicas: 5 -> 8
+ tags: canary
? enabled: true
```

### 7.5 Patch semantics

Applying a patch `P` to a document `D`:

1. **Base check.** If `P` has a base revision, `revision(D)` MUST match it (prefix match per §2.13). Otherwise the patch fails with a *stale base* conflict.
2. **Scope.** If `P` has a scope `S`, then `S` MUST be singular and MUST resolve in `D`. Every operation path is interpreted as `S` followed by that path, so operations **cannot address anything outside the scope**. A host MAY additionally bind a scope out of band, for example per tool call; the patch's scope MUST then lie within the bound scope.
3. **Operations** are applied in order to a working copy. Each operation sees the results of the operations before it.
4. **Validation.** If the host has a schema for `D`, the result MUST validate (§6.4).
5. **Atomicity.** If any step fails, the patch is rejected as a whole and `D` is unchanged. The error MUST identify the failing operation (its index and line) and the reason.
6. On success, the host returns the new document and its revision.

Operation semantics, for target path `T` with parent `P` and last segment `s`:

| Operation | Semantics |
| --- | --- |
| `T: old -> new` | `T` MUST exist and equal `old` (§2.12); it becomes `new`. Otherwise a *conflict* reporting the expected and actual values. |
| `= T: v` | If `T` is the root, the document becomes `v`. If `P` is a record or map, field or key `s` is inserted or replaced. If `s` is an index or selector into a list or tuple, the existing element is replaced; `=` never appends. |
| `+ T: v` | If `T` exists and is a list, `v` is appended. If `T` exists and is a set, `v` is inserted (an error if already present). If `T` exists otherwise, it is an error (use `=`). If `T` does not exist: when `P` is a record, field `s` is added; when `P` is a list and `s` is `[n]`, `v` is inserted before index `n` (`n` = length appends); when `P` is a map, the entry is added. |
| `- T` | Removes the field, list element, set element or map entry. It is an error if `T` does not exist or is a tuple element. With `: old`, `T` MUST equal `old` first. |
| `? T` | `T` MUST exist. With `: v`, it MUST also equal `v`. Changes nothing. |

> *Rationale (non-normative).* Replace-with-expected-value (`old -> new`) is the default spelling because it makes every edit a compare-and-swap. An LLM that misremembers a value produces a conflict instead of silently overwriting it, and a reviewer sees both the before and the after.

### 7.6 Diffs

Tools MAY generate patches by diffing two values. The reference implementation:

- recurses into records;
- matches list elements by a unique `id`, `key` or `name` field when present and when the change consists only of removals and appends;
- otherwise diffs equal-length lists by index;
- otherwise replaces the list.

Every change is emitted as conditional (`old -> new` or `- path: old`), and the patch carries the base revision.

---

## 8. The agent interaction model

### 8.1 Read a projection, write a conditional patch

NOVA's intended interaction for agents:

```
AGENT     @get services[id=api].{replicas,port}

TOOL      @revision sha256:91cf16db…
          @scope services[id=api]
          replicas: 5
          port: 3000

AGENT     @patch sha256:91cf16db
          @scope services[id=api]
          replicas: 5 -> 8

TOOL      ok sha256:0f1267cd…            (or: conflict / invalid, with reasons)
```

Properties:

- **Small context.** The agent sees only the fields it asked for.
- **Small output.** The agent emits a single short line rather than regenerating the document.
- **Bounded blast radius.** The scope makes edits outside `services[id=api]` impossible, and `@key` (§6.2) makes the selector unambiguous.
- **Concurrency safety.** The base revision and the expected old value detect concurrent changes.
- **Reviewability.** The patch is a precise, human-readable change.

Hosts SHOULD expose `get` and `patch` as tools. Where constrained decoding is available, the schema (§6) and the patch grammar (§7.4) SHOULD be used to constrain generation. Semantic validation (§7.5 step 4) is still required afterwards: a grammar can ensure that `replicas` is a `u16`, but not that 8 is a sensible number of replicas.

### 8.2 Result envelopes (provenance)

A tool result whose domain data must be kept separate from facts *about* that data uses an envelope document:

```
@envelope result
value:
  status: completed
meta:
  source: deployment-api
  observed_at: @timestamp("2026-11-17T16:00:00Z")
  verified: true
```

The body MUST be a record with a required field `value` (the domain value) and an optional field `meta`, a record. These `meta` fields have defined meanings when present:

| Field | Type | Meaning |
| --- | --- | --- |
| `source` | string | Where the value came from. |
| `observed_at` | timestamp | When it was observed. |
| `verified` | bool | Whether the tool verified it, rather than inferring it. |
| `revision` | string | Revision of the source document. |
| `schema` | string | Schema reference for `value`. |
| `confidence` | float in [0, 1] | The *producer's* confidence in its interpretation, never a property of the domain object. |

Consumers MUST NOT merge `meta` into `value`. An envelope cannot establish truth; it lets verifiable provenance travel without being confused with domain data.

---

## 9. Templates and composition

### 9.1 Motivation

YAML's anchors, aliases and merge keys let authors reuse structure, and that reuse is valuable. Its problems come from where it lives:

- reuse is mixed into the data layer, so every consumer has to understand it;
- aliases make the value a graph (and enable "billion laughs" expansion);
- tools such as Helm then template YAML as **text**, which means generating indentation-sensitive syntax with string functions (`nindent`, `toYaml`, `quote`).

NOVA separates the two concerns. A **template** is a different kind of document. It is **rendered** to a plain value before anything else sees it: schemas, hashing, binary encoding, patches and queries operate only on rendered values. Templates operate on **values, not text**, so indentation and escaping cannot go wrong. The template language is **total**: every render terminates, and its output is bounded.

### 9.2 Template documents

A template is a text document whose **first** directive is `@template`. It may then declare:

```
@template
@param env: string                       # required parameter, typed in schema syntax (§6.1)
@param replicas: u16 @range(1..50) = 2   # with a default
@param owner?: string                    # optional: binds null when not given
@import "base.nova" as base              # another document, rendered and bound to $base
@let registry = "ghcr.io/example"        # a reusable value (flow form)
@let defaults:                           # a reusable value (block form)
  enabled: true
  replicas: $replicas
```

The body is ordinary NOVA text plus the constructs below. In template documents only:

- a token beginning with `$` is a reference;
- `${` inside a quoted or block string begins an interpolation;
- the control lines `@if`, `@else` and `@for` are recognized.

In data documents, `$` MUST NOT begin a token (it is a reserved character, §4.4), so a data document can never be misread as a template.

### 9.3 Constructs

**References.** `$name` followed by an optional path (§7.1), such as `$base.registry` or `$svc.ports[0]`, is replaced by the referenced value. Undefined names and failed paths are errors.

**Interpolation.** In quoted and block strings, `${path}` inserts the canonical text (§4.10) of a scalar: a string, number, bool, timestamp, duration, or unit variant (its tag). Interpolating `null` or a container is an error. `$${` and `\$` produce a literal `$`.

**Spreads.** `...$x` inserts the fields of a record into a record, or the elements of a list, tuple or set into a list:

```
- ...$defaults          # a list item that is a record starting with the fields of $defaults
  id: auth              # later entries win over earlier ones
```

Within one record, entries are applied in order and later entries replace earlier ones, **except** that two explicitly written fields with the same key are an error. Spreads are shallow. Deep edits are done with overlays (§9.6).

**Conditionals.** In block records and block lists:

```
@if $metrics and $env == production:
  - id: metrics
@else if $env == staging:
  - id: metrics-lite
@else:
  - id: noop
```

The bodies are indented two spaces and contribute entries or items to the enclosing record or list.

Conditions combine `==`, `!=`, `not`, `and`, `or` and parentheses over flow values and references. A condition that is a single operand MUST evaluate to a `bool`: there is no truthiness, so `$owner` alone is an error — write `$owner != null`.

**Loops.** `@for x in $xs:` or `@for i, x in $xs:` repeats its body once per element of a list, tuple or set (with `i` bound to the index), or per entry of a record or map (with `i` bound to the key). Loop variables are scoped to the body. Records produced by loop bodies merge into the enclosing record, and duplicate keys are an error. Keys may be interpolated: `"app-${name}": $name`.

### 9.4 Evaluation

1. Parameters are bound from the caller's arguments. Unknown arguments are errors. A missing parameter takes its default if it has one, `null` if it is optional, and is otherwise an error. Each value is then **resolved** against its declared type (§6.3), so `--set when=2026-11-17T16:00:00Z` becomes a timestamp when the parameter is typed `timestamp`. Named types come from a schema supplied by the host.
2. Imports are rendered (without arguments) and bound.
3. `@let` bindings are evaluated in order; each may use parameters, imports and earlier bindings. Rebinding a name is an error.
4. The body is evaluated.

The result is a plain value. Hosts SHOULD validate it against the target schema.

### 9.5 Totality and limits

The language has no user-defined functions, no recursion, no arithmetic, no I/O other than host-mediated imports, and iteration only over finite values. Every render therefore terminates. Because `@let` values can be referenced repeatedly, output can still grow exponentially, so implementations MUST enforce an **output budget**: a limit on the number of value nodes produced, counting every copy made by a reference. The reference default is 10^6 nodes, which defeats "billion laughs" constructions. Implementations MUST also bound the import depth (reference: 16) and MUST reject import cycles.

**Imports** are resolved by the host. A host MUST NOT fetch from the network unless the user has configured it to. The reference file loader accepts only relative paths beneath the root template's directory and rejects absolute paths, URLs and `..` escapes.

### 9.6 Overlays

Kustomize-style overlays need no new mechanism: an overlay is a **patch** (§7.4) applied to a rendered base. Overlays get the same conditional operations, atomicity and validation as agent edits:

```
nova render base.tpl.nova --set env=prod > base.nova
nova patch base.nova prod-overlay.patch --schema infra.novs
```

### 9.7 Why this is not "YAML as a programming language" (non-normative)

- **Opt-in and visible.** Only documents that start with `@template` can contain template constructs. Data documents reject them.
- **One-way.** Templates render to values. Nothing downstream evaluates anything.
- **Typed at the boundary.** Parameters are declared and resolved against schema types before evaluation.
- **Total.** Templates have no recursion or user functions, have a bounded output, and terminate by construction.
- **Value-level.** There is no string pasting of syntax, so the Helm class of indentation and quoting bugs cannot occur.

---

## 10. Token-aware representation selection

*This section is non-normative.*

There is no universal relationship between characters, tokens and LLM task performance. Tokenizers differ, and fewer input tokens do not always mean fewer total tokens or more successful tasks. Tables shine on uniform record lists and lose their advantage on irregular, deeply nested data.

A **token-aware serializer** chooses among value-preserving representations `E` to minimize:

  T_input(E) + 𝔼[T_output(E)] + λ · P_failure(E)

where `λ` prices task failures (including retries and repair loops). Only representations that preserve the semantics the task needs are eligible. `T_input` can be measured exactly with the target model's tokenizer. `T_output` and `P_failure` require empirical evaluation per model and task.

The reference implementation provides a `TokenCounter` trait, a deterministic `HeuristicCounter` that approximates subword tokenizers, and `rank_profiles`, which renders the human and compact profiles and ranks them by count. Production systems should plug in the real tokenizer and re-rank with measured `T_output` and `P_failure`.

Recommended evaluation workloads:

1. reading heterogeneous nested data;
2. reading large homogeneous record collections;
3. generating complete structured objects;
4. editing a small part of an existing document;
5. following references across a collection of documents.

Measure parser throughput, allocations, bytes, tokenizer-specific token counts, round-trip fidelity, schema adherence, task accuracy, and — for agents — **total tokens and wall-clock time to a correct, validated result**, including retries.

---

## 11. Interoperability mappings

### 11.1 JSON and JSON Lines

**JSON → NOVA** (lossless)

- object → record (duplicate keys are an error)
- array → list
- string → string
- `true`/`false`/`null` → bool/null
- a number with neither fraction nor exponent → `int` (exact, never via binary64)
- any other number → `float`

JSON Lines maps to a list of the values on its non-empty lines.

**NOVA → JSON** (lossless for the JSON subset). Other kinds map as follows, and **every such mapping MUST be reported** to the caller, who MAY refuse it ("strict" conversion):

| NOVA | JSON |
| --- | --- |
| int beyond ±2^53 | number (with a warning about precision in common readers) |
| non-finite float | string |
| decimal, rational, timestamp, duration | canonical text as a string |
| bytes | base64 string |
| tuple, set | array |
| map | array of `[key, value]` arrays |
| unit variant | string (the tag) |
| variant with payload | `{"tag": payload}` |

### 11.2 Other formats

Adapters for YAML, TOML, CBOR, MessagePack and Protobuf SHOULD follow the same discipline: lossless where the target can represent the kind, and an explicit report where it cannot. YAML input SHOULD be read with the YAML 1.2 core schema and aliases expanded under the limits of §9.5. YAML-specific tags and merge keys SHOULD be rejected, not guessed at.

### 11.3 Language bindings

Bindings map NOVA kinds to native types. They SHOULD follow these rules:

- **The native type acts as a schema.** When the target type is known — a timestamp, a decimal, an enum — a binding SHOULD accept the string spellings that §6.3 would resolve, and nothing else. So a schema-free document with `at: 2026-11-17T16:00:00Z` deserializes into a timestamp field.
- **Enums map to variants.** A binding SHOULD accept the variant itself, a string naming a unit variant, or a single-entry record `{tag: payload}`. The last form lets data that passed through JSON still deserialize.
- **No lossy narrowing.** A rational, decimal or out-of-range integer MUST NOT be silently converted to a float or a narrower integer. Bindings MAY offer explicit, opt-in adapters.
- **Errors carry paths.** Errors SHOULD report the path (§7.1) of the offending value, e.g. `services[1].port: invalid value: integer 70000, expected u16`.

**Rust / Serde (reference binding, `nova-serde`):**

| Serde data model | NOVA |
| --- | --- |
| bool, i8…i128, u8…u128, f32/f64, char, str, bytes | bool, int, float, string (char), string, bytes |
| `None`, unit, unit struct | null |
| `Some(x)`, newtype struct | `x` |
| seq | list |
| tuple, tuple struct | tuple |
| struct, map with string keys | record |
| map with other keys | map (when deserializing, numeric keys also accept their string form, as in JSON objects) |
| unit / newtype / tuple / struct variant | `!Tag`, `!Tag(x)`, `!Tag((a, b))`, `!Tag({…})` |
| `nova_value::{Timestamp, Duration, Decimal, Rational}` | the corresponding kind |
| `nova_value::Value` | itself, losslessly |
| `#[serde(with = "nova_serde::with::set")]` | set |
| `with::duration` (`std::time::Duration`), `with::system_time` (`SystemTime`) | duration, timestamp |

Serde has no data-model slots for NOVA's extra kinds. The binding therefore uses reserved newtype-struct names (`$nova::timestamp`, `$nova::decimal`, `$nova::set`, `$nova::value`, …). With other serde formats these degrade to natural forms: a timestamp becomes its canonical text, a set becomes a sequence, and a variant becomes a string or `{tag: payload}`. This means the same Rust types can also be serialized to JSON with the usual JSON conventions.

---

## 12. Security considerations

- **No implicit execution.** Data documents contain no code, references or includes. Templates (§9) are a separate document kind with total semantics and host-mediated imports.
- **Resource limits.** Parsers and decoders MUST bound nesting depth and SHOULD bound input size, the string-table size (§5.3) and template output (§9.5). Element counts in binary input are checked against the remaining length before any allocation.
- **Duplicate keys** are rejected everywhere, which prevents parser-differential attacks in which two components see different values.
- **Canonicalization** gives one revision per value, so signatures and hashes over the canonical encoding are not malleable by reordering or re-spelling.
- **Schemas are never fetched** automatically from `@schema` references.
- **Patch scope** limits what an automated agent can change. Hosts SHOULD bind scopes per tool call for untrusted agents.
- **Unicode.** NOVA does not normalize strings. Applications that compare identifiers from different sources SHOULD normalize them (for example to NFC) themselves, and SHOULD be aware of confusable characters in keys.

---

## 13. Conformance

| Class | Requirements |
| --- | --- |
| **Value implementation** | §2 and §5.1–5.3: model, equality, canonical encoding, revisions. |
| **Text reader** | §4 parsing, including every error condition marked MUST. |
| **Text writer** | §4.10: output re-parses to an equal value; canonical spellings for the canonical profile. |
| **Binary implementation** | §5. |
| **Schema implementation** | §6.1–6.4 (resolution performs exactly the listed conversions). §6.5–6.6 are RECOMMENDED. |
| **Patch host** | §7.1–7.5, including atomicity and conflict reporting. |
| **Template renderer** | §9, including the output budget and import restrictions. |
| **Editor** | Document-preserving fidelity (§3) for regions it does not intend to change. |

---

## 14. Reference implementation

The Rust workspace in this repository implements the specification.

| Crate | Implements |
| --- | --- |
| `nova-value` | §2, §5.1–5.3: value model, decimals, rationals, time types, canonical encoding, decoding, revisions |
| `nova-binary` | §5.4–5.5: frames, CRC-32, recovering stream reader |
| `nova-text` | §4, §7.1: parser, writer profiles, paths, template syntax tree, token heuristics (§10) |
| `nova-schema` | §6: schema parser, resolution and validation, compatibility checks, fingerprints |
| `nova-patch` | §7: queries and projections, the patch language, atomic application, diffs |
| `nova-template` | §9: rendering, sandboxed file loader, limits |
| `nova-json` | §11.1: JSON and JSON Lines mapping with loss reporting |
| `nova-serde` | §11.3: Serde serializer/deserializer, text and binary helpers, `with::` adapters |
| `nova-cli` | The `nova` command: `fmt`, `convert`, `hash`, `validate`, `get`, `patch`, `diff`, `render`, `stats`, `schema {fmt,fingerprint,check}` |

**Known gaps in version 0.1:**

- **Fidelity.** The text parser is value-preserving only: comments are discarded, so `nova fmt` drops them. A lossless concrete syntax tree, needed for document-preserving editors, is future work.
- **Envelopes.** `@envelope` (§8.2) is accepted as a directive, but its body structure is not yet checked.
- **Formats.** There are no YAML, TOML, CBOR or Protobuf adapters yet. The positional profile (Appendix C) is not implemented.
- **Schema checks.** `@len` on strings counts Unicode scalar values. `@range` comparisons between mixed numeric kinds use binary64.

---

## Appendix A — Grammar summary

```
document    = [BOM] (blank | directive)* body blank*
directive   = "@" name [SP rest-of-line] NL
body        = ε | block-record(0) | block-list(0) | top-table | flow-value NL

block-record(n) = (indent(n) entry)+
entry       = key ":" (SP inline-value | NL block(n+2) | ws-comment NL block(n+2))
            | key table-head ":" NL table-rows(n+2)
            | template-line(n)                          ; templates only
block-list(n)   = (indent(n) "-" (SP item-value | NL block(n+2)))+
item-value  = key ":" … (first entry of a record at n+2) | inline-value
inline-value = flow-value | plain-scalar | block-string | variant-block
block-string = "|" ["-"] NL content-lines(n+2)
table-head  = "[" digit+ "]" "{" key ("," key)* "}"
table-rows(n) = (indent(n) [cell] ("," [cell])* NL){N}
cell        = flow-value

flow-value  = quoted | word | "[" items "]" | "{" fields "}" | "(" tuple ")"
            | "@set[" items "]" | "@map{" (flow-value "=>" flow-value),* "}"
            | "@" name "(" quoted ")" | "!" name ["(" flow-value ")"]
            | "$" path                                  ; templates only
key         = identifier | quoted
identifier  = ('A'..'Z' | 'a'..'z' | "_") ('A'..'Z' | 'a'..'z' | '0'..'9' | "_" | "-")*

template-line(n) = "@if" SP cond ":" NL body(n+2) [indent(n) "@else" (":" | SP "if" …) NL body(n+2)]
                 | "@for" SP [identifier ","] identifier SP "in" SP flow-value ":" NL body(n+2)
                 | "..." flow-value NL
cond        = or ; or = and ("or" and)* ; and = unary ("and" unary)*
unary       = "not" unary | "(" cond ")" | flow-value [("==" | "!=") flow-value]
```

---

## Appendix B — Comparison with existing formats

*Non-normative.* What NOVA takes from each format, and what it avoids:

| Format | Adopted | Avoided |
| --- | --- | --- |
| JSON / JSONL | Strict number grammar, escapes, record streams | Limited types, no comments, silently accepted duplicate keys |
| JSON5 | Comments, trailing commas, unquoted keys | — |
| YAML | Block layout, comments, reuse (moved into §9) | Implicit typing, flexible indentation, aliases in data, tag semantics |
| TOML | Explicit types, predictability | Awkward deep nesting and arrays of tables |
| RON | Tuples, enums and variants, typed values | Rust-specific conventions |
| Protobuf | Varints, zigzag, numeric field identities (`@id`), evolution rules | Schema-dependent wire format |
| MessagePack / CBOR | Self-describing binary, tagged types | Tag semantics that need out-of-band agreement |
| TOON | Tables with declared columns and row counts | Applying tables where data is irregular |
| JSON Patch | Patch operations, `test` | Opaque JSON Pointer paths, no keyed selectors, no base revision |

---

## Appendix C — Open questions and future work

1. **Positional profile (experimental).** With a known schema fingerprint, a record can be written as its field values in declared order, omitting names (`@mode positional`). This is opt-in transport only: it is not self-describing, and its exact tokenization and escaping rules are still open.
2. **References.** Explicit references to independently identified records (`@ref(services, api)`) across documents, without general cyclic graphs.
3. **Lossless syntax tree** for document-preserving editing, formatting and schema migration.
4. **Adapters** for YAML, TOML, CBOR and Protobuf, and bindings beyond Serde (e.g. the reflection-based `facet`).
5. **Constrained decoding.** Generate tokenizer-level grammars from schemas and the patch grammar.
6. **Empirical evaluation** of the §10 objective across models, using the five workloads.
7. **Scale-preserving decimals.** Should `1.50` and `1.5` be distinguishable? Version 0.1 says no; schemas can constrain scale.
8. **Rational shorthand.** Is a bare `11/17` being a rational the right trade-off, or should rationals require `@rational(...)`? Version 0.1 treats bare `n/d` as a number, consistent with integers.
