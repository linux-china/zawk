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

#[test]
fn builtin_assignments_infer_udf_rhs() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cases = [
        (
            r#"function sep(){return ","} BEGIN{FS=sep();print FS}"#,
            ",\n",
        ),
        (
            r#"function sep(value){return value} BEGIN{FS=sep(",");OFS=sep(7);print FS,OFS}"#,
            ",77\n",
        ),
        (
            r#"function sep(){return 7} BEGIN{FS=sep();print FS}"#,
            "7\n",
        ),
        (
            r#"function amount(){return 2.75} BEGIN{RSTART=amount();print RSTART}"#,
            "2\n",
        ),
        (
            r#"function enabled(){return 0.5} BEGIN{IGNORECASE=enabled();print IGNORECASE,("A"=="a")}"#,
            "1 1\n",
        ),
        (
            r#"function sep(){return ","} function configure(){FS=sep()} BEGIN{configure();print FS}"#,
            ",\n",
        ),
    ];
    for (program, stdout) in cases {
        let args = [program.to_string()];
        for config in CONFIGS {
            assert_eq!(
                run(dir, config, &args, b""),
                Outcome {
                    code: Some(0),
                    stdout: stdout.to_string(),
                },
                "{config:?}\n{program}",
            );
        }
    }
}

#[test]
fn udf_arguments_snapshot_scalars_and_preserve_array_references() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cases = [
        (
            r#"function show(a,b){print a,b} BEGIN{i=1;show(i,i++)}"#,
            "1 1\n",
        ),
        (
            r#"function show(a,b){print a,b} BEGIN{i=1;show(i,++i)}"#,
            "1 2\n",
        ),
        (
            r#"function show(a,b){print a,b} BEGIN{i=1;show(i,i=9)}"#,
            "1 9\n",
        ),
        (
            r#"function show(a,b){print a,b} BEGIN{i=1;show(i,i+=2)}"#,
            "1 3\n",
        ),
        (
            r#"function show(a,b){print a,b} BEGIN{s="old";show(s,s="new")}"#,
            "old new\n",
        ),
        (
            r#"function show(a,b){print a,b} function bump(){i=9;return 7}
               BEGIN{i=1;show(i,bump());print i}"#,
            "1 7\n9\n",
        ),
        (
            r#"function show(a,b){print a,b} function caller(value){
                   local other=3;show(value,value++);show(other,other=9);print value,other
               } BEGIN{caller(1)}"#,
            "1 1\n3 9\n2 9\n",
        ),
        (
            r#"function change(a,b){a+=10;return a ":" b}
               BEGIN{i=1;print change(i,i++),i}"#,
            "11:1 2\n",
        ),
        (
            r#"function show(a,b){print a,b}
               BEGIN{a[1]="old";show(a[1],a[1]="new");print a[1]}"#,
            "old new\nnew\n",
        ),
        (
            r#"function show(a,b){print a,b}
               BEGIN{s="aaa";show(s,gsub(/a/,"b",s));print s}"#,
            "aaa 3\nbbb\n",
        ),
        (
            r#"function keep(value){local extra;print value,extra+0}
               BEGIN{i=1;keep(i,i=9);print i}"#,
            "1 0\n9\n",
        ),
        (
            r#"function abs(a,b){print a,b} BEGIN{i=1;abs(i,i++)}"#,
            "1 1\n",
        ),
        (
            r#"function show(a,b){print a,b}
               BEGIN{i=1;show(i,(i==1 ? i=9 : i=7));print i}"#,
            "1 9\n9\n",
        ),
        (
            r#"function populate(arr){arr["later"]=7;return 1}
               function inspect(arr,n){print arr["later"],n;arr["after"]=9}
               BEGIN{a["initial"]=3;inspect(a,populate(a));print a["initial"],a["after"]}"#,
            "7 1\n3 9\n",
        ),
        (
            r#"function fill(arr,n){return split("x,y",arr,",")}
               function forward(arr,n){return fill(arr,n++)}
               BEGIN{i=0;print forward(a,i++),i,a[1],a[2];
                   print forward(b,i++),i,b[1],b[2]}"#,
            "2 1 x y\n2 2 x y\n",
        ),
        (
            r#"function shared(left,right,n){
                   left["new"]=7;print right["old"],right["new"],n;
                   delete right;print length(left);split("x,y",left,",");print right[1],right[2]
               } BEGIN{a["old"]=3;i=0;shared(a,a,i++);print i,a[1],a[2]}"#,
            "3 7 0\n0\nx y\n1 x y\n",
        ),
    ];
    for (program, stdout) in cases {
        let args = [program.to_string()];
        for config in CONFIGS {
            assert_eq!(
                run(dir, config, &args, b""),
                Outcome {
                    code: Some(0),
                    stdout: stdout.to_string(),
                },
                "{config:?}\n{program}",
            );
        }
    }
}

#[test]
fn eval_context_result_conversion_agrees_between_backends() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for value in ["1", "1.0", r#""1""#] {
        let program = format!(
            r#"BEGIN {{
                ctx["x"] = {value}
                print eval("x > 0", ctx), eval("x < 0", ctx), \
                      eval("x == 1.0", ctx), eval("x == 2.0", ctx), \
                      eval("x + 2", ctx), eval("2 + 3", ctx), \
                      eval("2.5 + 0.25", ctx), eval("1 > 0"), eval("1 < 0")
            }}"#
        );
        let args = [program];
        for config in CONFIGS {
            assert_eq!(
                run(dir, config, &args, b""),
                Outcome {
                    code: Some(0),
                    stdout: "1 0 1 0 3 5 2.75 1 0\n".to_string(),
                },
                "{config:?}, context value {value}",
            );
        }
    }
}

#[test]
fn non_ascii_time_zone_offsets_fall_back_to_utc() {
    for name in [
        "+汉a", "-a汉", "+éé", "+𐀀", "+é", "-１２", "+汉:00", "+08:汉", "+08:60",
    ] {
        let program = format!(
            r#"BEGIN {{
                tz = "{name}"
                ERRNO = ""
                print strptime("2024-01-02", "%Y-%m-%d", tz)
                print ERRNO
                ERRNO = ""
                print strftime("%Y-%m-%d %H:%M:%S %z", 0, tz)
                print ERRNO
                print "continued"
            }}"#
        );
        let parse_warning = format!("strptime: unknown time zone {name:?}, using UTC");
        let format_warning = format!("strftime: unknown time zone {name:?}, using UTC");
        for config in CONFIGS {
            let output = Command::new(env!("CARGO_BIN_EXE_zawk"))
                .env("LC_ALL", "C.UTF-8")
                .env("TZ", "Asia/Shanghai")
                .args(*config)
                .arg(&program)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(0), "{config:?}, {name}");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                format!(
                    "1704153600\n{parse_warning}\n1970-01-01 00:00:00 +0000\n{format_warning}\ncontinued\n"
                ),
                "{config:?}, {name}",
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                format!("zawk: warning: {parse_warning}\nzawk: warning: {format_warning}\n"),
                "{config:?}, {name}",
            );
        }
    }
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
