use assert_cmd::Command;
use std::fs::{read_to_string, File};
use std::io::Write;
use tempfile::tempdir;

const BACKEND_ARGS: &[&str] = &["-Binterp", "-Bcranelift"];

// A simple function that looks for the "constant folded" regex instructions in the generated
// output. This is a function that is possible to fool: test cases should be mindful of how it is
// implemented to ensure it is testing what is intended.
fn assert_folded(p: &str) {
    let prog: String = p.into();
    let out = String::from_utf8(
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(prog)
            .arg(String::from("--dump-bytecode"))
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(out.contains("MatchConst") || out.contains("StartsWithConst"))
}

// Compare two byte slices, up to reordering the lines of each.
fn unordered_output_equals(bs1: &[u8], bs2: &[u8]) {
    let mut lines1: Vec<_> = bs1.split(|x| *x == b'\n').collect();
    let mut lines2: Vec<_> = bs2.split(|x| *x == b'\n').collect();
    lines1.sort();
    lines2.sort();
    if lines1 != lines2 {
        let pretty_1: Vec<_> = lines1.into_iter().map(String::from_utf8_lossy).collect();
        let pretty_2: Vec<_> = lines2.into_iter().map(String::from_utf8_lossy).collect();
        panic!("expected (in any order) {:?}, got {:?}", pretty_1, pretty_2);
    }
}

#[test]
fn constant_regex_folded() {
    // NB: 'function unused()' forces `x` to be global
    let prog: String = r#"function unused() { print x; }
BEGIN {
    x = "hi";
    x=ARGV[1];
    print("h" ~ x);
}"#
    .into();
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog.clone())
            .arg(String::from("h"))
            .assert()
            .stdout(String::from("1\n"));
    }
    {
        assert_folded(
            r#"function unused() { print x; }
    BEGIN { x = "hi"; print("h" ~ x); }"#,
        );
        assert_folded(r#"BEGIN { x = "hi"; x = "there"; print("h" ~ x); }"#);
    }
}

#[test]
fn simple_fi() {
    let input = r#"Item,Count
    carrots,2
    potato chips,3
    custard,1"#;
    let expected = "6 3\n";

    let tmpdir = tempdir().unwrap();
    let data_fname = tmpdir.path().join("numbers");
    {
        let mut file = File::create(data_fname.clone()).unwrap();
        file.write_all(input.as_bytes()).unwrap();
    }
    let prog: String = r#"{n+=$FI["Count"]} END { print n, NR; }"#.into();
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from("-icsv"))
            .arg(String::from("-H"))
            .arg(prog.clone())
            .arg(fname_to_string(&data_fname))
            .assert()
            .stdout(expected);
    }
}

#[test]
fn fi_header_per_file() {
    let tmpdir = tempdir().unwrap();
    let h1 = tmpdir.path().join("h1.csv");
    let h2 = tmpdir.path().join("h2.csv");
    let h0 = tmpdir.path().join("h0.csv");
    let bom = tmpdir.path().join("bom.csv");
    std::fs::write(&h1, "id,name,x\n1,a,p\n").unwrap();
    // The columns of a later file may be in a different order.
    std::fs::write(&h2, "x,name,id\nq,c,3\nr,d,4\n").unwrap();
    // A file with only a header line.
    std::fs::write(&h0, "id,name\n").unwrap();
    // Excel writes a UTF-8 byte order mark.
    std::fs::write(&bom, "\u{feff}id,name\n5,e\n").unwrap();
    let prog = r#"{print FILENAME ~ /h2/, NR, FNR, $FI["id"], $FI["name"], $FI["x"]} END {print NR, FNR}"#;
    let expected = "0 1 1 1 a p\n1 2 1 3 c q\n1 3 2 4 d r\n3 0\n";
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, "-icsv", "-H", prog])
            .args([&h0, &h1, &h2, &h0])
            .assert()
            .stdout(expected);
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, "-icsv", "-H", r#"{print $FI["id"] $FI["name"]}"#])
            .args([&bom, &h1, &bom])
            .assert()
            .stdout("5e\n1a\n5e\n");
    }
}

// A column name missing from the header (a typo, or no -H) reads as "", not as $0, and is
// not added to FI.
#[test]
fn fi_missing_column_is_empty() {
    let prog = r#"{ k = "nmae"; print "[" $FI["nmae"] "]", "[" $FI[k] "]", $FI["age"] + 1, $FI["nmae"] + 1, ($FI["nmae"] < 5); n = 0; for (c in FI) n++; print n }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, "-icsv", "-H", prog])
            .write_stdin("name,age\nx,1\n")
            .assert()
            .stdout("[] [] 2 1 1\n2\n");
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, r#"{ print "[" $FI["a"] "]" }"#])
            .write_stdin("a,b\n")
            .assert()
            .stdout("[]\n");
    }
}

#[test]
fn file_and_data_arg() {
    let input = r#"Hi"#;
    let prog = r#"{ print; }"#;
    let expected = "Hi\n";

    let tmpdir = tempdir().unwrap();
    let data_fname = tmpdir.path().join("numbers");
    let prog_fname = tmpdir.path().join("prog");
    {
        let mut data_file = File::create(data_fname.clone()).unwrap();
        data_file.write_all(input.as_bytes()).unwrap();
        let mut prog_file = File::create(prog_fname.clone()).unwrap();
        prog_file.write_all(prog.as_bytes()).unwrap();
    }
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-f")
            .arg(prog_fname.clone())
            .arg(data_fname.clone())
            .assert()
            .stdout(expected);
    }
}

#[test]
fn source_and_program_file() {
    let tmpdir = tempdir().unwrap();
    let lib_fname = tmpdir.path().join("lib.awk");
    let data_fname = tmpdir.path().join("data");
    std::fs::write(&lib_fname, "function f(x) { return x * 2 }\n").unwrap();
    std::fs::write(&data_fname, "a 1\nb 2\n").unwrap();
    for backend_arg in BACKEND_ARGS {
        // `-e` and `-f` are concatenated in command-line order.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-f")
            .arg(&lib_fname)
            .arg("-e")
            .arg("{ print f($2) }")
            .arg(&data_fname)
            .assert()
            .stdout("2\n4\n");
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .args(["-e", "BEGIN { x = 1 }", "--source", "BEGIN { print x + 1 }"])
            .assert()
            .stdout("2\n");
    }
}

#[test]
fn exec_ends_options() {
    let tmpdir = tempdir().unwrap();
    let prog_fname = tmpdir.path().join("prog.awk");
    std::fs::write(&prog_fname, "BEGIN { for (i = 1; i < ARGC; i++) print ARGV[i] }\n").unwrap();
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-E")
            .arg(&prog_fname)
            .args(["-v", "x=1", "--help", "foo"])
            .assert()
            .success()
            .stdout("-v\nx=1\n--help\nfoo\n");
    }
}

#[test]
fn characters_as_bytes() {
    let prog = r#"BEGIN { s = "éa"; print length(s), index(s, "a"), match(s, /a/), length(substr(s, 2)); printf "[%3.1s]\n", "ab" }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg(prog)
            .assert()
            .stdout("2 2 2 1\n[  a]\n");
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-b")
            .arg(prog)
            .assert()
            .stdout("3 3 3 2\n[  a]\n");
    }
}

#[test]
fn posix_disables_sugar_syntax() {
    Command::cargo_bin("zawk")
        .unwrap()
        .arg("$2,$1")
        .write_stdin("x y\n")
        .assert()
        .stdout("y x\n");
    // As a POSIX program, `$2,$1` is a range pattern.
    Command::cargo_bin("zawk")
        .unwrap()
        .args(["--posix", "$2,$1"])
        .write_stdin("x y\n")
        .assert()
        .stdout("x y\n");
}

#[test]
fn multiple_files() {
    let input = r#"Item,Count
carrots,2
potato chips,3
custard,1"#;
    let expected = r#"Item,Count
carrots,2
potato chips,3
custard,1
file 1
file 2 3
"#;

    let tmpdir = tempdir().unwrap();
    let data_fname = tmpdir.path().join("numbers");
    let prog1 = tmpdir.path().join("p1");
    let prog2 = tmpdir.path().join("p2");
    for (fname, data) in &[
        (&data_fname, input),
        (
            &prog1,
            r#"function max_of(x, y) { return x<y?y:x; } BEGIN { FS = ","; } { print; } END { print "file 1"; } "#,
        ),
        (
            &prog2,
            r#"{ x = max_of(int($2), x); } END { print "file 2", x; }"#,
        ),
    ] {
        let mut file = File::create(fname).unwrap();
        file.write_all(data.as_bytes()).unwrap();
    }
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(format!("-f{}", fname_to_string(&prog1)))
            .arg(format!("-f{}", fname_to_string(&prog2)))
            .arg(fname_to_string(&data_fname))
            .assert()
            .stdout(expected);
    }
}

mod v_args {
    //! Tests for v args.
    use super::*;

    #[test]
    fn simple() {
        let expected = "1\n";
        let prog: String = r#"BEGIN {print x;}"#.into();
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from("-vx=1"))
                .arg(prog.clone())
                .assert()
                .stdout(expected);
        }
    }

    #[test]
    fn ident_v_arg() {
        let expected = "var-with-dash\n";
        let prog: String = r#"BEGIN {print x;}"#.into();
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from("-vx=var-with-dash"))
                .arg(prog.clone())
                .assert()
                .stdout(expected);
        }
    }

    #[test]
    fn ident_v_arg_escape() {
        let expected = "var-with\n-dash 1+1\n";
        let prog: String = r#"BEGIN {print x, y;}"#.into();
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from("-vx=var-with\\n-dash"))
                .arg(String::from("-vy=1+1"))
                .arg(prog.clone())
                .assert()
                .stdout(expected);
        }
    }
}

#[test]
fn mixed_map() {
    let expected = "hi 0 5\n1 1 3\n";
    let prog: String = r#"BEGIN {
m[1]=2
m["1"]++
m["hi"]=5
for (k in m) {
    print k,k+0,  m[k]
}}"#
    .into();
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog.clone())
            .output()
            .unwrap()
            .stdout;
        unordered_output_equals(expected.as_bytes(), &output[..]);
    }
}

#[test]
fn iter_across_functions() {
    let input = ",,3,,4\n,,3,,6\n,,4,,5";
    let expected = "3 62\n4 30\n";

    let tmpdir = tempdir().unwrap();
    let data_fname = tmpdir.path().join("numbers");
    {
        let mut file = File::create(data_fname.clone()).unwrap();
        file.write_all(input.as_bytes()).unwrap();
    }
    let prog: String = r#"function update(h, k, v) {
            h[k] += v*v+v;
        }
        BEGIN {FS=",";}
        { update(h,$3,$5) }
        END {for (k in h) { print k, h[k]; }}"#
        .into();
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog.clone())
            .arg(fname_to_string(&data_fname))
            .output()
            .unwrap()
            .stdout;
        unordered_output_equals(expected.as_bytes(), &output[..]);
    }
}

