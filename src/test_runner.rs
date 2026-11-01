//! `zawk test`: runs the functions named `test_*` of AWK files, as `cargo test` runs `#[test]`
//! functions.
//!
//! Each test is a process of its own (`zawk --test-entry NAME -f FILE`): a failed assertion ends
//! the program at once, and the globals of a test must not leak into the next one. The rules of
//! the file do not run. The tests of a file are the functions written in it, not those of the
//! files it includes, which have their own run.

use crate::include::Includer;
use crate::lexer::{Tok, Tokenizer};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TEST_PREFIX: &str = "test_";

/// One line of the report.
enum Case {
    /// The test function `name` of `file`.
    Function { file: PathBuf, name: String },
    /// `file` could not be read or tokenized, which is reported as a failure.
    Broken { file: PathBuf, error: String },
}

impl Case {
    fn label(&self) -> String {
        match self {
            Case::Function { file, name } => format!("{}::{}", file.display(), name),
            Case::Broken { file, .. } => file.display().to_string(),
        }
    }

    /// Runs the case: `Err` holds the output to show for a failure.
    fn run(&self, exe: &Path) -> Result<(), String> {
        let (file, name) = match self {
            Case::Function { file, name } => (file, name),
            Case::Broken { error, .. } => return Err(error.clone()),
        };
        let output = Command::new(exe)
            .arg("--test-entry")
            .arg(name)
            .arg("-f")
            .arg(file)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("failed to run {}: {}", exe.display(), e))?;
        if output.status.success() {
            return Ok(());
        }
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if text.is_empty() {
            text = output.status.to_string();
        }
        Err(text)
    }
}

/// Runs the tests of `paths` (the current directory if there are none), keeping those whose
/// function name contains `filter`, and prints a report on stdout. Returns whether they all passed.
///
/// # Errors
///
/// When a path cannot be read: no test is run.
pub fn run(paths: &[&String], filter: Option<&str>) -> Result<bool, String> {
    let mut files = Vec::new();
    if paths.is_empty() {
        collect_awk_files(Path::new("."), &mut files)?;
    }
    for path in paths {
        let path = Path::new(path.as_str());
        if path.is_dir() {
            collect_awk_files(path, &mut files)?;
        } else if path.is_file() {
            files.push(path.to_path_buf());
        } else {
            return Err(format!("no such file or directory: {}", path.display()));
        }
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find zawk itself: {e}"))?;

    let mut cases = Vec::new();
    let mut filtered_out = 0;
    for file in files {
        match test_functions(&file) {
            Ok(names) => {
                for name in names {
                    if filter.is_none_or(|text| name.contains(text)) {
                        cases.push(Case::Function {
                            file: file.clone(),
                            name,
                        });
                    } else {
                        filtered_out += 1;
                    }
                }
            }
            Err(error) => cases.push(Case::Broken { file, error }),
        }
    }

    // A closed stdout must not hide the exit code, so the report ignores write errors.
    let mut out = io::stdout().lock();
    let plural = if cases.len() == 1 { "" } else { "s" };
    let _ = writeln!(out, "running {} test{}", cases.len(), plural);
    let mut failures = Vec::new();
    for case in &cases {
        let label = case.label();
        let _ = write!(out, "test {label} ... ");
        let _ = out.flush();
        match case.run(&exe) {
            Ok(()) => {
                let _ = writeln!(out, "ok");
            }
            Err(output) => {
                let _ = writeln!(out, "FAILED");
                failures.push((label, output));
            }
        }
    }
    if !failures.is_empty() {
        let _ = writeln!(out, "\nfailures:");
        for (label, output) in &failures {
            let newline = if output.ends_with('\n') { "" } else { "\n" };
            let _ = write!(out, "\n---- {label} ----\n{output}{newline}");
        }
        let _ = writeln!(out, "\nfailures:");
        for (label, _) in &failures {
            let _ = writeln!(out, "    {label}");
        }
    }
    let result = if failures.is_empty() { "ok" } else { "FAILED" };
    let _ = writeln!(
        out,
        "\ntest result: {}. {} passed; {} failed; {} filtered out",
        result,
        cases.len() - failures.len(),
        failures.len(),
        filtered_out
    );
    Ok(failures.is_empty())
}

/// Adds the `.awk` files under `dir` to `files`, in name order, skipping hidden entries. Links to
/// directories are not followed, as they may form a cycle.
fn collect_awk_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let read_error = |e: io::Error| format!("cannot read {}: {}", dir.display(), e);
    let mut entries = fs::read_dir(dir)
        .map_err(read_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(read_error)?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if entry.file_type().map_err(read_error)?.is_dir() {
            collect_awk_files(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "awk") && path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// The names of the test functions written in `file`, found by the lexer in the pieces of the
/// file between its `@include` lines. An error in the program is reported by each test.
fn test_functions(file: &Path) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(file).map_err(|e| format!("cannot read file: {e}\n"))?;
    let name = file.display().to_string();
    let mut includer = Includer::default();
    includer
        .add_source(Some(name.clone()), &text, Some(file))
        .map_err(|e| format!("{e}\n"))?;
    let mut names = Vec::new();
    for piece in includer
        .pieces
        .iter()
        .filter(|p| p.name.as_ref() == Some(&name))
    {
        for token in Tokenizer::new(&piece.text) {
            match token {
                Ok((_, Tok::FunDec(name), _)) if name.starts_with(TEST_PREFIX) => {
                    names.push(name.to_string());
                }
                Ok(_) => {}
                Err(e) => return Err(format!("syntax error: {}\n", e.desc)),
            }
        }
    }
    Ok(names)
}
