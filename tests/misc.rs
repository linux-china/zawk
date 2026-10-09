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