#[test]
fn simple_rc() {
    let expected = "hi\n";
    for (prog, rc) in [
        (r#"BEGIN { print "hi"; exit(0); print "there"; }"#, 0),
        (r#"BEGIN { print "hi"; exit 0; print "there"; }"#, 0),
        (r#"BEGIN { print "hi"; exit; print "there"; }"#, 0),
        (r#"BEGIN { print "hi"; exit(1); print "there"; }"#, 1),
        (r#"BEGIN { print "hi"; exit 1; print "there"; }"#, 1),
        (r#"BEGIN { print "hi"; exit(4); print "there"; }"#, 4),
        (r#"BEGIN { print "hi"; exit 4; print "there"; }"#, 4),
    ] {
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from(prog))
                .assert()
                .stdout(expected)
                .code(rc);
        }
    }
}

#[test]
fn trivial_parallel_rc() {
    let expected = "hi\n";
    for (prog, rc) in [
        (r#"BEGIN { print "hi"; exit 0; print "there"; }"#, 0),
        (r#"END { print "hi"; exit 1; print "there"; }"#, 1),
    ] {
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from(prog))
                .arg("-pr")
                .assert()
                .stdout(expected)
                .code(rc);
        }
    }
}

#[test]
fn multi_rc() {
    let mut text = String::default();
    for _ in 0..50_000 {
        text.push_str("x\n");
    }
    let (dir, data) = file_from_string("inputs", &text);
    let out = dir.path().join("out");
    let prog = format!(
      "BEGIN {{ print \"should flush\" > \"{}\"; }} PID == 2 && NR == 100 {{ print \"hi\"; exit 2; }} PREPARE {{ m[PID] = NR; }} END {{ for (k in m) print k, m[k]; }}",
      fname_to_string(&out),
    );
    eprintln!("data={:?}", data);
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-pf")
            .arg("-j2")
            .arg(&prog)
            .arg(fname_to_string(&data))
            .arg(fname_to_string(&data))
            .arg(fname_to_string(&data))
            .arg(fname_to_string(&data))
            .assert()
            .stdout("hi\n")
            .code(2);
        assert_eq!(read_to_string(&out).unwrap(), "should flush\n");
    }
}

#[test]
fn nested_loops() {
    let expected = "0 0\n0 1\n0 2\n1 0\n1 1\n1 2\n2 0\n2 1\n2 2\n";
    let prog: String =
        "BEGIN { m[0]=0; m[1]=1; m[2]=2; for (i in m) for (j in m) print i,j; }".into();
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog.clone())
            .output()
            .unwrap()
            .stdout;
        unordered_output_equals(expected.as_bytes(), &output[..]);
    }
}

#[test]
fn for_loops_with_continue_and_update_statement() {
    let expected = "0 0\n0 1\n0 3\n0 4\n0 5\n2 0\n2 1\n2 3\n2 4\n2 5\n3 0\n3 1\n3 3\n3 4\n3 5\n4 0\n4 1\n4 3\n4 4\n4 5\n";
    let prog: String =
        "BEGIN { for (i = 0; i < 5; i++) { if (i == 1) { continue; } for (j = -1; j < 5;) { j += 1; if (j == 2) { continue; } print i,j; } } }".into();
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog.clone())
            .output()
            .unwrap()
            .stdout;
        unordered_output_equals(expected.as_bytes(), &output[..]);
    }
}

#[test]
fn dont_reorder_files_with_f() {
    let expected = "1 1\n2 2\n3 3\n";
    let prog = "NR == FNR { print NR, FNR}";
    let test_data_1 = "1\n2\n3\n";
    let test_data_2 = "1\n2\n3\n4\n5\n";
    let tmp = tempdir().unwrap();
    let prog_file = tmp.path().join("prog");
    let f1 = tmp.path().join("f1");
    let f2 = tmp.path().join("f2");
    File::create(f1.clone())
        .unwrap()
        .write_all(test_data_1.as_bytes())
        .unwrap();
    File::create(f2.clone())
        .unwrap()
        .write_all(test_data_2.as_bytes())
        .unwrap();
    File::create(prog_file.clone())
        .unwrap()
        .write_all(prog.as_bytes())
        .unwrap();
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(format!("-f{}", fname_to_string(&prog_file)))
            .arg(fname_to_string(&f1))
            .arg(fname_to_string(&f2))
            .assert()
            .stdout(String::from(expected));
    }
}

fn fname_to_string(path: &std::path::Path) -> String {
    path.to_owned().into_os_string().into_string().unwrap()
}

fn file_from_string(
    name: impl AsRef<str>,
    s: impl AsRef<str>,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let file = tmp.path().join(name.as_ref());
    File::create(file.clone())
        .unwrap()
        .write_all(s.as_ref().as_bytes())
        .unwrap();
    (tmp, file)
}

#[test]
fn non_utf8_input_does_not_panic() {
    // "caf" + Latin-1 'é' (0xE9): invalid UTF-8 must not abort the process
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(r#"{ print length($0), length(digest("md5", $0)), substr($0, 1, 3) }"#))
            .write_stdin(&b"caf\xe9\n"[..])
            .assert()
            .success()
            .stdout(String::from("4 32 caf\n"));
    }
}

#[test]
fn exit_runs_end_block() {
    // (program, expected stdout, expected exit code), compared with gawk
    let cases: &[(&str, &str, i32)] = &[
        (r#"{exit 2} END{print "end", NR}"#, "end 1\n", 2),
        (r#"BEGIN{exit 3} END{print "end", NR}"#, "end 0\n", 3),
        (r#"NR==2{exit} {s+=$1} END{print "sum", s}"#, "sum 1\n", 0),
        (r#"{exit 1; print "dead"} END{print "e"}"#, "e\n", 1),
        // `exit` without code in END keeps the earlier code, `exit code` in END exits at once
        (r#"{exit 4} END{print "end"; exit}"#, "end\n", 4),
        (r#"{exit 4} END{print "end"; exit 5; print "no"}"#, "end\n", 5),
        // `exit` inside (nested) user defined functions
        (
            r#"function f(x){ if (x==2) exit 7; return x*10 } {print f($1)} END{print "end", NR}"#,
            "10\nend 2\n",
            7,
        ),
        (
            r#"function g(){ exit 9 } function f(){ g(); print "no" } {f()} END{print "end"}"#,
            "end\n",
            9,
        ),
        (r#"function f(){ exit 6 } END{print "in end"; f(); print "no"}"#, "in end\n", 6),
        // without END block
        (r#"{print; exit 2}"#, "1\n", 2),
    ];
    for (prog, expected, code) in cases {
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from(*prog))
                .write_stdin("1\n2\n3\n")
                .assert()
                .code(*code)
                .stdout(String::from(*expected));
        }
    }
}

#[test]
fn redirect_truncates_existing_file() {
    for backend_arg in BACKEND_ARGS {
        let tmp = tempdir().unwrap();
        let out = tmp.path().join("out.txt");
        let out_s = out.to_str().unwrap().to_string();

        // `>` truncates on first open; later writes to the open handle append.
        std::fs::write(&out, "xxxxxxxx\nyyyy\n").unwrap();
        let prog = format!(r#"BEGIN {{ print "a" > "{0}"; print "b" > "{0}" }}"#, out_s);
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .assert()
            .success();
        assert_eq!(read_to_string(&out).unwrap(), "a\nb\n");

        // `>` after close() truncates again; `>>` appends.
        let prog = format!(
            r#"BEGIN {{ print "c" > "{0}"; close("{0}"); print "d" > "{0}"; close("{0}"); print "e" >> "{0}" }}"#,
            out_s
        );
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .assert()
            .success();
        assert_eq!(read_to_string(&out).unwrap(), "d\ne\n");

        // --out-file truncates as well.
        std::fs::write(&out, "xxxxxxxx\nyyyy\n").unwrap();
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(format!("--out-file={}", out_s))
            .arg(String::from(r#"BEGIN { print "z" }"#))
            .assert()
            .success();
        assert_eq!(read_to_string(&out).unwrap(), "z\n");
    }
}

#[test]
fn mod_by_zero_is_fatal() {
    // Integer and floating-point `%` by zero halt with an error instead of crashing.
    let progs = [
        r#"BEGIN { x = 0; print 1 % x }"#,
        r#"BEGIN { x = 0; print 1.5 % x }"#,
        r#"BEGIN { y = 5; y %= 0; print y }"#,
    ];
    for backend_arg in BACKEND_ARGS {
        for prog in progs {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from(prog))
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{} {}", backend_arg, prog);
            assert!(output.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("division by zero attempted in `%'"),
                "{} {}: {}",
                backend_arg,
                prog,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        // i64::MIN % -1 overflows but is well-defined in awk.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(
                r#"BEGIN { x = -9223372036854775807 - 1; y = -1; print x % y, 7 % 3, -7 % 3 }"#,
            ))
            .assert()
            .success()
            .stdout(String::from("0 1 -1\n"));
    }
}

#[test]
fn integer_overflow() {
    for backend_arg in BACKEND_ARGS {
        // Integer literals beyond 2^53 are floats, and sums of two non-constant values are
        // computed in floating point, so they neither wrap around nor halt (as in gawk).
        for (prog, expected) in [
            (r#"BEGIN { x = 9223372036854775807; print x + 1 }"#, "9223372036854775808\n"),
            (r#"BEGIN { x = -9223372036854775807; print x - 2 }"#, "-9223372036854775808\n"),
            (
                r#"BEGIN { a = 0; b = 1; for (i = 0; i < 100; i++) { c = a + b; a = b; b = c }
                           x = 1; for (i = 0; i < 70; i++) x += x
                           m[1] = 1; for (i = 0; i < 70; i++) m[1] += m[1]
                           print b, x, m[1], i + 1 }"#,
                "573147844013817200640 1180591620717411303424 1180591620717411303424 71\n",
            ),
        ] {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from(prog))
                .assert()
                .success()
                .stdout(String::from(expected));
        }
        // Products are computed in floating point, so they do not wrap around.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(
                r#"BEGIN { y = 3037000500; f = 1; for (i = 1; i <= 25; i++) f *= i;
                           print (y * y > 9.2e18), (f > 1.5e25 && f < 1.6e25), 6 * 7 }"#,
            ))
            .assert()
            .success()
            .stdout(String::from("1 1 42\n"));
    }
}

#[test]
fn float_array_keys() {
    // Floating-point keys are converted to strings, and can be mixed with integer keys.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(
                r#"BEGIN {
                    x = 2.0; b[x] = "two"; print b[2], length(b)
                    for (i = 1; i <= 4; i++) a[i / 2] = i
                    n = 0; for (k in a) n++
                    print n, a[0.5], a["1.5"], a[2]
                    for (i = 1; i <= 3; i++) c[i * 2] = i
                    print c[2], c[4], c["6"]
                }"#,
            ))
            .assert()
            .success()
            .stdout(String::from("two 1\n4 1 3 4\n1 2 3\n"));
    }
}

#[test]
fn argv0_is_program_name() {
    // As in gawk, ARGV[0] is the name zawk was invoked with, without its directory.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(r#"BEGIN { print ARGV[0], ARGC, ARGV[1] }"#))
            .arg(String::from("x"))
            .assert()
            .success()
            .stdout(String::from("zawk 2 x\n"));
    }
}

#[test]
fn large_integer_literals() {
    // Integer literals that are not exact as doubles (beyond 2^53, including those outside the
    // i64 range) are parsed as floats, as in gawk.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(
                r#"BEGIN {
                    print (100000000000000000000 == 1e20), (9223372036854775808 == 2^63)
                    print (-9223372036854775808 == -2^63), (0xffffffffffffffff == 2^64 - 1)
                    print 9223372036854775807, 0x7fffffffffffffff, 0x1F
                }"#,
            ))
            .assert()
            .success()
            .stdout(String::from(
                "1 1\n1 1\n9223372036854775808 9223372036854775808 31\n",
            ));
    }
}

#[test]
fn many_closed_output_files() {
    // Writing to many files, closing each one, must not keep a writer thread per file: this used
    // to fail to spawn a thread after ~16k files.
    const N: usize = 20000;
    let input: String = (1..=N).map(|i| format!("{}\n", i)).collect();
    let tmp = tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap();
    Command::cargo_bin("zawk")
        .unwrap()
        .args(["-v", &format!("d={}", dir)])
        .arg(r#"{ f = d "/o" $1; print > f; close(f) } END { print NR }"#)
        .write_stdin(input)
        .assert()
        .success()
        .stdout(format!("{}\n", N));
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), N);
    assert_eq!(read_to_string(tmp.path().join(format!("o{}", N))).unwrap(), format!("{}\n", N));
}

