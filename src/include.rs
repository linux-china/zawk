//! `@include "file"` and `--include file`: other awk source files spliced into the program
//! before it is parsed, as in gawk.
//!
//! A relative file name is looked up in the directories of `AWKPATH` (`.:/usr/local/share/awk`
//! by default), trying `name` then `name.awk`; a file name with a root (`/lib.awk`, or
//! `C:\lib.awk` on Windows) is used as is. Each file is loaded once: including it again (or
//! including a `-f` file) does nothing, which also stops include cycles.
//!
//! The program is split into pieces at the `@include` lines, so that each piece remembers the
//! line of its file it starts at, and errors are reported as `file:line:column` of that file.
//! A line inside a multi-line string, raw string or regex literal is part of that literal, not
//! a directive.
//!
//! As in gawk, each file must hold complete rules and functions: a file ending inside a `{`, a
//! `(` or a literal is an error at the end of that file, rather than swallowing the text that
//! follows it. The same goes for the text before an `@include` line.

use crate::common;
use crate::lexer::{Tok, Tokenizer};
use crate::lint::Source;
use std::path::{Component, Path, PathBuf};

/// Expands the includes of the program sources into a list of pieces, in program order.
#[derive(Default)]
pub struct Includer {
    /// The canonical paths of the files loaded so far.
    seen: hashbrown::HashSet<PathBuf>,
    pub pieces: Vec<Source>,
}

impl Includer {
    /// Adds a source named `name` (`None` for a program given on the command line), expanding its
    /// `@include` lines. `path` is the local file it was read from, if any, so that including it
    /// again does nothing.
    pub fn add_source(&mut self, name: Option<String>, text: &str, path: Option<&Path>) -> Result<(), String> {
        self.add(name, text, path, false)
    }

    /// `add_source`, `included` telling whether the source is an included file, which must end
    /// outside of any rule, function or literal.
    fn add(&mut self, name: Option<String>, text: &str, path: Option<&Path>, included: bool) -> Result<(), String> {
        if let Some(path) = path {
            self.seen.insert(canonical(path));
        }
        let lines: Vec<&str> = text.split('\n').collect();
        let mut start = 0;
        let mut expanded = false;
        // The text with the directives found so far blanked out, for the lexer to tell whether a
        // later line starts inside a literal.
        let mut masked = String::new();
        let mut offset = 0;
        for (i, line) in lines.iter().enumerate() {
            let line_start = offset;
            offset += line.len() + 1;
            let Some(file) = include_directive(line) else {
                continue;
            };
            if masked.is_empty() {
                masked = text.to_string();
            }
            if inside_token(&masked, line_start) {
                continue;
            }
            masked.replace_range(line_start..line_start + line.len(), &" ".repeat(line.len()));
            self.push(&name, &lines, start, i, expanded, Check::BeforeInclude)?;
            expanded = true;
            start = i + 1;
            // Errors in the included file have their own location.
            let loaded = self.load(file).map_err(|e| match &name {
                Some(name) => format!("{}:{}: {}", name, i + 1, e),
                None => format!("line {}: {}", i + 1, e),
            })?;
            if let Some((path, text)) = loaded {
                self.add(Some(path.to_string_lossy().into_owned()), &text, Some(&path), true)?;
            }
        }
        let check = if included { Check::EndOfFile } else { Check::None };
        self.push(&name, &lines, start, lines.len(), expanded, check)
    }

    /// Splices in the file `file`, unless it was loaded already (`--include file`).
    pub fn include(&mut self, file: &str) -> Result<(), String> {
        match self.load(file)? {
            Some((path, text)) => self.add(Some(path.to_string_lossy().into_owned()), &text, Some(&path), true),
            None => Ok(()),
        }
    }

    /// The path and text of the file `file`, or `None` if it was loaded already.
    fn load(&self, file: &str) -> Result<Option<(PathBuf, String)>, String> {
        if file.starts_with("http://") || file.starts_with("https://") || file.starts_with("gh:") {
            return Err(format!("can't include remote source file `{}': not supported", file));
        }
        let path = resolve(file)?;
        if self.seen.contains(&canonical(&path)) {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!("can't open source file `{}' for reading: {}", file, common::io_error_reason(&e))
        })?;
        Ok(Some((path, text)))
    }

    /// Adds the lines `start..end` of a source as a piece, checking that it is complete as `check`
    /// asks. Once the source had an include, empty pieces are left out: without includes, the source
    /// is kept exactly as it is.
    fn push(
        &mut self,
        name: &Option<String>,
        lines: &[&str],
        start: usize,
        end: usize,
        expanded: bool,
        check: Check,
    ) -> Result<(), String> {
        if expanded && start >= end {
            return Ok(());
        }
        let text = lines[start..end].join("\n");
        if check != Check::None {
            if let Some((mut line, mut col, mut desc)) = incomplete(&text, start) {
                if check == Check::BeforeInclude {
                    // The rule or function goes on after the included file.
                    desc = desc.replace("end of file", "`@include` inside a rule or function");
                    (line, col) = (end + 1, 1);
                }
                return Err(match name {
                    Some(name) => format!("syntax error at {}:{}:{}: {}", name, line, col, desc),
                    None => format!("syntax error at line {}, column {}: {}", line, col, desc),
                });
            }
        }
        self.pieces.push(Source {
            name: name.clone(),
            text,
            line: start,
        });
        Ok(())
    }
}

