//! `-pr --keep-order`: the workers write stdout in the order of the input, so a filter or a
//! transformation prints exactly what the serial run prints. Small `--chunk-size` values make
//! many chunks, so that several workers take part even on small test inputs.
use assert_cmd::Command;
use std::io::Write;
use tempfile::tempdir;

const BACKEND_ARGS: &[&str] = &["-Binterp", "-Bcranelift"];

fn zawk(args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut cmd = Command::cargo_bin("zawk").unwrap();
    cmd.env_remove("ZAWK_PARALLEL")
        .env_remove("ZAWK_PARALLEL_MIN_SIZE");
    cmd.args(args);
    if let Some(input) = stdin {
        cmd.write_stdin(input.to_vec());
    }
    cmd.output().unwrap()
}

fn run_ok(args: &[&str], stdin: Option<&[u8]>) -> String {
    let out = zawk(args, stdin);
    assert!(
        out.status.success(),
        "zawk {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "zawk {:?}: unexpected stderr: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn write_file(dir: &std::path::Path, name: &str, rows: std::ops::Range<u64>) -> String {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    for i in rows {
        let word = ["alpha", "beta", "ERROR", "WARN", "INFO"][(i * 7 % 5) as usize];
        writeln!(
            f,
            "{},{},{}.{},{},user{},{}",
            i,
            word,
            i * 37 % 1000,
            i % 10,
            i % 97,
            i % 50,
            "x".repeat((i % 23) as usize)
        )
        .unwrap();
    }
    path.to_str().unwrap().to_string()
}

const PROGRAMS: &[&str] = &[
    "{ print }",
    "/ERROR/",
    "$4 > 50 { print $1, $2, $3 * 2 }",
    "{ printf \"%s|\", $1; printf \"%s\\n\", toupper($2) }",
    "{ $3 = \"\"; print }",
    "BEGIN { print \"start\" } $2 == \"WARN\" { print $1 } END { print \"end\" }",
    "{ for (i = 0; i < $1 % 3; i++) print $1, i }",
    "$1 % 100 == 0 { print FILENAME, $1 }",
];

#[test]
fn output_matches_serial_run() {
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..3000);
    for backend in BACKEND_ARGS {
        for prog in PROGRAMS {
            let serial = run_ok(&[backend, "-F,", prog, &file], None);
            assert!(!serial.is_empty());
            for chunk in ["64", "1000", "4096"] {
                let ordered = run_ok(
                    &[
                        backend,
                        "-pr",
                        "--keep-order",
                        "-j",
                        "4",
                        "--chunk-size",
                        chunk,
                        "-F,",
                        prog,
                        &file,
                    ],
                    None,
                );
                assert_eq!(
                    ordered, serial,
                    "{} --chunk-size {}: {}",
                    backend, chunk, prog
                );
            }
        }
    }
}

#[test]
fn input_paths_keep_order() {
    let tmp = tempdir().unwrap();
    let a = write_file(tmp.path(), "a.csv", 0..1500);
    let b = write_file(tmp.path(), "b.csv", 1500..2600);
    let data = std::fs::read(&a).unwrap();
    for backend in BACKEND_ARGS {
        // Several files (pread), a pipe (single reader), CSV (single reader with quote
        // prescan), whitespace and TSV splitting.
        let cases: Vec<(Vec<&str>, Option<&[u8]>)> = vec![
            (vec!["-F,", "{ print FILENAME, $1 }", &a, &b], None),
            (vec!["-F,", "{ print $2, $1 }"], Some(&data)),
            (vec!["-icsv", "{ print $1, $2 }", &a, &b], None),
            (vec!["{ print NF, $1 }", &a], None),
            (vec!["-itsv", "{ print $1 }", &b], None),
        ];
        for (args, stdin) in cases {
            let mut serial_args = vec![*backend];
            serial_args.extend(&args);
            let serial = run_ok(&serial_args, stdin);
            let mut par_args = vec![*backend, "--keep-order", "--chunk-size", "256"];
            par_args.extend(&args);
            assert_eq!(run_ok(&par_args, stdin), serial, "{:?}", par_args);
        }
    }
}

#[test]
fn large_output_per_chunk() {
    // Each chunk prints more than a write batch (64 KiB), which the worker holding the next
    // chunk sends before the chunk ends.
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..40000);
    let prog = "{ print $0, $0, $0; print $1 }";
    for backend in BACKEND_ARGS {
        let serial = run_ok(&[backend, "-F,", prog, &file], None);
        let ordered = run_ok(
            &[
                backend,
                "-pr",
                "--keep-order",
                "-j",
                "4",
                "--chunk-size",
                "131072",
                "-F,",
                prog,
                &file,
            ],
            None,
        );
        assert_eq!(ordered, serial, "{}", backend);
    }
}

#[test]
fn exit_keeps_order_of_what_is_printed() {
    // Which records the other workers still process once one calls `exit` is not defined, but
    // what is printed comes in the order of the input, followed by END.
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..3000);
    for backend in BACKEND_ARGS {
        let out = run_ok(
            &[
                backend,
                "-pr",
                "--keep-order",
                "-j",
                "4",
                "--chunk-size",
                "128",
                "-F,",
                "$1 == 1700 { exit } { print $1 } END { print \"end\" }",
                &file,
            ],
            None,
        );
        let mut lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.pop(), Some("end"), "{}", backend);
        let nums: Vec<u64> = lines.iter().map(|l| l.parse().unwrap()).collect();
        assert!(
            nums.windows(2).all(|w| w[0] < w[1]),
            "{}: {:?}",
            backend,
            nums
        );
        assert!(!nums.contains(&1700));
    }
}