#[test]
fn many_unclosed_output_files() {
    // `print > ($1 ".txt")` without `close`: the number of writer threads is capped by closing
    // the least recently used files (reopened for appending), instead of aborting after ~16k
    // threads.
    const N: usize = 20000;
    let input: String = (0..2 * N).map(|i| format!("{}\n", i)).collect();
    let tmp = tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap();
    Command::cargo_bin("zawk")
        .unwrap()
        .args(["-v", &format!("d={}", dir), "-v", &format!("n={}", N)])
        .arg(r#"{ print > (d "/o" ($1 % n)) } END { print NR }"#)
        .write_stdin(input)
        .assert()
        .success()
        .stdout(format!("{}\n", 2 * N));
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), N);
    for i in [0, 1, N / 2, N - 1] {
        let data = read_to_string(tmp.path().join(format!("o{}", i))).unwrap();
        assert_eq!(data, format!("{}\n{}\n", i, i + N), "file o{}", i);
    }
}

#[test]
fn next_in_end_is_error() {
    // `next`/`nextfile` in END used to jump back to the main loop and rerun END forever.
    for prog in [r#"END { print "a"; next }"#, "END { nextfile }"] {
        for backend_arg in BACKEND_ARGS {
            let out = Command::cargo_bin("zawk")
                .unwrap()
                .arg(backend_arg)
                .arg(prog)
                .write_stdin("x\n")
                .timeout(std::time::Duration::from_secs(10))
                .assert()
                .failure()
                .stdout("")
                .get_output()
                .clone();
            assert!(String::from_utf8_lossy(&out.stderr).contains("used in END action"));
        }
    }
}

#[test]
fn close_output_command() {
    for backend_arg in BACKEND_ARGS {
        // close() waits for the command, so its output comes before later output, and earlier
        // output comes before the command's.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(
                r#"BEGIN { print "header"; print "b\na" | "sleep 0.2; sort"; close("sleep 0.2; sort"); print "done" }"#,
            ))
            .assert()
            .success()
            .stdout(String::from("header\na\nb\ndone\n"));
        // Commands still open at exit are waited for.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(r#"BEGIN { print "b\na" | "sleep 0.2; sort" }"#))
            .assert()
            .success()
            .stdout(String::from("a\nb\n"));
        // close() returns the exit status of an output or input command, 0 for files, and -1
        // for names that were never opened or are already closed.
        let tmp = tempdir().unwrap();
        let out = tmp.path().join("out.txt");
        let prog = format!(
            r#"BEGIN {{
                print "x" | "cat >/dev/null; exit 3"; print close("cat >/dev/null; exit 3")
                print close("never opened")
                print "y" > "{0}"; print close("{0}"); print close("{0}")
                "echo hi" | getline v; print close("echo hi"); print close("echo hi")
                "echo hi; exit 4" | getline v; print close("echo hi; exit 4")
                print close("cat >/dev/null; exit 3")
            }}"#,
            out.to_str().unwrap()
        );
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .assert()
            .success()
            .stdout(String::from("3\n-1\n0\n-1\n0\n-1\n4\n-1\n"));
        // A command killed by a signal gives 256 plus the signal number, as in gawk.
        #[cfg(unix)]
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(
                r#"BEGIN { print system("kill -9 $$"); "kill -9 $$" | getline; print close("kill -9 $$") }"#,
            ))
            .assert()
            .success()
            .stdout(String::from("265\n265\n"));
    }
}

#[test]
fn system_flushes_output() {
    for backend_arg in BACKEND_ARGS {
        // Pending stdout output appears before the command's output.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(r#"BEGIN { printf "a"; system("echo b"); print "c" }"#))
            .assert()
            .success()
            .stdout(String::from("ab\nc\n"));
        // Output files are flushed, so the command sees their contents.
        let tmp = tempdir().unwrap();
        let out = tmp.path().join("out.txt");
        let prog = format!(
            r#"BEGIN {{ print "x" > "{0}"; system("cat {0}"); print "y" }}"#,
            out.to_str().unwrap()
        );
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .assert()
            .success()
            .stdout(String::from("x\ny\n"));
    }
}

#[test]
fn environ_passed_to_commands() {
    // As in gawk, changes to ENVIRON are passed to system, getline and print pipes.
    let prog = r#"BEGIN {
        ENVIRON["ZAWK_T1"] = "bar"; delete ENVIRON["ZAWK_T2"]
        system("echo s=$ZAWK_T1 t=$ZAWK_T2")
        "echo g=$ZAWK_T1" | getline v; print v
        print "p" | "cat; echo $ZAWK_T1"
    }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .env("ZAWK_T2", "gone")
            .arg(String::from(*backend_arg))
            .arg(String::from(prog))
            .assert()
            .success()
            .stdout(String::from("s=bar t=\ng=bar\np\nbar\n"));
    }
}

#[test]
fn match_with_array() {
    // gawk's match(s, re, arr): arr[0] is the whole match, arr[n] the n-th capture group, and
    // arr[n, "start"] / arr[n, "length"] their positions; a failed match clears arr.
    let prog = r#"BEGIN {
        n = match("foo=bar42 baz", /([a-z]+)=([a-z]+)([0-9]+)?/, m)
        print n, RSTART, RLENGTH, m[0], m[1], m[2], m[3]
        match("xx abc123", /([a-z]+)([0-9]+)/, a)
        print a[0], a[1], a[2], a[1, "start"], a[1, "length"], a[2, "start"]
        print match("zzz", /q(.)/, a), RSTART, RLENGTH, length(a)
    }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from(prog))
            .assert()
            .success()
            .stdout(String::from(
                "1 1 9 foo=bar42 foo bar 42\nabc123 abc 123 4 3 7\n0 0 -1 0\n",
            ));
    }
}

#[test]
fn redirect_open_failure_is_fatal() {
    // As in gawk, an output file that cannot be opened stops the program with an error, after
    // the output printed so far.
    let tmp = tempdir().unwrap();
    let bad = tmp.path().join("missing-dir").join("out.txt");
    let bad = bad.to_str().unwrap();
    let progs = [
        format!(r#"BEGIN {{ print "before"; print "x" > "{}"; print "after" }}"#, bad),
        format!(r#"BEGIN {{ print "before"; printf "%s\n", "x" >> "{}"; print "after" }}"#, bad),
    ];
    for backend_arg in BACKEND_ARGS {
        for prog in &progs {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(prog)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{} {}", backend_arg, prog);
            assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n", "{}", backend_arg);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("cannot redirect to"), "{}: {}", backend_arg, stderr);
        }
        // Reopening a file after close() truncates it again for `>`, and appends for `>>`.
        let out = tmp.path().join("ok.txt");
        let prog = format!(
            r#"BEGIN {{ f = "{0}"; print "1" > f; print "2" > f; close(f); print "3" >> f; close(f);
                       print "4" > f; close(f); while ((getline l < f) > 0) print l }}"#,
            out.to_str().unwrap()
        );
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .assert()
            .success()
            .stdout(String::from("4\n"));
    }
}

#[test]
fn dev_stdout_stderr_keep_program_order() {
    // print > "/dev/stdout" shares the buffer of standard output, and print > "/dev/stderr" is
    // written after flushing standard output, so the merged output is in program order.
    let zawk = assert_cmd::cargo::cargo_bin("zawk");
    let prog = r#"BEGIN { print "1"; print "2" > "/dev/stderr"; print "3" > "/dev/stdout";
                         printf "4\n" > "/dev/stderr"; print "5" }
                  { print "o" $0; print "e" $0 > "/dev/stderr" }"#;
    for backend_arg in BACKEND_ARGS {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(r#""$0" "$1" "$2" 2>&1"#)
            .arg(&zawk)
            .arg(backend_arg)
            .arg(prog)
            .stdin(File::open(test_data_file("a.txt")).unwrap())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", backend_arg);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "1\n2\n3\n4\n5\noline one\neline one\noline two\neline two\n",
            "{}",
            backend_arg
        );
    }
}

fn test_data_file(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/compat/data")
        .join(name)
}

#[test]
fn exit_codes_follow_gawk() {
    // As in gawk: 1 for usage and syntax errors, 2 for other fatal errors.
    let cases: &[(&[&str], i32)] = &[
        (&["BEGIN { print 1 +"], 1),
        (&["--bogus-option", "BEGIN { }"], 1),
        (&["-v", "x", "BEGIN { }"], 1),
        (&[], 1),
        (&["BEGIN { x = 0; print 1 % x }"], 2),
        (&["BEGIN { f() }"], 2),
        (&["BEGIN { a = 1; a[1] = 2 }"], 2),
        (&["-v", "1x=2", "BEGIN { }"], 2),
        (&["-f", "/nonexistent/prog.awk"], 2),
        (&["{ print }", "/nonexistent/input.txt"], 2),
        (&["BEGIN { exit 3 }"], 3),
        (&["--version"], 0),
    ];
    for backend_arg in BACKEND_ARGS {
        for (args, code) in cases {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .args(*args)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(*code), "{} {:?}", backend_arg, args);
        }
    }
}

#[test]
fn div_by_zero_is_fatal() {
    // As in gawk, `/` by zero halts with an error (exit code 2) instead of producing inf/nan.
    let progs = [
        r#"BEGIN { x = 0; print 1 / x }"#,
        r#"BEGIN { print 0 / 0 }"#,
        r#"BEGIN { y = 5; y /= 0; print y }"#,
        r#"BEGIN { x = "abc"; print 1 / x }"#,
    ];
    for backend_arg in BACKEND_ARGS {
        for prog in progs {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .arg(String::from(prog))
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{} {}", backend_arg, prog);
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("division by zero attempted"), "{} {}: {}", backend_arg, prog, stderr);
        }
    }
}

#[test]
fn parquet_input() {
    // -i parquet: $1..$NF are the columns, FI maps column names, $0 is the row as JSON, nested
    // values (LIST, STRUCT, MAP) and JSON columns are JSON text.
    let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/types.parquet");
    let cases = [
        (
            r#"{ print $1, $2, $FI["age"], $FI["amount"], $FI["active"], NF }"#,
            "1 Alice 30 12345.67 1 14\n2 Bob 张三 25 -0.50 0 14\n3  41   14\n",
        ),
        (
            r#"NR == 1 { print $FI["tags"]; print $FI["addr"]; print $FI["attrs"]; print $FI["doc"]; print $FI["uid"], $FI["ts"] }"#,
            "[1,2,3]\n{\"city\":\"Beijing\",\"zip\":100000}\n{\"k1\":\"v1\"}\n{\"a\": [true, null]}\na0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11 2024-01-15 10:30:00\n",
        ),
        (
            r#"NR == 2"#,
            "{\"id\":2,\"name\":\"Bob 张三\",\"age\":25,\"score\":null,\"active\":false,\"born\":\"1970-01-01\",\"ts\":\"1999-12-31 23:59:59.123\",\"amount\":-0.50,\"tags\":[],\"addr\":{\"city\":null,\"zip\":0},\"attrs\":{},\"doc\":null,\"uid\":null,\"note\":null}\n",
        ),
        (
            r#"{ s += $FI["age"] } END { print NR, s, $FI["missing"] "|" }"#,
            "3 96 |\n",
        ),
    ];
    for backend_arg in BACKEND_ARGS {
        for (prog, expected) in cases {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(String::from(*backend_arg))
                .args(["-i", "parquet", prog])
                .arg(&demo)
                .assert()
                .success()
                .stdout(String::from(expected));
        }
        // Standard input, and an error for a file that is not Parquet.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .args(["-i", "parquet", r#"{ print $FI["name"] }"#])
            .pipe_stdin(&demo)
            .unwrap()
            .assert()
            .success()
            .stdout("Alice\nBob 张三\n\n");
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .args(["-i", "parquet", "{ print }", "Cargo.toml"])
            .assert()
            .code(2);
    }
}