/// The file name of an `@include "file"` line: the directive alone on its line, optionally
/// followed by `;` and a comment.
fn include_directive(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("@include")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    let tail = rest[end + 1..].trim_start();
    let tail = tail.strip_prefix(';').unwrap_or(tail).trim_start();
    let tail = tail.trim_end_matches('\r');
    (tail.is_empty() || tail.starts_with('#')).then(|| &rest[..end])
}

/// Whether a piece must hold complete rules and functions: at the end of an included file, or
/// before an `@include` line.
#[derive(Clone, Copy, PartialEq)]
enum Check {
    None,
    EndOfFile,
    BeforeInclude,
}

/// Where (1-based line and column of its file, the text starting at line `first_line`) and why
/// `text` does not hold complete rules and functions: it ends inside a `{`, a `(` or a literal.
/// Unbalanced closing brackets and other errors are left to the parser, which reports them where
/// they are.
fn incomplete(text: &str, first_line: usize) -> Option<(usize, usize, String)> {
    // Columns count characters, as in `SourceMap::describe`.
    let pos = |line: usize, col: usize| {
        let text = text.split('\n').nth(line).unwrap_or("");
        (first_line + line + 1, text.get(..col).unwrap_or(text).chars().count() + 1)
    };
    let mut open = Vec::new();
    for tok in Tokenizer::new(text) {
        match tok {
            Ok((l, Tok::LBrace, _)) => open.push((l, '}')),
            // The lexer folds `(` into calls, `print(`, `printf(` and `exit(`.
            Ok((l, Tok::LParen | Tok::CallStart(_) | Tok::PrintLP | Tok::PrintfLP | Tok::ExitLP, _)) => {
                open.push((l, ')'))
            }
            Ok((_, tok @ (Tok::RBrace | Tok::RParen), _)) => {
                let close = if tok == Tok::RBrace { '}' } else { ')' };
                if open.pop().map(|(_, c)| c) != Some(close) {
                    return None;
                }
            }
            Ok(_) => {}
            Err(e) => {
                let (line, col) = pos(e.location.line, e.location.col);
                return Some((line, col, e.desc.into_owned()));
            }
        }
    }
    let (l, close) = open.pop()?;
    let (open_line, open_col) = pos(l.line, l.col);
    let body = text.trim_end_matches('\n');
    let last = body.rsplit('\n').next().unwrap_or("");
    let (line, col) = pos(body.matches('\n').count(), last.len());
    let desc = format!(
        "unexpected end of file: missing `{}` for the one at line {}, column {}",
        close, open_line, open_col
    );
    Some((line, col, desc))
}

/// Whether the byte `offset` of `text` is inside a token (a string, raw string or regex literal
/// spanning lines), as lexed from the start. A lexing error before `offset` counts as outside:
/// the program is then reported as wrong by the parser anyway.
fn inside_token(text: &str, offset: usize) -> bool {
    for tok in Tokenizer::new(text) {
        let Ok((start, _, end)) = tok else {
            return false;
        };
        if start.offset() >= offset {
            return false;
        }
        if end.offset() > offset {
            return true;
        }
    }
    false
}

/// Where `file` is: a file name with a root as is; a relative path with a directory separator
/// (`./lib.awk`, `lib/x.awk`) first from the current directory, then like a plain file name, the
/// first match in `AWKPATH` (as gawk does).
fn resolve(file: &str) -> Result<PathBuf, String> {
    let not_found = || format!("can't open source file `{}' for reading: No such file or directory", file);
    if file.is_empty() {
        return Err(not_found());
    }
    let path = Path::new(file);
    let rooted = path.has_root() || matches!(path.components().next(), Some(Component::Prefix(_)));
    let candidates = |p: &Path| {
        let mut v = vec![p.to_path_buf()];
        if !file.ends_with(".awk") {
            v.push(PathBuf::from(format!("{}.awk", p.display())));
        }
        v
    };
    if rooted {
        return candidates(path).into_iter().find(|p| p.is_file()).ok_or_else(not_found);
    }
    let has_separator = file.contains('/') || (cfg!(windows) && file.contains('\\'));
    let local = if has_separator { candidates(path) } else { Vec::new() };
    local
        .into_iter()
        .chain(search_path().into_iter().flat_map(|dir| candidates(&dir.join(file))))
        .find(|p| p.is_file())
        .ok_or_else(not_found)
}

