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
        // close() returns the exit status of a command, 0 for files and input commands, and -1
        // for names that were never opened.
        let tmp = tempdir().unwrap();
        let out = tmp.path().join("out.txt");
        let prog = format!(
            r#"BEGIN {{
                print "x" | "cat >/dev/null; exit 3"; print close("cat >/dev/null; exit 3")
                print close("never opened")
                print "y" > "{0}"; print close("{0}")
                "echo hi" | getline v; print close("echo hi")
            }}"#,
            out.to_str().unwrap()
        );
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog)
            .assert()
            .success()
            .stdout(String::from("3\n-1\n0\n0\n"));
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
        // `getline < file` and `cmd | getline` still return -1 rather than failing.
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
            .stdout("-1\n-1\n");
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
            .stdout("\"x,y\",\"a,b\"\r\n");
    }
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