#[test]
fn unreadable_input_is_fatal_with_regex_separators() {
    // With a regex FS, a custom RS, paragraph mode, or FS assigned in a rule, records are read by
    // the regex splitter. An input file that cannot be read stops the program with an error, as
    // with the default separators and in gawk, instead of being read as empty input.
    let tmp = tempdir().unwrap();
    let ok = tmp.path().join("ok.txt");
    File::create(&ok).unwrap().write_all(b"a,b\n").unwrap();
    let ok = ok.to_str().unwrap();
    let missing = tmp.path().join("missing.txt");
    let missing = missing.to_str().unwrap();
    let cases: &[&[&str]] = &[
        &["-F[,;]", "{ print $1 }"],
        &["-vRS=x", "{ print }"],
        &["-vRS=", "{ print }"],
        &["{ FS = \":\" } { print $1 }"],
    ];
    for backend_arg in BACKEND_ARGS {
        for args in cases {
            for files in [vec![missing, ok], vec![ok, missing]] {
                let output = Command::cargo_bin("zawk")
                    .unwrap()
                    .arg(backend_arg)
                    .args(*args)
                    .args(&files)
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(2), "{} {:?} {:?}", backend_arg, args, files);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains("missing.txt"), "{} {:?}: {}", backend_arg, args, stderr);
            }
        }
        // `getline < file` still returns -1 rather than failing; a failing command that prints
        // nothing is just at EOF (its status is reported by close()), as in gawk.
        let prog = format!(
            r#"BEGIN {{ RS = "x"; print (getline l < "{}"); print ("cat {} 2>/dev/null" | getline l) }}"#,
            missing, missing
        );
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-A")
            .arg(prog)
            .assert()
            .success()
            .stdout("-1\n0\n");
    }
}

#[test]
fn default_fs_records_across_buffers() {
    // With the default FS, records longer than the read buffer (which then grows), and records
    // that cross buffer boundaries, keep all their fields: a record of a 100-byte field and a
    // 1-byte field must not become a single field, and a field before trailing whitespace must
    // not include it.
    let mut input = String::new();
    let mut seed = 7u64;
    let mut next = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for _ in 0..200 {
        for _ in 0..next(6) {
            input.push_str([" ", "  ", "\t", " \t ", ""][next(5) as usize]);
            let len = [1, 3, 10, 63, 64, 65, 100, 300, 1000][next(9) as usize];
            input.push_str(&"x".repeat(len));
        }
        input.push_str(["", " ", "\t"][next(3) as usize]);
        input.push('\n');
    }
    let expected: String = input
        .lines()
        .map(|line| {
            let fields: Vec<_> = line.split_ascii_whitespace().collect();
            let mut s = fields.len().to_string();
            for f in fields {
                s.push_str(&format!(" {}", f.len()));
            }
            s + "\n"
        })
        .collect();
    let prog = r#"{ s = NF; for (i = 1; i <= NF; i++) s = s " " length($i); print s }"#;
    for backend_arg in BACKEND_ARGS {
        for chunk_size in ["64", "100", "1000", "8192"] {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .arg(backend_arg)
                .arg("--chunk-size")
                .arg(chunk_size)
                .arg(prog)
                .write_stdin(input.clone())
                .output()
                .unwrap();
            assert!(output.status.success(), "{} {}", backend_arg, chunk_size);
            assert!(
                String::from_utf8_lossy(&output.stdout) == expected,
                "{} --chunk-size {}: fields differ",
                backend_arg,
                chunk_size
            );
        }
    }
}

#[test]
fn empty_input_files_do_not_end_input() {
    // An empty input file, or one whose last record ends exactly at the end of a read buffer,
    // ends only that file: the following files are still read.
    let tmp = tempdir().unwrap();
    let path = |name: &str| tmp.path().join(name).to_str().unwrap().to_string();
    let (empty, f1, f2, exact) = (path("empty.txt"), path("f1.txt"), path("f2.txt"), path("exact.txt"));
    File::create(&empty).unwrap();
    File::create(&f1).unwrap().write_all(b"a,1\nb,2\n").unwrap();
    File::create(&f2).unwrap().write_all(b"c,3\n").unwrap();
    // 64 bytes: with --chunk-size 64, the input ends exactly at the end of the first buffer.
    File::create(&exact).unwrap().write_all(format!("{}\n", "x".repeat(63)).as_bytes()).unwrap();
    let prog = "{ print $1 } END { print NR }";
    let cases: &[(&[&str], Vec<&String>, &str)] = &[
        (&[], vec![&empty, &f1, &empty, &f2], "a,1\nb,2\nc,3\n3\n"),
        (&["-F,"], vec![&empty, &f1, &empty, &f2], "a\nb\nc\n3\n"),
        (&["-icsv"], vec![&empty, &f1, &empty, &f2], "a\nb\nc\n3\n"),
        (&["-F,"], vec![&empty, &empty], "0\n"),
        (&["--chunk-size", "64"], vec![&exact, &f2], &format!("{}\nc,3\n2\n", "x".repeat(63))),
        (&["--chunk-size", "64", "-F,"], vec![&exact, &f2], &format!("{}\nc\n2\n", "x".repeat(63))),
    ];
    for backend_arg in BACKEND_ARGS {
        for (opts, files, expected) in cases {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(backend_arg)
                .args(*opts)
                .arg(prog)
                .args(files)
                .assert()
                .success()
                .stdout(expected.to_string());
        }
    }
}

#[test]
fn awk_builtins_cannot_be_redefined() {
    // POSIX and gawk builtins cannot be redefined, as in gawk; zawk's stdlib extensions can (see
    // tests/compat/cases/08-function-shadows-stdlib.awk).
    for name in ["length", "substr", "split", "gsub", "sprintf", "gensub"] {
        let prog = format!("function {}(s) {{ return 1 }} BEGIN {{ print 1 }}", name);
        let output = Command::cargo_bin("zawk").unwrap().arg(prog).output().unwrap();
        assert!(!output.status.success(), "{}", name);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("`{}' is a built-in function, it cannot be redefined", name)),
            "{}: {}",
            name,
            stderr
        );
    }
}

#[test]
fn special_variables_cannot_be_parameters() {
    // As in gawk, a special variable used as a parameter is an error (zawk used to ignore the
    // argument and resolve the name to the global, so assignments clobbered it).
    for (prog, name) in [
        ("function f(NR) { return NR } BEGIN { print f(3) }", "NR"),
        ("function f(x, NR) { NR = 5; return x } { f(1, 2); print NR }", "NR"),
        ("function f(NF, FS) { return NF \"-\" FS } BEGIN { print f(3, \":\") }", "NF"),
    ] {
        let output = Command::cargo_bin("zawk").unwrap().arg(prog).output().unwrap();
        assert!(!output.status.success(), "{}", prog);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "parameter `{}': cannot use a special variable as a function parameter",
                name
            )),
            "{}: {}",
            prog,
            stderr
        );
    }
}

#[test]
fn csv_output_of_conditional_print_argument() {
    // With -o csv, a print argument that is a conditional expression is escaped like the others
    // (it was printed as an empty field).
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-ocsv")
            .arg(r#"{ print (NR > 0 ? "x,y" : "z"), $1 }"#)
            .write_stdin("a,b\n")
            .assert()
            .success()
            .stdout("\"x,y\",\"a,b\"\n");
    }
}

#[test]
fn csv_rfc4180_backslash_and_line_breaks() {
    // Backslashes are plain data in CSV (only "" is an escape), a lone \r is kept, and -o csv
    // keeps line breaks inside quotes.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-icsv")
            .arg(r#"{ print "[" $1 "]" "[" $2 "]" }"#)
            .write_stdin("\"C:\\new\\table\",x\na\rb,\"say \"\"hi\"\"\"\r\n")
            .assert()
            .success()
            .stdout("[C:\\new\\table][x]\n[a\rb][say \"hi\"]\n");
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-ocsv")
            .arg(r#"BEGIN { print "multi\nline", "C:\\new", "q\"q", "t\tt" }"#)
            .assert()
            .success()
            .stdout("\"multi\nline\",C:\\new,\"q\"\"q\",t\tt\n");
    }
}

#[test]
fn malformed_input_warnings() {
    // Bad JSONL lines and a CSV quote that is never closed are still read, but reported on stderr
    // (blank JSONL lines are not). A kind of warning is reported at most 5 times.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-ijsonl")
            .arg("{ print NR, NF }")
            .write_stdin("{\"a\":1}\nbad\n\n{\"a\":2\n{}\n[1,2]\n\"x\"\n")
            .assert()
            .success()
            .stdout("1 1\n2 0\n3 0\n4 1\n5 1\n6 2\n7 1\n")
            .stderr("zawk: warning: -:2: invalid JSON\nzawk: warning: -:4: invalid JSON\n");
        let out = Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-ijsonl")
            .arg("END { print NR }")
            .write_stdin("bad\n".repeat(8))
            .assert()
            .success()
            .stdout("8\n")
            .get_output()
            .clone();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(stderr.lines().count(), 6, "{}", stderr);
        assert!(stderr.ends_with("-:5: invalid JSON\nzawk: warning: further `invalid JSON' warnings suppressed\n"));
        // With and without the quoted field used by the program.
        for prog in [r#"{ print NR, "[" $2 "]" }"#, "{ print NR }"] {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(backend_arg)
                .arg("-icsv")
                .arg(prog)
                .write_stdin("x,\"y\"\"z\"\na,\"b\nc\n")
                .assert()
                .success()
                .stderr("zawk: warning: -: record 2: unterminated quoted field runs to the end of the file\n");
        }
    }
}

#[test]
fn markdown_output() {
    // -o markdown renders print records as a Markdown table: the first record is the header,
    // followed by the delimiter row; `|` in fields is escaped and tabs/newlines are kept on one line.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-omarkdown")
            .arg(r#"BEGIN { print "name", "age" } { print $1, $2 } END { print "a|b\tc", "x\ny" }"#)
            .write_stdin("Alice 30\nBob 25\n")
            .assert()
            .success()
            .stdout(
                "| name | age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |\n| a\\|b\\tc | x\\ny |\n",
            );
    }
}

#[test]
fn table_output() {
    // -o table renders print records as a bordered table: the first record is the header,
    // numeric columns are right-aligned and short records are padded.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-otable")
            .arg(r#"BEGIN { print "name", "age" } { print $1, $2 } END { print "张三" }"#)
            .write_stdin("Alice 30\nBob 5\n")
            .assert()
            .success()
            .stdout(
                "┌───────┬─────┐\n│ name  │ age │\n├───────┼─────┤\n│ Alice │  30 │\n│ Bob   │   5 │\n│ 张三  │     │\n└───────┴─────┘\n",
            );
    }
}

#[test]
fn jsonl_output() {
    // -o jsonl writes print records as JSON objects: the first record is the header, where
    // `name:number` and `name:boolean` set the JSON type, and missing columns are null.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-ojsonl")
            .arg(r#"BEGIN { print "name", "age:number", "ok:boolean" } { print $1, $2, $3 } END { print "a\tb" }"#)
            .write_stdin("Alice 30 1\nBob x 0\nCarol 5 no\n")
            .assert()
            .success()
            .stdout(concat!(
                r#"{"name":"Alice","age":30,"ok":true}"#,
                "\n",
                r#"{"name":"Bob","age":0,"ok":false}"#,
                "\n",
                r#"{"name":"Carol","age":5,"ok":false}"#,
                "\n",
                r#"{"name":"a\tb","age":null,"ok":null}"#,
                "\n",
            ));
    }
}

