//! Recursive-descent parser for the Hyprland-inspired `.conf` grammar.
//!
//! ```text
//! document   := statement*
//! statement  := blank | comment | category | assignment
//! category   := IDENT '{' statement* '}'
//! assignment := ('$' IDENT | KEY) '=' VALUE
//! ```
//!
//! `#` at the start of a line or after whitespace starts a comment, so
//! `class:^(#32770)$` needs no escaping; `##` is always a literal `#`. Variables (`$name`) are
//! expanded at assignment time. `source = PATH` includes another file in
//! place; `%ENV%` references and paths relative to the including file are
//! resolved. Category nesting flattens to `outer:inner:key`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One `key = value` assignment after variable expansion and flattening.
#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub value: String,
    pub file: PathBuf,
    pub line: usize,
}

impl Entry {
    pub fn location(&self) -> String {
        format!("{}:{}", self.file.display(), self.line)
    }
}

#[derive(Debug, Default)]
pub struct Document {
    pub entries: Vec<Entry>,
    pub errors: Vec<String>,
    pub files: Vec<PathBuf>,
    pub vars: HashMap<String, String>,
}

const MAX_SOURCE_DEPTH: usize = 16;

/// Parses `path` (and everything it sources). `vars` seeds the variable table.
pub fn parse_file(path: &Path, vars: HashMap<String, String>) -> Document {
    let mut doc = Document { vars, ..Default::default() };
    let mut stack = Vec::new();
    include(path, &mut doc, &mut stack);
    doc
}

fn include(path: &Path, doc: &mut Document, stack: &mut Vec<PathBuf>) {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&canon) {
        doc.errors.push(format!("{}: source cycle detected", path.display()));
        return;
    }
    if stack.len() >= MAX_SOURCE_DEPTH {
        doc.errors.push(format!("{}: source nesting too deep", path.display()));
        return;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            doc.errors.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    stack.push(canon);
    doc.files.push(path.to_path_buf());
    let mut parser = Parser {
        lines: text.lines().map(str::to_owned).collect(),
        pos: 0,
        file: path.to_path_buf(),
    };
    parser.block(&mut Vec::new(), doc, stack);
    stack.pop();
}

struct Parser {
    lines: Vec<String>,
    pos: usize,
    file: PathBuf,
}

impl Parser {
    fn err(&self, line: usize, msg: impl std::fmt::Display) -> String {
        format!("{}:{}: {msg}", self.file.display(), line)
    }

    /// Parses statements until EOF (top level) or a closing brace (nested).
    fn block(&mut self, prefix: &mut Vec<String>, doc: &mut Document, stack: &mut Vec<PathBuf>) {
        let nested = !prefix.is_empty();
        while self.pos < self.lines.len() {
            let line_no = self.pos + 1;
            let raw = strip_comment(&self.lines[self.pos]);
            self.pos += 1;
            let mut line = raw.trim().to_string();
            if line.is_empty() {
                continue;
            }

            if line == "}" {
                if nested {
                    return;
                }
                doc.errors.push(self.err(line_no, "unexpected '}'"));
                continue;
            }

            // `name {` possibly followed by a statement on the same line.
            if let Some((head, rest)) = split_category_open(&line) {
                prefix.push(head);
                let rest = rest.trim();
                if !rest.is_empty() {
                    // Re-queue the remainder (e.g. `blur { enabled = true }`).
                    self.pos -= 1;
                    self.lines[self.pos] = rest.to_string();
                }
                self.block(prefix, doc, stack);
                prefix.pop();
                continue;
            }

            // `key = value }` closes the block after the assignment.
            let mut closes = false;
            if nested && line.ends_with('}') && !line.contains('{') {
                line.pop();
                line = line.trim_end().to_string();
                closes = true;
            }

            self.statement(&line, line_no, prefix, doc, stack);
            if closes {
                return;
            }
        }
        if nested {
            doc.errors.push(self.err(self.lines.len(), format!("unclosed '{{' for '{}'", prefix.join(":"))));
        }
    }