#[test]
fn keep_order_needs_record_parallelism() {
    let out = zawk(&["-pf", "--keep-order", "{ print }", "/dev/null"], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--keep-order"));
    // Serial runs are ordered anyway.
    assert_eq!(
        run_ok(
            &["-p0", "--keep-order", "{ print $2 }"],
            Some(b"a b\nc d\n")
        ),
        "b\nd\n"
    );
}

#[test]
fn jobs_with_keep_order_alone() {
    // `--keep-order` implies `-pr`, so `-j` needs no `-p`.
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..2000);
    let serial = run_ok(&["-F,", "{ print $1 }", &file], None);
    let ordered = run_ok(
        &[
            "--keep-order",
            "-j",
            "4",
            "--chunk-size",
            "1000",
            "-F,",
            "{ print $1 }",
            &file,
        ],
        None,
    );
    assert_eq!(ordered, serial);
    // Without either, `-j` is still refused.
    assert!(
        !zawk(&["-j", "4", "{ print }", &file], None)
            .status
            .success()
    );
}

#[test]
fn range_pattern_runs_serially() {
    // Each worker would keep its own range state; `--keep-order` runs the program serially.
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..3000);
    let prog = "$1 == 10, $1 == 2990 { print $1 }";
    for backend in BACKEND_ARGS {
        let serial = run_ok(&[backend, "-F,", prog, &file], None);
        assert_eq!(serial.lines().count(), 2981);
        let out = zawk(
            &[
                backend,
                "--keep-order",
                "--chunk-size",
                "1000",
                "-F,",
                prog,
                &file,
            ],
            None,
        );
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), serial);
        assert!(String::from_utf8_lossy(&out.stderr).contains("running serially"));
        // An explicit `-pr` keeps running in parallel, with a W304 warning.
        let out = zawk(
            &[backend, "-pr", "--chunk-size", "1000", "-F,", prog, &file],
            None,
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("W304"));
    }
}

#[test]
fn cross_record_state_warns() {
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..100);
    // Record counters (W301) and state carried between records (W305).
    for (prog, code) in [
        ("NR % 10 == 0 { print NR }", "W301"),
        ("!seen[$2]++ { print $2 }", "W305"),
    ] {
        let out = zawk(&["--keep-order", "-F,", prog, &file], None);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--keep-order: line 1, column") && err.contains(code),
            "{}",
            err
        );
    }
    // A plain filter gets no warning.
    run_ok(
        &["--keep-order", "-F,", "$2 == \"WARN\" { print $1 }", &file],
        None,
    );
}

#[test]
fn slow_chunk_keeps_order() {
    // The chunks after a slow one wait once their held output is large; the output is unchanged.
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..200000);
    let prog = "$1 == 1 { system(\"sleep 1\") } { print $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0, $0 }";
    let serial = run_ok(&["-F,", prog, &file], None);
    assert!(serial.len() > 64 << 20);
    let ordered = run_ok(&["--keep-order", "-j", "4", "-F,", prog, &file], None);
    assert!(ordered == serial);
}

#[test]
fn chunk_read_in_begin_keeps_order() {
    // The main thread reads the first chunk before the parallel stage starts (the `-H` header,
    // the jsonl schema, a `getline` in BEGIN): that chunk's output still comes first.
    let tmp = tempdir().unwrap();
    let file = write_file(tmp.path(), "data.csv", 0..20000);
    let jsonl = tmp.path().join("data.jsonl");
    {
        let mut f = std::fs::File::create(&jsonl).unwrap();
        for i in 0..20000 {
            writeln!(f, "{{\"id\":{},\"name\":\"n{}\"}}", i, i).unwrap();
        }
    }
    let jsonl = jsonl.to_str().unwrap();
    for backend in BACKEND_ARGS {
        let cases: Vec<Vec<&str>> = vec![
            vec!["-H", "-F,", "{ print $1, $2 }", &file],
            vec!["-icsv", "-H", "{ print $1, $2 }", &file],
            vec!["-ijsonl", "{ print $FI[\"id\"] }", jsonl],
            vec!["-F,", "BEGIN { getline; h = $1 } { print h, $1 }", &file],
            // BEGIN reads past the first chunks.
            vec![
                "-F,",
                "BEGIN { for (i = 0; i < 500; i++) getline; print \"begin\", $1 } { print $1 }",
                &file,
            ],
        ];
        for args in cases {
            let mut serial_args = vec![*backend];
            serial_args.extend(&args);
            let serial = run_ok(&serial_args, None);
            for chunk in ["256", "4096"] {
                let mut par_args =
                    vec![*backend, "--keep-order", "-j", "4", "--chunk-size", chunk];
                par_args.extend(&args);
                assert_eq!(run_ok(&par_args, None), serial, "{:?}", par_args);
            }
        }
    }
}