#[test]
fn parquet_output() {
    // -o parquet writes the printed records as a Parquet file: the first record is the header,
    // where `name:TYPE` sets a column type; it is read back here with -i parquet.
    let tmpdir = tempdir().unwrap();
    let out = tmpdir.path().join("out.parquet");
    let out = out.to_str().unwrap();
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("-oparquet")
            .arg("--out-file")
            .arg(out)
            .arg(r#"BEGIN { print "name", "age:int32", "ok:BOOLEAN", "d:Date", "ts:Timestamp", "j:JSON", "id:UUID" }
                { print $1, $2, $3, $4, $5, $6, $7 }"#)
            .write_stdin(
                "Alice 30 true 2024-01-02 2024-01-02T03:04:05Z {\"a\":1} 67e55044-10b1-426f-9247-bb680e5fe0c8\n\
                 Bob x 0 86400 1700000000 {} -\n",
            )
            .assert()
            .success();
        Command::cargo_bin("zawk")
            .unwrap()
            .arg("-iparquet")
            .arg("{ print $0 }")
            .arg(out)
            .assert()
            .success()
            .stdout(concat!(
                r#"{"name":"Alice","age":30,"ok":true,"d":"2024-01-02","ts":"2024-01-02 03:04:05","j":{"a":1},"id":"67e55044-10b1-426f-9247-bb680e5fe0c8"}"#,
                "\n",
                r#"{"name":"Bob","age":null,"ok":false,"d":"1970-01-02","ts":"2023-11-14 22:13:20","j":{},"id":null}"#,
                "\n",
            ));
    }
    // An unknown column type is a fatal error.
    Command::cargo_bin("zawk")
        .unwrap()
        .arg("-oparquet")
        .arg("--out-file")
        .arg(out)
        .arg(r#"BEGIN { print "a:nope" }"#)
        .assert()
        .code(2);
}

#[test]
fn strftime_arguments() {
    // strftime(format, timestamp, utc), as in gawk, in both backends: the timestamp defaults to
    // the current time, negative timestamps are dates before 1970, the third argument formats in
    // UTC, an empty format is an empty string, and strftime() uses PROCINFO["strftime"].
    let prog = r#"BEGIN {
        print strftime("%Y-%m-%d %H", 0), strftime("%Y-%m-%d %H", 0, 1), strftime("%H", 0, "")
        print strftime("%Y-%m-%d", -86400, 1), "[" strftime("", 0) "]"
        print (strftime("%Y") == strftime("%Y", systime())), (index(strftime(), strftime("%Y")) > 0)
        PROCINFO["strftime"] = "%Y-%m-%d"; print strftime("") "|" (strftime() == strftime("%Y-%m-%d"))
    }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .env("TZ", "Asia/Shanghai")
            .arg(backend_arg)
            .arg(prog)
            .assert()
            .success()
            .stdout("1970-01-01 08 1970-01-01 00 08\n1969-12-31 []\n1 1\n|1\n");
    }
}

#[test]
fn unicode_identifiers_and_hex() {
    // identifiers may contain multi-byte characters; hex() accepts strings shorter than "0x"
    let prog = r#"BEGIN { 变量 = 3; print 变量 + 1; print hex("5"), hex(""), hex("-"), hex("0x1f"), hex("-ff") }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg(prog)
            .assert()
            .success()
            .stdout("4\n5 0 0 31 -255\n");
    }
}

#[test]
fn return_outside_function() {
    for prog in ["BEGIN { return 1 }", "{ return }", "END { return }"] {
        let output = Command::cargo_bin("zawk").unwrap().arg(prog).write_stdin("").output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{}", prog);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("`return' used outside function context"), "{}", stderr);
    }
}

#[test]
fn stdlib_invalid_input_warns() {
    // stdlib functions warn (once) and return an empty value instead of aborting
    let prog = r#"BEGIN {
        print "[" encode("hex-base64", "zz") "]", eval("1+"), "[" read_all("/nonexistent/zawk") "]"
        print "[" html_value("<p>x</p>", "[[[") "]", "[" xml_value("<a", "/a") "]", "[" json_value("{}", "$[") "]"
        print is("nope", "x"), length(sqlite_query(":memory:", "selec x"))
        a = rparse("a", "(b)?(a)"); print "[" a[1] "]" a[2]
    }"#;
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk").unwrap().arg(backend_arg).arg(prog).output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "[] 0 []\n[] [] []\n0 0\n[]a\n");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("zawk: warning: encode: hex-base64"), "{}", stderr);
    }
}

#[test]
fn func_keyword_alias() {
    // `func` is accepted as an alias of `function` (gawk, BWK awk)
    let prog = "func f(x){return x*2} function g(y){return f(y)+1} BEGIN{print f(2), g(3)}";
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk").unwrap().arg(backend_arg).arg(prog).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "4 7\n");
    }
}

#[test]
fn function_name_space_before_paren() {
    // whitespace is allowed between the function name and `(` in a declaration
    let prog = "function f (a) {return a*2} func g\t(x, y){return x+y} BEGIN{print f(3), g(1, 2)}";
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk").unwrap().arg(backend_arg).arg(prog).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "6 3\n");
    }
}

#[test]
fn input_file_operands() {
    // `-` is standard input, directories are skipped with a warning and empty operands are
    // skipped, as in gawk.
    let tmp = tempdir().unwrap();
    let hdr = tmp.path().join("hdr");
    File::create(&hdr).unwrap().write_all(b"h\n").unwrap();
    let hdr = hdr.to_str().unwrap();
    let dir = tmp.path().to_str().unwrap();
    for backend_arg in BACKEND_ARGS {
        for fs in ["-F ", "-F,", "-F::"] {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .args([backend_arg, fs, "{print FILENAME == \"-\", $0} END{print NR}", hdr, dir, "", "-"])
                .write_stdin("a\nb\n")
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(String::from_utf8_lossy(&output.stdout), "0 h\n1 a\n1 b\n3\n");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(&format!("zawk: warning: command line argument `{}' is a directory: skipped", dir)), "{}", stderr);
        }
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .args([backend_arg, "BEGIN{while ((getline l < \"-\") > 0) print \"got\", l}"])
            .write_stdin("a\nb\n")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "got a\ngot b\n");
    }
}

#[test]
fn input_file_cannot_open() {
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .args([backend_arg, "BEGIN{print \"begin\"} {print}", "nonexist"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "begin\n");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "zawk: fatal: cannot open file `nonexist' for reading: No such file or directory\n"
        );
    }
}