    fn statement(&self, line: &str, line_no: usize, prefix: &[String], doc: &mut Document, stack: &mut Vec<PathBuf>) {
        let Some((lhs, rhs)) = line.split_once('=') else {
            doc.errors.push(self.err(line_no, format!("expected 'key = value', got '{line}'")));
            return;
        };
        let key = lhs.trim();
        let raw_value = rhs.trim();
        if key.is_empty() {
            doc.errors.push(self.err(line_no, "missing key before '='"));
            return;
        }

        let value = match expand_vars(raw_value, &doc.vars) {
            Ok(v) => v,
            Err(name) => {
                doc.errors.push(self.err(line_no, format!("undefined variable ${name}")));
                raw_value.to_string()
            }
        };

        if let Some(name) = key.strip_prefix('$') {
            if name.is_empty() || !name.chars().all(is_ident_char) {
                doc.errors.push(self.err(line_no, format!("invalid variable name '{key}'")));
            } else {
                doc.vars.insert(name.to_string(), value);
            }
            return;
        }

        if key == "source" && prefix.is_empty() {
            let expanded = expand_env(&value);
            let mut path = PathBuf::from(expanded.trim().trim_matches('"'));
            if path.is_relative() {
                if let Some(dir) = self.file.parent() {
                    path = dir.join(path);
                }
            }
            include(&path, doc, stack);
            return;
        }

        let full_key = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}:{}", prefix.join(":"), key)
        };
        doc.entries.push(Entry { key: full_key, value, file: self.file.clone(), line: line_no });
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Recognises `ident {` and returns (ident, rest-after-brace).
fn split_category_open(line: &str) -> Option<(String, String)> {
    let brace = line.find('{')?;
    let head = line[..brace].trim();
    if head.is_empty() || head.contains('=') || !head.chars().all(|c| is_ident_char(c) || c == '.' || c == '-') {
        return None;
    }
    Some((head.to_string(), line[brace + 1..].to_string()))
}

/// Removes a `# comment`. A `#` opens a comment at the start of the line or
/// after whitespace; elsewhere it is literal. `##` is an escaped literal `#`.
fn strip_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut prev_ws = true;
    while let Some(c) = chars.next() {
        if c == '#' {
            if prev_ws && out.trim().is_empty() {
                break; // whole-line comment, including `####` banners
            }
            if chars.peek() == Some(&'#') {
                chars.next();
                out.push('#');
                prev_ws = false;
                continue;
            }
            if prev_ws {
                break;
            }
        }
        prev_ws = c.is_whitespace();
        out.push(c);
    }
    out
}

/// Expands `$name` references. Returns the first undefined name on error.
pub fn expand_vars(value: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    if !value.contains('$') {
        return Ok(value.to_string());
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while let Some(&(j, n)) = chars.peek() {
            if is_ident_char(n) {
                end = j + n.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let name = &value[start..end];
        if name.is_empty() {
            out.push('$');
            continue;
        }
        // Longest defined prefix wins, so `$nord10` beats `$nord1`.
        let found = (1..=name.len()).rev().find_map(|n| vars.get(&name[..n]).map(|v| (n, v)));
        match found {
            Some((n, v)) => {
                out.push_str(v);
                out.push_str(&name[n..]);
            }
            None => return Err(name.to_string()),
        }
    }
    Ok(out)
}

/// Expands `%NAME%` environment references (Windows style).
pub fn expand_env(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(text: &str) -> Document {
        let dir = std::env::temp_dir().join(format!("deadbeef-parser-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{:x}.conf", text.len() * 31 + text.lines().count()));
        std::fs::write(&path, text).unwrap();
        let doc = parse_file(&path, HashMap::new());
        let _ = std::fs::remove_file(&path);
        doc
    }

    #[test]
    fn nested_categories_and_vars() {
        let doc = parse_str(
            "$t = wt\n$nord1 = a\n$nord10 = b\n\
             general {\n gaps_in = 5 # c\n col.x = rgba(00ff00ff)\n}\n\
             decoration {\n blur { enabled = true }\n rounding = 4\n}\n\
             bind = SUPER, X, exec, $t\nx = $nord10 $nord1\n\
             ########\n# banner\nwindowrule = float, class:^(#32770)$ # dialogs\ny = a##b\n",
        );
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
        let kv: Vec<_> = doc.entries.iter().map(|e| (e.key.as_str(), e.value.as_str())).collect();
        assert_eq!(
            kv,
            vec![
                ("general:gaps_in", "5"),
                ("general:col.x", "rgba(00ff00ff)"),
                ("decoration:blur:enabled", "true"),
                ("decoration:rounding", "4"),
                ("bind", "SUPER, X, exec, wt"),
                ("x", "b a"),
                ("windowrule", "float, class:^(#32770)$"),
                ("y", "a#b"),
            ]
        );
    }

    #[test]
    fn errors_are_reported() {
        let doc = parse_str("general {\n nonsense\n}\n}\nfoo = $missing\n");
        assert_eq!(doc.errors.len(), 3, "{:?}", doc.errors);
    }

    #[test]
    fn env_expansion() {
        std::env::set_var("DEADBEEF_TEST_VAR", "C:\\x");
        assert_eq!(expand_env("%DEADBEEF_TEST_VAR%\\a.conf"), "C:\\x\\a.conf");
        assert_eq!(expand_env("100%"), "100%");
    }
}
