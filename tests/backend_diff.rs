//! Differential tests: the same program must give the same stdout and exit code with every
//! backend and optimization level. The programs are the compat cases (including the known
//! differences from gawk, which must still agree between backends) and programs generated from
//! combinations of operations and operands, where no expected output is needed.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The configurations compared with the first one.
const CONFIGS: &[&[&str]] = &[
    &["-Binterp"],
    &["-Bcranelift", "-O0"],
    &["-Bcranelift", "-O3"],
];

#[derive(PartialEq, Eq, Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
}

fn run(dir: &Path, config: &[&str], args: &[String], stdin: &[u8]) -> Outcome {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zawk"))
        .current_dir(dir)
        .env("LC_ALL", "C.UTF-8")
        .args(config)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // A program may exit before reading its input.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    let output = child.wait_with_output().unwrap();
    Outcome {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    }
}

/// Runs `args` with every configuration, returning a description of the differences, if any.
fn differences(name: &str, dir: &Path, args: &[String], stdin: &[u8]) -> Option<String> {
    let outcomes: Vec<Outcome> = CONFIGS.iter().map(|c| run(dir, c, args, stdin)).collect();
    if outcomes.iter().all(|o| *o == outcomes[0]) {
        return None;
    }
    let details: Vec<String> = CONFIGS
        .iter()
        .zip(&outcomes)
        .map(|(c, o)| format!("--- {} (exit {:?})\n{}", c.join(" "), o.code, o.stdout))
        .collect();
    Some(format!("DIFF {}\n{}", name, details.join("")))
}

fn directive<'a>(program: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("# {}: ", name);
    program
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
}

#[test]
fn compat_cases_agree_between_backends() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat");
    let mut cases: Vec<PathBuf> = fs::read_dir(dir.join("cases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "awk"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    let errors: Vec<String> = cases
        .iter()
        .filter_map(|awk_file| {
            let program = fs::read_to_string(awk_file).unwrap();
            let split = |name| {
                directive(&program, name)
                    .map_or(vec![], |s| s.split_whitespace().map(String::from).collect())
            };
            let stdin = directive(&program, "stdin")
                .map_or(vec![], |file| fs::read(dir.join(file)).unwrap());
            let mut args = split("opts");
            args.push("-f".into());
            args.push(awk_file.to_string_lossy().into_owned());
            args.extend(split("args"));
            let name = awk_file.file_stem().unwrap().to_string_lossy();
            differences(&name, &dir, &args, &stdin)
        })
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// Operations on the parameter `s` of a user-defined function; `s` must not change in the caller.
const PARAM_OPS: &[&str] = &[
    r#"sub(/[0-9a]/, "X", s)"#,
    r#"gsub(/./, "<&>", s)"#,
    r#"n = gsub(/z/, "", s); s = n s"#,
    r#"s = gensub(/(.)/, "\\1\\1", "g", s)"#,
    r#"getline s < "data/a.txt"; close("data/a.txt")"#,
    r#"n = split(s, parts, ""); s = n "/" parts[1]"#,
    r#"s ??= "dflt""#,
    "s++",
    "s += 0.5",
    r#"s = s "x""#,
    "s = substr(s, 2)",
    "s = length(s)",
    r##"if (s ~ /^[0-9]+$/) sub(/^/, "#", s); else s = "-" s"##,
    r#"sub(/./, "1", s); s = s * 2"#,
];

/// Operands passed as the argument: `v` is set in BEGIN, `$1`/`$0` come from the input.
const OPERANDS: &[&str] = &[
    "123", "2.5", "0", "-0", "\"aa\"", "\"12\"", "\"\"", "v", "$1", "$0", "$1 + 0", "u", "a[1]",
];

#[test]
fn generated_param_programs_agree_between_backends() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat");
    let stdin = b"12 apple\naa\n";
    let mut errors = vec![];
    for op in PARAM_OPS {
        for operand in OPERANDS {
            // The operand is also kept in a global and a local to check the callers' copies.
            let program = format!(
                r#"function h(s,    parts, n) {{ {op}; return s }}
function loc(x,    l) {{ l = x; r = h(l); return r "|" l }}
BEGIN {{ v = "a1"; a[1] = 77 }}
{{ g = {operand}; r = h({operand}); print r, g, {operand}; print loc({operand}); print h(g), g }}"#
            );
            let name = format!("op `{}` with operand `{}`\n{}", op, operand, program);
            if let Some(diff) = differences(&name, &dir, std::slice::from_ref(&program), stdin) {
                errors.push(diff);
            }
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