#[test]
fn empty_match_respects_utf8_boundaries() {
    let cases = [
        (r#"BEGIN{s="中文"; n=gsub(//, "-", s); print n, s}"#, "3 -中-文-\n"),
        (r#"BEGIN{s="中文"; gsub(/x*/, "-", s); print s}"#, "-中-文-\n"),
        (r#"BEGIN{print gensub(//, "-", "g", "中文")}"#, "-中-文-\n"),
        (r#"BEGIN{print gensub(//, "-", 2, "中文")}"#, "中-文\n"),
        (r#"BEGIN{print gensub(/^a/, "X", 2, "aaa")}"#, "aaa\n"),
        (r#"BEGIN{print gensub(/\<a/, "X", 2, "aaa ab")}"#, "aaa Xb\n"),
    ];
    for backend_arg in BACKEND_ARGS {
        for (prog, expected) in cases {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .args([backend_arg, prog])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&output.stdout), expected, "{} {}", backend_arg, prog);
        }
    }
}

#[test]
fn extension_functions_reject_invalid_args_without_panic() {
    let cases = [
        (r#"BEGIN{print mask("é")}"#, "*\n"),
        (r#"BEGIN{print mask("中文")}"#, "中*\n"),
        (r#"BEGIN{print truncate("中文字符串", 4, "..")}"#, "中文..\n"),
        (r#"BEGIN{print truncate("abcdef", 1, "...")}"#, "a\n"),
        (r#"BEGIN{print truncate("abcdef", 5, "..")}"#, "abc..\n"),
        (r#"BEGIN{print "[" repeat("a", -1) "]"}"#, "[]\n"),
        (r#"BEGIN{print "[" pad("a", -5, "*") "]"}"#, "[]\n"),
        (r#"BEGIN{print "[" mkpass(-1) "]"}"#, "[]\n"),
    ];
    for backend_arg in BACKEND_ARGS {
        for (prog, expected) in cases {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .args([backend_arg, prog])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&output.stdout), expected, "{} {}", backend_arg, prog);
        }
    }
}

#[test]
fn error_messages_are_gawk_style() {
    // (program, exit code, expected parts of stderr)
    let cases: &[(&str, i32, &[&str])] = &[
        (
            r#"BEGIN { undefined_fn() }"#,
            2,
            &[
                r#"zawk: fatal: line 1, column 9: Call to unknown function "undefined_fn""#,
                "    BEGIN { undefined_fn() }\n            ^\n",
            ],
        ),
        (
            r#"BEGIN { x = -1; print $x }"#,
            2,
            &["zawk: fatal: attempt to access field -1"],
        ),
        (
            r#"BEGIN { a[1][2] = 1 }"#,
            2,
            &["zawk: fatal: arrays of arrays such as `a[i][j]` are not supported"],
        ),
        (
            r#"BEGIN { if (1) { }}}"#,
            1,
            &[
                "zawk: syntax error at line 1, column 20: unexpected `}`\n",
                "    BEGIN { if (1) { }}}\n                       ^\n",
            ],
        ),
        // An error at the end of a line is reported on that line.
        (
            "BEGIN {\n  print 1 +\n}",
            1,
            &["zawk: syntax error at line 2, column 12: unexpected newline\n      print 1 +\n"],
        ),
        // Invalid regex literals are syntax errors, as in gawk.
        (
            r#"$0 ~ /a(b/"#,
            1,
            &["zawk: syntax error at line 1, column 6: invalid regex /a(b/: unclosed group"],
        ),
        // Invalid switch statements, as in gawk.
        (
            "BEGIN { switch (x) { case 1: break; case \"1\": break } }",
            1,
            &["zawk: syntax error at line 1, column 37: duplicate case values in switch body: 1"],
        ),
        (
            "BEGIN { switch (x) { case 1:\ncase 1.0: break } }",
            1,
            &["zawk: syntax error at line 2, column 1: duplicate case values in switch body: 1"],
        ),
        (
            "BEGIN { switch (x) { default: break; default: break } }",
            1,
            &["zawk: syntax error at line 1, column 38: duplicate `default' detected in switch body"],
        ),
        (
            "BEGIN { switch (x) { case y: break } }",
            1,
            &["zawk: syntax error at line 1, column 27: unexpected `y`"],
        ),
        (
            "BEGIN { switch (x) { case 1: continue } }",
            2,
            &["zawk: fatal: `continue' is not allowed outside a loop"],
        ),
        (
            "{ switch ($1) { default: continue } }",
            2,
            &["zawk: fatal: `continue' is not allowed outside a loop"],
        ),
        (
            "BEGIN { switch (x) { } break }",
            2,
            &["zawk: fatal: `break' is not allowed outside a loop or switch"],
        ),
        // Dynamic regexes fail when they are used.
        (
            r#"BEGIN { r = "("; print ("x" ~ r) }"#,
            2,
            &["zawk: fatal: invalid regex /(/: unclosed group\n"],
        ),
    ];
    for backend_arg in BACKEND_ARGS {
        for (prog, code, parts) in cases {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .env_remove("ZAWK_DEBUG")
                .arg(String::from(*backend_arg))
                .arg(String::from(*prog))
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(*code), "{} {}: {}", backend_arg, prog, stderr);
            // Locations in the zawk sources are only reported by debug builds.
            let stderr = if cfg!(debug_assertions) {
                strip_source_locations(&stderr)
            } else {
                assert!(!stderr.contains("[src/"), "{} {}: {}", backend_arg, prog, stderr);
                stderr.to_string()
            };
            for part in *parts {
                assert!(stderr.contains(part), "{} {}: {}", backend_arg, prog, stderr);
            }
        }
    }
}

#[test]
fn static_errors_have_locations() {
    // Errors found after parsing (unknown functions, argument counts, arrays used as scalars) are
    // reported like syntax errors: `file:line:column` for `-f` files, then the line and a caret.
    let tmpdir = tempdir().unwrap();
    let write = |name: &str, text: &str| {
        let path = tmpdir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path.into_os_string().into_string().unwrap()
    };
    let ok = write("ok.awk", "BEGIN { x = 1 }\n");
    let split = write("split.awk", "BEGIN {\n  n = split(s, arr, \" \", x, y)\n}\n");
    let substr = write("substr.awk", "BEGIN {\n\tx = substr(\"abc\")\n}\n");
    let array = write("array.awk", "BEGIN {\n  a[1] = 1\n  print a\n}\n");
    let scalar = write("scalar.awk", "BEGIN {\n  x = 1\n  x[1] = 2\n}\n");
    let param = write("param.awk", "function f(a) {\n  return a + 1\n}\nBEGIN { z[1]; f(z) }\n");
    let unknown = write("unknown.awk", "BEGIN {\n  print 1\n  undefined_fn(1)\n}\n");
    let syntax = write("syntax.awk", "BEGIN {\n  print 1 +\n}\n");
    let cases: Vec<(Vec<&str>, i32, String)> = vec![
        (
            vec!["-f", &split],
            2,
            format!(
                "zawk: fatal: {}:2:7: function `split` expects 2 to 4 arguments, got 5\n      n = split(s, arr, \" \", x, y)\n          ^\n",
                split
            ),
        ),
        (
            vec!["-f", &substr],
            2,
            format!(
                "zawk: fatal: {}:2:6: function `substr` expects 2 or 3 arguments, got 1\n    \tx = substr(\"abc\")\n    \t    ^\n",
                substr
            ),
        ),
        (
            vec!["-f", &array],
            2,
            format!("zawk: fatal: {}:3:9: attempt to use array `a` in a scalar context\n      print a\n            ^\n", array),
        ),
        (
            vec!["-f", &scalar],
            2,
            format!("zawk: fatal: {}:3:3: attempt to use scalar `x` as an array\n", scalar),
        ),
        (
            vec!["-f", &param],
            2,
            format!(
                "zawk: fatal: {}:2:10: attempt to use array `a` (parameter of function `f`) in a scalar context\n",
                param
            ),
        ),
        // Lines count from the start of the file the error is in.
        (
            vec!["-f", &ok, "-f", &unknown],
            2,
            format!("zawk: fatal: {}:3:3: Call to unknown function \"undefined_fn\"\n", unknown),
        ),
        (
            vec!["-f", &ok, "-f", &syntax],
            1,
            format!("zawk: syntax error at {}:2:12: unexpected newline\n", syntax),
        ),
        // With other sources, `-e` sources are named `-e#N`, counting `-e` options only.
        (
            vec!["-e", "BEGIN { x = 1 }", "-e", "BEGIN { x[1] = 2 }"],
            2,
            "zawk: fatal: -e#2:1:9: attempt to use scalar `x` as an array\n".into(),
        ),
        (
            vec!["-f", &ok, "-e", "BEGIN { print 1 +\n}"],
            1,
            "zawk: syntax error at -e#1:1:18: unexpected newline\n".into(),
        ),
        (
            vec!["-e", "BEGIN { undefined_fn() }"],
            2,
            "zawk: fatal: line 1, column 9: Call to unknown function \"undefined_fn\"\n".into(),
        ),
        (
            vec!["BEGIN { print length(1, 2) }"],
            2,
            "zawk: fatal: line 1, column 15: function `length` expects 0 or 1 argument, got 2\n".into(),
        ),
        (
            vec!["BEGIN { sub(/a/) }"],
            2,
            "zawk: fatal: line 1, column 9: function `sub` expects 2 or 3 arguments, got 1\n".into(),
        ),
        (
            vec!["BEGIN { print mktime() }"],
            2,
            "zawk: fatal: line 1, column 15: function `mktime` expects 1 to 3 arguments, got 0\n".into(),
        ),
        (
            vec!["BEGIN { print mktime(1, 2, 3, 4) }"],
            2,
            "zawk: fatal: line 1, column 15: function `mktime` expects 1 to 3 arguments, got 4\n".into(),
        ),
        (
            vec!["BEGIN { print seq(3) + 1 }"],
            2,
            "zawk: fatal: line 1, column 15: attempt to use array returned by `seq()` in a scalar context\n".into(),
        ),
    ];
    for backend_arg in BACKEND_ARGS {
        for (args, code, expected) in &cases {
            let output = Command::cargo_bin("zawk")
                .unwrap()
                .env_remove("ZAWK_DEBUG")
                .arg(String::from(*backend_arg))
                .args(args)
                .output()
                .unwrap();
            let stderr = strip_source_locations(&String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.status.code(), Some(*code), "{} {:?}: {}", backend_arg, args, stderr);
            assert!(stderr.starts_with(expected.as_str()), "{} {:?}: {}", backend_arg, args, stderr);
        }
    }
}

/// Removes the `[src/file.rs:1:2] ` prefixes of error messages.
fn strip_source_locations(s: &str) -> String {
    let mut res = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("[src/") {
        res.push_str(&rest[..i]);
        rest = match rest[i..].find("] ") {
            Some(j) => &rest[i + j + 2..],
            None => &rest[i..],
        };
    }
    res.push_str(rest);
    res
}

// Run `prog` with each backend and check that it fails with exit code 2 and a stderr containing
// `expected`.
fn assert_fatal(prog: &str, expected: &str) {
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{} {}: {}", backend_arg, prog, stderr);
        assert!(stderr.contains(expected), "{} {}: {}", backend_arg, prog, stderr);
    }
}

#[test]
fn deep_recursion() {
    // The cranelift backend recurses on the native stack: programs run on a large stack, and
    // running out of it is a fatal error rather than an abort.
    let rec = "function rec(n) { if (n == 0) return 0; return 1 + rec(n - 1) }";
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(format!("{} BEGIN {{ print rec(1000000) }}", rec))
            .assert()
            .success()
            .stdout("1000000\n");
    }
    let output = Command::cargo_bin("zawk")
        .unwrap()
        .arg("-Bcranelift")
        .arg(format!(r#"{} BEGIN {{ print "start"; print rec(100000000) }}"#, rec))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "start\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("function call nesting too deep"), "{}", stderr);
}

#[test]
fn printf_width_and_precision_limits() {
    assert_fatal(r#"BEGIN { printf "%999999999999d", 1 }"#, "field width 999999999999 is too large");
    assert_fatal(r#"BEGIN { printf "%*d", 2^40, 1 }"#, "field width 1099511627776 is too large");
    assert_fatal(r#"BEGIN { x = sprintf("%-*s", 2^40, "a") }"#, "is too large");
    assert_fatal(r#"BEGIN { printf "%.999999999999f", 1 }"#, "precision 999999999999 is too large");
    // A string precision only truncates.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(r#"BEGIN { printf "%.999999999999s|%5s|\n", "abc", "x" }"#)
            .assert()
            .success()
            .stdout("abc|    x|\n");
    }
}

#[test]
fn stdlib_result_size_limits() {
    assert_fatal(r#"BEGIN { x = repeat("ab", 1e12) }"#, "repeat: result length 2000000000000 is too large");
    assert_fatal(r#"BEGIN { x = pad_end("a", 1e12, "*") }"#, "pad_end: length 1000000000000 is too large");
    assert_fatal(r#"BEGIN { x = pad_start("a", 1e12, "*") }"#, "pad_start: length");
    assert_fatal(r#"BEGIN { x = pad("a", 1e12, "*") }"#, "pad: length");
    assert_fatal(r#"BEGIN { a = seq(1, 1, 1e12) }"#, "seq: element count 1000000000000 is too large");
    assert_fatal(r#"BEGIN { a = seq(1, 0, 10) }"#, "seq: step 0 must be positive");
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(r#"BEGIN { a = seq(10, 0, 1); print length(repeat("ab", 3)), pad_end("a", 3, "*"), length(a) }"#)
            .assert()
            .success()
            .stdout("6 a** 0\n");
    }
}

#[test]
fn stdlib_failures_return_empty_with_warning() {
    let prog = r#"BEGIN { print "[" decode("base64", "!!!") "][" encode("nosuch", "x") "][" fend("1/0") "][" eval("1+") "][" digest("nosuch", "x") "]";
                 print decode("base64", encode("base64", "hi")), fend("1+2") }"#;
    for backend_arg in BACKEND_ARGS {
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{}: {}", backend_arg, stderr);
        assert_eq!(String::from_utf8_lossy(&output.stdout), "[][][][0][]\nhi 3\n", "{}", backend_arg);
        for expected in [
            "zawk: warning: decode: base64:",
            r#"zawk: warning: encode: unsupported format "nosuch""#,
            r#"zawk: warning: fend: "1/0": division by zero"#,
            r#"zawk: warning: eval: "1+":"#,
            r#"zawk: warning: digest: unsupported algorithm "nosuch""#,
        ] {
            assert!(stderr.contains(expected), "{}: {}", backend_arg, stderr);
        }
    }
}

#[test]
fn array_scalar_errors_name_the_variable() {
    assert_fatal(
        "function f(a) { return a + 1 } BEGIN { x[1] = 1; f(x) }",
        "attempt to use array `a` (parameter of function `f`) in a scalar context",
    );
    assert_fatal(
        "function g(a) { return a[1] } BEGIN { print g(1) }",
        "attempt to use scalar `a` (parameter of function `g`) as an array",
    );
    assert_fatal("BEGIN { x[1] = 1; print x }", "attempt to use array `x` in a scalar context");
    assert_fatal("BEGIN { x = 1; x[1] = 2 }", "attempt to use scalar `x` as an array");
    // Assigning a scalar to an array variable.
    assert_fatal("BEGIN { x[1] = 1; x = 2; print x }", "attempt to use array `x` in a scalar context");
    assert_fatal(
        r#"BEGIN { split("a", x); y = "s"; x = y }"#,
        "attempt to use array `x` in a scalar context",
    );
    assert_fatal(
        "function f(a) { a[1] = 1; a = 3 } BEGIN { f(z) }",
        "attempt to use array `a` (parameter of function `f`) in a scalar context",
    );
    assert_fatal(
        "BEGIN { print seq(1, 10) }",
        "attempt to use array returned by `seq()` in a scalar context",
    );
    assert_fatal(
        r#"BEGIN { print from_json("{bad") }"#,
        "attempt to use array returned by `from_json()` in a scalar context",
    );
    assert_fatal("BEGIN { a[1][2] = 3 }", "arrays of arrays such as `a[i][j]` are not supported");
    assert_fatal(
        "BEGIN { print 2--1 }",
        "syntax error: the operand of `--` must be a variable, an array element or a field",
    );
}

#[test]
fn functions_subcommand_lists_builtins() {
    let out = Command::cargo_bin("zawk").unwrap().args(["functions", "--json"]).output().unwrap();
    assert!(out.status.success());
    let docs: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let docs = docs.as_array().unwrap();
    let trim = docs.iter().find(|f| f["name"] == "trim").unwrap();
    assert_eq!(trim["returns"], "str");
    assert_eq!(trim["params"][0]["optional"], false);
    assert_eq!(trim["params"][1]["name"], "chars");
    assert_eq!(trim["params"][1]["optional"], true);
    // Every example compiles: it is guarded by a condition that is false at runtime, so that
    // functions using the network, databases, ... are not called.
    let mut failures = vec![];
    for f in docs {
        let example = f["example"].as_str().unwrap();
        let prog = format!("BEGIN {{ if (ARGC > 100) {{ {} }} }}", example);
        let out = Command::cargo_bin("zawk").unwrap().arg("-Binterp").arg(&prog).output().unwrap();
        if !out.status.success() {
            failures.push(format!("{}: {}", example, String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    assert!(failures.is_empty(), "examples that do not compile:\n{}", failures.join("\n"));
    let out = Command::cargo_bin("zawk").unwrap().args(["functions", "Bitwise"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("# Bitwise\n") && text.contains("xor(a: int, b: int) -> int"));
}

// Builtins with side effects run when their result is unused, and pure expressions used as
// statements compile.
#[test]
fn builtins_as_statements() {
    let prog = r#"BEGIN {
        bf_icontains("b"); print bf_contains("b"), bf_contains("c"), bf_icontains("b")
        !x; -x; +x; $1
        if (ARGC > 100) {
            http_get("http://127.0.0.1:9/"); http_post("http://127.0.0.1:9/", "x")
            r = http_get("http://127.0.0.1:9/"); print r["status"], http_post("http://127.0.0.1:9/")["status"]
            sqlite_execute("a.db", "delete from t"); sqlite_query("a.db", "select 1")
            jev($0, "q"); bf_icontains("d")
        }
        print "ok"
    }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(prog)
            .assert()
            .success()
            .stdout("1 0 1\nok\n");
    }
}

// Every kind of expression, of every scalar type, compiles when used as a statement.
#[test]
fn expressions_as_statements() {
    let exprs = [
        "1", "-1", "1.5", "\"s\"", "i", "f", "s", "-i", "!f", "!s", "i + 1", "f * 2.5",
        "s \"t\"", "i < f", "length(s)", "substr(s, 1)", "sqrt(f)", "int(f)", "(i, s) in m",
        "m[s]", "i ? f : s", "i++", "f += 1", "s = s \"x\"",
    ];
    for expr in exprs {
        let prog = format!(r#"BEGIN {{ i = 2; f = 2.5; s = "a"; m["k"] = 1; {expr}; {expr}
            print "ok" }}"#);
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(*backend_arg)
                .arg(&prog)
                .assert()
                .success()
                .stdout("ok\n");
        }
    }
}

#[test]
fn single_byte_fs_unterminated_last_record() {
    // With a single-byte FS and an input that does not end in a newline, a program that does not
    // use every field must neither undercount NF on the last record nor yield an extra empty one.
    let cases: &[(&str, &str, &str)] = &[
        ("a,b", "{print $1}", "a\n"),
        ("a,b", "{print NF}", "2\n"),
        ("a,b", "{print $0, NF}", "a,b 2\n"),
        ("x,a,b", "{print $1; n++} END{print n}", "x\n1\n"),
        ("x,a,b", "{print $1, NF}", "x 3\n"),
        ("a,b\nc,d", "{print $1}", "a\nc\n"),
        ("a,b\nc,d", "{print NF}", "2\n2\n"),
        ("a,b\nc,d", "{print NR, NF, $0}", "1 2 a,b\n2 2 c,d\n"),
        ("a,b,", "{print NF}", "3\n"),
        ("a", "{print $1, NF}", "a 1\n"),
    ];
    for backend_arg in BACKEND_ARGS {
        for parallel in [None, Some("-pr")] {
            for (input, prog, expected) in cases {
                let mut cmd = Command::cargo_bin("zawk").unwrap();
                cmd.arg(backend_arg);
                if let Some(p) = parallel {
                    cmd.args([p, "-j2"]);
                }
                cmd.arg("-F,").arg(prog).write_stdin(*input).assert().success().stdout(*expected);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn output_files_beyond_fd_limit() {
    // With more output files than file descriptors, the least recently used files are closed and
    // later reopened for appending, so no output is lost (as in gawk).
    let zawk = assert_cmd::cargo::cargo_bin("zawk");
    for backend_arg in BACKEND_ARGS {
        let tmpdir = tempdir().unwrap();
        let prog = "BEGIN{for(i=0;i<300;i++) print \"a\" i > (\"f\" i)
                         for(i=0;i<300;i++) print \"b\" i > (\"f\" i)
                         close(\"f0\"); print \"c\" > \"f0\"}";
        let out = std::process::Command::new("sh")
            .current_dir(tmpdir.path())
            .arg("-c")
            .arg("ulimit -n 64 && exec \"$0\" \"$@\"")
            .arg(&zawk)
            .args([backend_arg, prog])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(read_to_string(tmpdir.path().join("f0")).unwrap(), "c\n");
        for i in 1..300 {
            let data = read_to_string(tmpdir.path().join(format!("f{i}"))).unwrap();
            assert_eq!(data, format!("a{i}\nb{i}\n"), "file f{i} ({backend_arg})");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn output_write_error_exit_code() {
    // /dev/full opens fine, but writes to it fail (asynchronously, in the writer thread): the
    // program must not exit successfully.
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([backend_arg, "BEGIN{print 1 > \"/dev/full\"}"])
            .assert()
            .code(2);
    }
}

#[cfg(unix)]
#[test]
fn stdout_broken_pipe_is_silent() {
    // `zawk ... | head -1`: once the reader of stdout goes away, zawk stops quietly and exits
    // with 141 (128 + SIGPIPE), as the shell reports for gawk killed by SIGPIPE.
    use std::io::{BufRead, BufReader};
    use std::process::{Command as StdCommand, Stdio};
    let prog = "BEGIN { for (i = 0; i < 10000000; i++) print i }";
    for args in [&["-Binterp"][..], &["-Bcranelift"], &["-Bcranelift", "-pr"]] {
        let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("zawk"))
            .args(args)
            .arg(prog)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "0\n");
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(141), "{:?}: {}", args, stderr);
        assert_eq!(stderr, "", "{:?}", args);
    }
    // Output to a command that exits early is still an error.
    let output = Command::cargo_bin("zawk")
        .unwrap()
        .arg(r#"BEGIN { for (i = 0; i < 1000000; i++) print i | "head -1" }"#)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Broken pipe"));
}

#[cfg(unix)]
#[test]
fn stdout_broken_pipe_under_load() {
    // Regression test: when the reader of stdout went away while writes were still pending on the
    // writer thread, tearing down the runtime could panic and drop it a second time (SIGSEGV,
    // SIGBUS or abort instead of 141). The race shows up mostly on a busy machine, so keep the
    // CPUs busy while running `zawk '{print}' file | head -1` repeatedly.
    use std::io::{BufRead, BufReader};
    use std::process::{Command as StdCommand, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let dir = tempdir().unwrap();
    let path = dir.path().join("big.csv");
    {
        let mut f = std::io::BufWriter::new(File::create(&path).unwrap());
        for i in 0..1_000_000 {
            writeln!(f, "{},{},abc,def", i, i * 2).unwrap();
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let n_busy = 2 * std::thread::available_parallelism().map_or(4, |n| n.get());
    let busy: Vec<_> = (0..n_busy)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    let mut failures = Vec::new();
    for _ in 0..20 {
        let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("zawk"))
            .arg("{print}")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if output.status.code() != Some(141) || !stderr.is_empty() {
            failures.push((output.status, stderr));
        }
    }
    stop.store(true, Ordering::Relaxed);
    for t in busy {
        t.join().unwrap();
    }
    assert!(failures.is_empty(), "{:?}", failures);
}

#[test]
fn ignorecase_parallel() {
    // IGNORECASE (set with -v or in BEGIN) applies on the worker threads of -p, to regexes,
    // string comparisons and index().
    let input: String = "ABC x\nabc y\nAbC z\nabd w\n".repeat(500);
    let prog = r#"/^abc/ { re++ } $1 == "abc" { eq++ } index($1, "B") { ix++ } END { print re, eq, ix }"#;
    for backend_arg in BACKEND_ARGS {
        for begin in [false, true] {
            let mut cmd = Command::cargo_bin("zawk").unwrap();
            cmd.arg(backend_arg).args(["-pr", "-j4"]);
            let prog = if begin {
                format!("BEGIN {{ IGNORECASE = 1 }} {}", prog)
            } else {
                cmd.args(["-v", "IGNORECASE=1"]);
                prog.to_string()
            };
            cmd.arg(prog).write_stdin(input.clone()).assert().success().stdout("1500 1500 2000\n");
        }
    }
}

#[test]
fn parallel_begin_values_counted_once() {
    // Every worker starts from the values BEGIN set; aggregating them must count those values
    // once, not once per worker, so these programs print the same in serial and parallel mode.
    let input: String = (1..=20000).map(|i| format!("{}\n", i)).collect();
    let cases = [
        (r#"BEGIN { n = 10 } { n++ } END { print n }"#, "20010\n"),
        (r#"BEGIN { t = 100 } { t -= 1 } END { print t }"#, "-19900\n"),
        (r#"BEGIN { f = 0.5 } { f += 1 } END { printf "%.1f\n", f }"#, "20000.5\n"),
        (r#"BEGIN { n = 10; s = "a" } { x++ } END { print n, s, x }"#, "10 a 20000\n"),
        (r#"BEGIN { a["x"] = 10; a["y"] = 5 } { a["x"]++ } END { print a["x"], a["y"], length(a) }"#, "20010 5 2\n"),
        (r#"BEGIN { s = "init" } NR == 5 { s = "set" } END { print s }"#, "set\n"),
        (r#"BEGIN { p[0] = 0 } { p[PID] = 1 } END { print (length(p) > 2) }"#, "1\n"),
    ];
    for backend_arg in BACKEND_ARGS {
        for (prog, expected) in cases {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(backend_arg)
                .args(["-pr", "-j4", "--chunk-size", "1024", prog])
                .write_stdin(input.clone())
                .assert()
                .success()
                .stdout(expected.to_string());
        }
    }
}

#[test]
fn lint_before_run() {
    // Errors: the report goes to stderr and the program does not run.
    let out = Command::cargo_bin("zawk")
        .unwrap()
        .args(["--lint", "BEGIN { print \"ran\"; print substring(\"abc\", 1) }"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let report = String::from_utf8(out.stderr).unwrap();
    assert!(report.contains("\"E301\""), "{}", report);

    // Warnings only: the report goes to stderr and the program runs.
    let out = Command::cargo_bin("zawk")
        .unwrap()
        .args(["--lint", "BEGIN { print NRR \"ran\" }"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "ran\n");
    assert!(String::from_utf8(out.stderr).unwrap().contains("\"W201\""));

    // --deny-warnings (implies --lint): warnings stop the run with exit code 1.
    let out = Command::cargo_bin("zawk")
        .unwrap()
        .args(["--deny-warnings", "BEGIN { print NRR \"ran\" }"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains("\"W201\""));

    // `zawk lint --deny-warnings`: warnings give exit code 1.
    Command::cargo_bin("zawk").unwrap().args(["lint", "BEGIN { print NRR }"]).assert().success();
    Command::cargo_bin("zawk")
        .unwrap()
        .args(["lint", "--deny-warnings", "BEGIN { print NRR }"])
        .assert()
        .code(1);
    Command::cargo_bin("zawk")
        .unwrap()
        .args(["--deny-warnings", "BEGIN { print 1 }"])
        .assert()
        .success()
        .stdout("1\n")
        .stderr("");

    // No diagnostics: nothing is printed to stderr; `-v` variables count as set.
    Command::cargo_bin("zawk")
        .unwrap()
        .args(["--lint", "-v", "x=1", "-e", "BEGIN { print x }"])
        .assert()
        .success()
        .stdout("1\n")
        .stderr("");
}

#[test]
fn inspect_files() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let inspect = |args: &[&str]| {
        let out = Command::cargo_bin("zawk").unwrap().arg("inspect").args(args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let csv = dir.join("demo.csv");
    let report = inspect(&[csv.to_str().unwrap()]);
    assert!(report.contains("Format:  csv (read with: zawk -i csv -H"), "{}", report);
    assert!(report.contains("│ $3    │ age  │ INT64  │"), "{}", report);
    assert!(report.contains("│ age         │ INT64       │ 8      │ 49    │             5 │ 27.8 │ 19.8796 │  10 │  24 │  48 │     5 │           0.00% │"), "{}", report);

    let jsonl = dir.join("demo.jsonl");
    let report = inspect(&["-o", "md", "-n", "2", jsonl.to_str().unwrap()]);
    assert!(report.contains("## Sample (first 2 rows)\n\n| id | name | age |\n| ---: | --- | ---: |\n| 1 | Jackie | 48 |\n"), "{}", report);

    let parquet = dir.join("types.parquet");
    let report = inspect(&["-o", "markdown", parquet.to_str().unwrap()]);
    assert!(report.contains("| $8 | amount | DECIMAL(10,2) |"), "{}", report);
    assert!(report.contains("| active | BOOLEAN | 0 | 1 | 2 |"), "{}", report);
}

#[test]
fn nullish_coalescing() {
    // `??` takes the right side when the left one is uninitialized, empty or zero, and binds
    // tighter than concatenation; the left side is evaluated once.
    let prog = r#"function f() { calls++; return "v" }
BEGIN {
    print name ?? "Guest"
    name = ""; print "Hello " name ?? "Guest"
    name = 0; print name ?? "Guest"
    name = "Bob"; print "Hello " name ?? "Guest"
    print x ?? y ?? "z"
    print 1 + n ?? 41
    print f() ?? "w", calls
    print "0" ?? "a", "0.0" ?? "b", "+0" ?? "c", " -0 " ?? "d", "0x0" ?? "e", 0.5 ?? "f"
}
{ print $1 ?? "none", $2 ?? "none", $3 ?? "none" }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(prog)
            .write_stdin("0 abc\n")
            .assert()
            .stdout("Guest\nHello Guest\nGuest\nHello Bob\nz\n42\nv 1\na b c d e 0.5\nnone abc none\n");
    }
}

#[test]
fn nullish_coalescing_operands() {
    // The right operand may have a sign, `!` or `++`; the left one may be a postfix increment;
    // `??` binds tighter than unary minus and `^`; an array element on the left is not created.
    let prog = r#"function f(arr) { return arr["k"] ?? "def" }
BEGIN {
    print i++ ?? "z", i
    print u ?? x++, x
    print u ?? -1, u ?? !y, u ?? ++k, k
    a[1]; print a[2] ?? "missing", length(a)
    a[3] = 0; print a[3] ?? "zero", length(a)
    a[4] = "v"; print a[4] ?? "d"
    print -u ?? 5, 1 + u ?? 5, u ?? 2 ^ 2
    print f(m), length(m); m["k"] = "hi"; print f(m)
    print m["a", "b"] ?? "multi", length(m)
}"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(prog)
            .assert()
            .stdout("z 1\n0 1\n-1 1 1 1\nmissing 1\nzero 2\nv\n-5 6 4\ndef 0\nhi\nmulti 1\n");
    }
}

#[test]
fn hex_strings_are_numbers_everywhere() {
    // One string-to-number conversion: arithmetic, comparisons, truth values, printf and sorting
    // all read `0x` prefixed input (with an optional sign) as hexadecimal; there are no `0o`/`0b`
    // prefixes.
    let prog = r#"{
    print $1+0, -$1, int($1), $2+0, ($1==16), ($1>15), ($2<0), ($3==0)
    printf "%d %x %.1f %c\n", $1, $4, $2, $5
    print ($6 ? "t" : "f"), "0x8000000000000000"+0, "0x-5"+0, $7+0, float("0b101"), float("0o17")
    a[1] = $1; a[2] = "9"; a[3] = $4; n = asort(a); print a[1], a[2], a[3]
}"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(prog)
            .write_stdin("0x10 -0x10 0x0 0x1A 0x41 0X0 0b101\n")
            .assert()
            .stdout("16 -16 16 -16 1 1 1 1\n16 1a -16.0 A\nf 9223372036854775808 0 0 0 0\n9 0x10 0x1A\n");
    }
}

#[test]
fn default_fs_blanks_are_the_same_everywhere() {
    // With the default FS, `\r` separates fields (so a CRLF line ending is not part of the last
    // field) whether the record is main input, read by getline, assigned to $0 or split().
    let dir = tempdir().unwrap();
    let crlf = dir.path().join("crlf.txt");
    std::fs::write(&crlf, "a b\r\n").unwrap();
    let f = crlf.to_str().unwrap().replace('\\', "\\\\");
    let prog = format!(
        r#"BEGIN {{
    getline < "{f}"; print NF, length($2)
    "cat {f}" | getline; print NF, length($2)
}}
{{
    print NF, length($2)
    n = split($0, p); print n, length(p[2])
    $0 = $0; print NF, length($2)
    n = split(" \tx\r\ny  ", q, " ", s); print n, q[1] q[2], length(s[0]), length(s[1]), length(s[2])
    n = split("a::b:", r, ":"); print n, r[1] r[3], length(r[4]), split("", r, ":")
}}"#
    );
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(&prog)
            .arg(&crlf)
            .assert()
            .stdout("2 1\n2 1\n2 1\n2 1\n2 1\n2 xy 2 2 2\n4 ab 0 0\n");
    }
}

#[test]
fn stdlib_bad_arguments_do_not_panic() {
    // Arguments that used to panic (or abort on a huge allocation) inside stdlib functions.
    let cases = [
        (r#"BEGIN { print length(sprintf("%.*f", 70000, 1)) }"#, "70002\n"),
        (r#"BEGIN { print length(parse("a b", "{x} {y")), length(parse("", "{")) }"#, "0 0\n"),
        (r#"BEGIN { m = variant("("); print "[" m["name"] "][" m["value"] "]" }"#, "[][]\n"),
        (r#"BEGIN { m = variant(" week(5) "); print m["name"], m["value"] }"#, "week 5\n"),
        (r#"BEGIN { print length(hex2rgb("\xff\xfe")) }"#, "0\n"),
    ];
    for backend_arg in BACKEND_ARGS {
        for (prog, expected) in cases {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(backend_arg)
                .arg(prog)
                .assert()
                .success()
                .stdout(expected.to_string());
        }
        let out = Command::cargo_bin("zawk")
            .unwrap()
            .arg(backend_arg)
            .arg("BEGIN { print mkpass(1e19) }")
            .assert()
            .code(2)
            .get_output()
            .clone();
        assert!(String::from_utf8_lossy(&out.stderr).contains("mkpass: length"));
    }
}

#[test]
fn char_at_out_of_range_is_empty() {
    let prog = r#"BEGIN { print "[" char_at("abc", 0) "]" char_at("abc", 1) "[" char_at("abc", -1) "]" char_at("abc", 3) "[" char_at("abc", 4) "]" }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, prog])
            .assert()
            .success()
            .stdout("[]a[]c[]\n");
    }
}

#[test]
fn index_with_regex_literal_is_error() {
    // `index(s, /re/)` used to search for "0" or "1" (the value of `$0 ~ /re/`) and return 0.
    for prog in [r#"BEGIN { print index("abc", /b/) }"#, r#"BEGIN { print last_index("abc", /b/) }"#] {
        for backend_arg in BACKEND_ARGS {
            let out = Command::cargo_bin("zawk")
                .unwrap()
                .args([*backend_arg, prog])
                .assert()
                .failure()
                .stdout("")
                .get_output()
                .clone();
            assert!(String::from_utf8_lossy(&out.stderr).contains("regexp constant as second argument is not allowed"));
        }
    }
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, r#"BEGIN { print index("abc", "b"), match("abc", /b/) }"#])
            .assert()
            .success()
            .stdout("2 2\n");
    }
}

#[test]
fn float_literals_with_trailing_dot() {
    let prog = r#"BEGIN { print 1., 0., 3.e1, -1., 1.E+2, .5, 1.+1 }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, prog])
            .assert()
            .success()
            .stdout("1 0 30 -1 100 0.5 2\n");
    }
}

#[test]
fn uninitialized_values_are_strnums() {
    // Missing elements and unassigned variables compare as 0 with numbers and as "" with strings,
    // whatever the type of the array or variable; copies keep that, other strings do not.
    let cases = [
        (r#"BEGIN { a["j"] = "s"; print (a["k"] == 0), (a["k"] == ""), (a["k"] < "a") }"#, "1 1 1\n"),
        (r#"BEGIN { n = split("", arr); print (arr[1] == 0) }"#, "1\n"),
        (r#"BEGIN { if (0) x = "a"; y = x; print (x == 0), (y == 0), (("" x) == 0) }"#, "1 1 0\n"),
        (r#"BEGIN { getline x < "/dev/null"; print (x == 0) }"#, "1\n"),
        (r#"BEGIN { x = "10"; a["j"] = "10"; print (x < 9), (a["j"] < 9) }"#, "1 1\n"),
        (r#"BEGIN { a[1] = 5; b = a[2]; c = b; print "[" b "]" "[" c "]", (b == 0) }"#, "[][] 1\n"),
        (r#"BEGIN { a[1] = 5; for (i = 1; i <= 2; i++) { b = a[i]; print "[" b "]" } }"#, "[5]\n[]\n"),
        (r#"function f(n) { if (n) return n * 2 } BEGIN { x = f(0); print "[" f(3) "]" "[" f(0) "]" "[" x "]" }"#, "[6][][]\n"),
        (r#"function f(n) { if (n > 0) { f(n - 1); return } return n } BEGIN { print "[" f(2) "]" "[" f(0) "]" }"#, "[][0]\n"),
    ];
    for (prog, expected) in cases {
        for backend_arg in BACKEND_ARGS {
            Command::cargo_bin("zawk")
                .unwrap()
                .args([*backend_arg, prog])
                .assert()
                .success()
                .stdout(expected);
        }
    }
}

#[test]
fn to_json_keeps_empty_values_and_sorts_keys() {
    let prog = r#"BEGIN { a["k"] = "v"; a["e"] = ""; print to_json(a); for (i = 2; i >= 0; i--) b["k" i] = i; print to_json(b) }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, prog])
            .assert()
            .success()
            .stdout("{\"e\":\"\",\"k\":\"v\"}\n{\"k0\":0,\"k1\":1,\"k2\":2}\n");
    }
}

#[test]
fn json_values_are_decoded() {
    let prog = r#"BEGIN {
        x = from_json("[1,null,\"s\"]"); print "[" x[2] "]", length(x[2])
        o = from_json("{\"n\":null}"); print ("n" in o), length(o["n"])
        print json_value("{\"a\":\"line1\\nline2 \\\"q\\\"\"}", "$.a")
        print "[" json_value("{\"a\":null}", "$.a") "]", json_value("{\"a\":[1,2]}", "$.a")
        q = json_query("{\"b\":[{\"n\":\"x\\ty\"},{\"n\":2}]}", "$.b[*].n"); print q[1] "|" q[2]
    }"#;
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .args([*backend_arg, prog])
            .assert()
            .success()
            .stdout("[] 0\n1 0\nline1\nline2 \"q\"\n[] [1,2]\nx\ty|2\n");
    }
}

#[test]
fn for_in_order_is_reproducible() {
    let prog = r#"{ a[$1]; b[NR] = $1 } END { for (k in a) printf "%s", k; for (k in b) printf " %s", k; print "" }"#;
    let input = "x\ny\nz\nw\nv\nfoo\nbar\nbaz\n";
    let run = |backend_arg: &str| {
        let out = Command::cargo_bin("zawk").unwrap().args([backend_arg, prog]).write_stdin(input).output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let expected = run(BACKEND_ARGS[0]);
    for backend_arg in BACKEND_ARGS {
        for _ in 0..5 {
            assert_eq!(run(backend_arg), expected, "backend {}", backend_arg);
        }
    }
}
