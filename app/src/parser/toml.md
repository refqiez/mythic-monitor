# simplified-TOML

TOML inspired markup format that focuses on fast, easy parsing.

## Document Structure

- A simplified-TOML document is a **single table** at the root level.
- You can define **root-level keys** before any section.
- Sections are optional and start with a header in square brackets:

```toml
root-level-key = 42

[section_name]
key = "value"
```

- Keys defined before the first section belong to the root table.
- Keys after a section header belong to that section.
- Duplicate keys sections are allowed; they are stored in the order they appear.


## Keys, Section names

- Unquoted, simple identifiers.
- Must match `[a-zA-Z_][a-zA-Z0-9_-]*`.

```toml
username = "admin"
max_retries = 5
timeout-sec = 30.5
```


## Values

Simplified-TOML supports the following value types:

### Numbers

- All numeric literals are parsed as **floating-point numbers (f64)**.
- No numerci separators `_`.
- Must match `[-+]? [0-9]+("." [0-9]+)?`.

```toml
version = 1.0
port = 080
negative = -42
```

### Strings

- Strings are enclosed in **double quotes `"`**.
- **No escapes handling**.
- **Double quotes inside strings are forbidden**.
- You need to post process string with custom escape rule if you want special or double quote characters.

```toml
title = "Simplified TOML Parser"
```

### Booleans

- Only `true` or `false` (lowercase) are valid.

```toml
enabled = true
disabled = false
```

### Arrays

- Arrays are **single-line**, enclosed in square brackets `[]`.
- Elements can be **mixed types** (heterogeneous).
- Trailing commas are allowed.
- **Cannot nest another arrays or inline-tables inside.**

```toml
numbers = [1, 2.0, 3]
mixed = ["hello", true, 42]
# invalid = [ [] ]
# invalid = [ {} ]
```

### Inline Tables

- Inline tables are **single-line**, enclosed in braces `{}`.
- Trailing commas are allowed.
- Duplicate keys inside an inline table are stored in order.
- **Cannot nest another arrays or inline-tables inside.**

```toml
config = { retries=3, timeout=30.5, enabled=true }
empty_table = {}
# invalid = { arr = [] }
# invalid = { tbl = {} }
```


## Sections

- Glorified way of defining table with name.
- Section headers are **square-bracketed identifiers**:
  - Section names **cannot be empty**.
- **Dotted section names are not supported.**

```toml
[database]
user = "admin"
password = "secret"
```

## Comments

- Comments start with `#` and continue to the end of the line.
- Inline comments are allowed after a value:

```toml
# feature flag
enabled = true  # flag is on
```


## Rules & Limitations

- **No multi-line strings.**
- **No date/time values.**
- **No numeric separators** (underscores) or hex/octal/binary.
- **No nested arrays or nested inline tables.**
- **Arrays and inline tables must be on a single line.**
- **No nested table access via dotted keys.**
- **Allows duplicated keys.**


## Parser Output

- The parser returns a **Document**:

```rust
struct Document<'a> {
    root: Vec<Entry<'a>>,
    sections: Vec<(String, Vec<Entry<'a>>, Pos)>,
}
```

- Each **Entry** contains:

```rust
struct Entry<'a> {
    key: String,
    value: Value<'a>,
    pos: Pos, // line, column, offset
}
```

- **Value** enum:

```rust
enum Value<'a> {
    String(&'a str),
    Number(f64),
    Boolean(bool),
    Array(Vec<Value<'a>>),
    Table(Vec<Entry<'a>>),
}
```

- **Line/column tracking:**
  - `line` starts at 1 and counts physical lines.
  - `column` starts at 1.
  - `offset` is the byte offset in the original text.

---

## 8. Example

```toml
title = "Toy TOML parser"
version = 1.0
enabled = true
empty_array = []

[database]
user = "admin"
password = "secret"
ports = [8000, 8001, 8002]
config = { retries=3, timeout=30.5 }

[empty_section]
```

- **Root table:** `title`, `version`, `enabled`, `empty_array`
- **Sections:** `database` with keys `user`, `password`, `ports`, `config`
- **Empty section:** `empty_section`

---

This parser is ideal for **toy projects**, configuration files, or learning purposes where **full TOML compliance is not required**.