/// The directories of `AWKPATH`, an empty entry being the current directory.
fn search_path() -> Vec<PathBuf> {
    let default = if cfg!(windows) { "." } else { ".:/usr/local/share/awk" };
    let awkpath = std::env::var_os("AWKPATH").filter(|p| !p.is_empty());
    let awkpath = awkpath.unwrap_or_else(|| default.into());
    std::env::split_paths(&awkpath)
        .map(|dir| if dir.as_os_str().is_empty() { PathBuf::from(".") } else { dir })
        .collect()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive() {
        assert_eq!(include_directive(r#"@include "lib.awk""#), Some("lib.awk"));
        assert_eq!(include_directive(r#"  @include	"a b.awk" ;  # lib"#), Some("a b.awk"));
        assert_eq!(include_directive("@include \"lib\"\r"), Some("lib"));
        assert_eq!(include_directive(r#"# @include "lib.awk""#), None);
        assert_eq!(include_directive(r#"@include "lib.awk" BEGIN {}"#), None);
        assert_eq!(include_directive(r#"@includes "lib.awk""#), None);
        assert_eq!(include_directive(r#"print "@include""#), None);
    }

    #[test]
    fn directive_inside_literal() {
        let raw = "BEGIN { s = r#\"a\n@include \"lib.awk\"\nb\"#; print s }";
        assert!(inside_token(raw, raw.find('@').unwrap()));
        assert!(!inside_token("BEGIN { x = 1 }\n@include \"lib.awk\"", 16));
        let mut inc = Includer::default();
        inc.add_source(None, raw, None).unwrap();
        assert_eq!(inc.pieces.len(), 1);
        assert_eq!(inc.pieces[0].text, raw);
    }

    #[test]
    fn incomplete_sources() {
        assert_eq!(incomplete("function f() { return 1 }\n", 0), None);
        assert_eq!(
            incomplete("BEGIN { x = 1\n", 2),
            Some((3, 14, "unexpected end of file: missing `}` for the one at line 3, column 7".into()))
        );
        assert_eq!(incomplete("BEGIN { s = \"a\n", 0).map(|(l, c, _)| (l, c)), Some((1, 14)));
        // Mismatched or extra closing brackets are reported by the parser, where they are.
        assert_eq!(incomplete("BEGIN { f(1 }", 0), None);
        assert_eq!(incomplete("BEGIN { } }", 0), None);
        // Parentheses folded into call, `print(`, `printf(` and `exit(` tokens are matched too.
        assert_eq!(incomplete("function f(x) { return toupper(x) }\n", 0), None);
        assert_eq!(incomplete("BEGIN { print(1); printf(\"%s\", 2); exit(0) }\n", 0), None);
        assert_eq!(
            incomplete("function f(x) {\n  return toupper(x)\n", 0),
            Some((2, 20, "unexpected end of file: missing `}` for the one at line 1, column 15".into()))
        );
        assert_eq!(incomplete("BEGIN { print(f(1)\n", 0).map(|(l, c, _)| (l, c)), Some((1, 19)));
    }

    #[test]
    fn pieces_keep_file_lines() {
        let dir = std::env::temp_dir().join(format!("zawk-include-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = dir.join("lib.awk");
        std::fs::write(&lib, "function f() { return 1 }\n").unwrap();
        let main = format!(
            "BEGIN {{ x = 1 }}\n@include \"{0}\"\n@include \"{0}\"\nBEGIN {{ print f() }}",
            lib.display()
        );
        let mut inc = Includer::default();
        inc.add_source(Some("main.awk".into()), &main, None).unwrap();
        let pieces: Vec<(Option<&str>, &str, usize)> = inc
            .pieces
            .iter()
            .map(|p| (p.name.as_deref(), p.text.as_str(), p.line))
            .collect();
        let lib_name = lib.to_string_lossy();
        assert_eq!(
            pieces,
            vec![
                (Some("main.awk"), "BEGIN { x = 1 }", 0),
                (Some(&*lib_name), "function f() { return 1 }\n", 0),
                (Some("main.awk"), "BEGIN { print f() }", 3),
            ]
        );
        let err = inc.add_source(None, "@include \"no-such-lib-zawk\"", None).unwrap_err();
        assert!(err.starts_with("line 1: can't open source file `no-such-lib-zawk'"), "{}", err);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
