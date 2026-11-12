//! Safe automatic parallelism: without `-p`, an aggregation over large local files runs as with
//! `-pr` and prints what the serial run prints. `ZAWK_PARALLEL_MIN_SIZE=1` lowers the size
//! threshold (64 MiB) so that small test files qualify.
use assert_cmd::Command;
use std::io::Write;
use tempfile::tempdir;

const BACKEND_ARGS: &[&str] = &["-Binterp", "-Bcranelift"];

fn zawk(env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let mut cmd = Command::cargo_bin("zawk").unwrap();
    cmd.env_remove("ZAWK_PARALLEL")
        .env_remove("ZAWK_PARALLEL_MIN_SIZE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args).output().unwrap()
}

fn sorted_lines(bs: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(bs)
        .lines()
        .map(String::from)
        .collect();
    lines.sort();
    lines
}

/// The number of functions in the bytecode of the program as zawk would run it: a parallel
/// program has its BEGIN, main loop and END compiled apart, a serial one in one function, so it
/// tells which strategy was picked.
fn bytecode(env: &[(&str, &str)], extra: &[&str], prog: &str, file: &str) -> usize {
    let mut args = extra.to_vec();
    args.extend(["--dump-bytecode", "-F,", prog, file]);
    let out = zawk(env, &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("function "))
        .count()
}

fn data_file(dir: &std::path::Path) -> String {
    let path = dir.join("data.csv");
    let mut f = std::fs::File::create(&path).unwrap();
    for i in 0..3000u64 {
        let word = ["alpha", "beta", "ERROR", "WARN", "INFO"][(i * 7 % 5) as usize];
        writeln!(
            f,
            "{},{},{}.{},{},user{},a-b-{}",
            i,
            word,
            i * 37 % 1000,
            i % 10,
            i % 97,
            i % 50,
            i % 3
        )
        .unwrap();
    }
    path.to_str().unwrap().to_string()
}

#[test]
fn aggregations_run_in_parallel_with_the_serial_result() {
    let tmp = tempdir().unwrap();
    let file = data_file(tmp.path());
    let small = [("ZAWK_PARALLEL_MIN_SIZE", "1")];
    for prog in [
        "{ s += int($4) } END { print s }",
        "BEGIN { PROCINFO[\"sorted_in\"] = \"@ind_str_asc\" } { a[$2] += int($3) } END { for (k in a) print k, a[k] }",
        "{ c[$5]++ } END { print length(c) }",
        "/ERROR/ { c++ } END { print c, NR }",
        "{ n = split($6, p, \"-\"); c[p[3]] += n } END { PROCINFO[\"sorted_in\"] = \"@val_num_desc\"; for (k in c) print k, c[k] }",
        "{ x = int($3 * 2); s += x } END { print s }",
        "BEGIN { t = 500 } $3 > t { hi++ } $3 <= t { lo++ } END { print hi, lo, NR, FNR, FILENAME }",
    ] {
        // It is picked: the program compiles as with `-pr`, not as without.
        let auto = bytecode(&small, &[], prog, &file);
        assert_eq!(auto, bytecode(&[], &["-pr"], prog, &file), "{}", prog);
        assert_ne!(auto, bytecode(&small, &["-p0"], prog, &file), "{}", prog);
        assert_ne!(
            auto,
            bytecode(&[("ZAWK_PARALLEL", "0")], &[], prog, &file),
            "{}",
            prog
        );
        // Below the threshold, the program runs serially.
        assert_ne!(auto, bytecode(&[], &[], prog, &file), "{}", prog);
        for backend in BACKEND_ARGS {
            let serial = zawk(&[], &[backend, "-F,", prog, &file]);
            // Small chunks share the records out among several workers.
            let parallel = zawk(
                &small,
                &[backend, "--chunk-size", "1024", "-F,", prog, &file],
            );
            assert!(
                serial.status.success() && parallel.status.success(),
                "{}",
                prog
            );
            // The same output, in the same order.
            assert_eq!(
                String::from_utf8_lossy(&serial.stdout),
                String::from_utf8_lossy(&parallel.stdout),
                "{}",
                prog
            );
            // No warning about running in parallel.
            assert!(
                parallel.stderr.is_empty(),
                "{}: {}",
                prog,
                String::from_utf8_lossy(&parallel.stderr)
            );
        }
    }
}

#[test]
fn order_dependent_programs_stay_serial() {
    let tmp = tempdir().unwrap();
    let file = data_file(tmp.path());
    let small = [("ZAWK_PARALLEL_MIN_SIZE", "1")];
    for prog in [
        "{ print $1 }",
        "NR == 1 { h = $2 } { s += int($4) } END { print h, s }",
        "{ m = $4 > m ? $4 : m } END { print m }",
        "{ last = $1 } END { print last }",
        "!seen[$2]++ { n++ } END { print n }",
        "BEGIN { s = 10 } { s += int($4) } END { print s }",
        "{ s += int($4) } END { print s, $0 }",
        // Sums of fractions depend on the order of the additions.
        "{ s += $4 } END { print s }",
        "{ x = $3 * 2; s += x } END { print s }",
        "{ a[$2] += $3 } END { for (k in a) print k, a[k] }",
        // `for (k in a)` in END over an array of the workers: another order than serially.
        "{ a[$2] += int($3) } END { for (k in a) print k, a[k] }",
        "{ x = $1 % 7; c[x]++ } END { for (k in c) print k, c[k] }",
    ] {
        assert_eq!(
            bytecode(&small, &[], prog, &file),
            bytecode(&[], &[], prog, &file),
            "{}",
            prog
        );
    }
    // `-v` gives an initial value, which each worker would add.
    let prog = "{ s += int($4) } END { print s }";
    assert_eq!(
        bytecode(&small, &["-v", "s=10"], prog, &file),
        bytecode(&[], &["-v", "s=10"], prog, &file)
    );
    // Standard input is not a regular file whose size is known.
    let out = Command::cargo_bin("zawk")
        .unwrap()
        .env("ZAWK_PARALLEL_MIN_SIZE", "1")
        .args(["-F,", prog])
        .write_stdin("1,a,2,3\n4,b,5,6\n")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "9\n");
}

/// The arrays combined from the workers are visited by `for (k in a)` in an order that depends
/// only on their keys: the same output (not only the same lines) from one run to the next, and
/// whatever `-j` and the chunk size, which change how the records are shared out.
#[test]
fn combined_arrays_iterate_in_a_fixed_order() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keys.csv");
    let mut f = std::fs::File::create(&path).unwrap();
    for i in 0..20000u64 {
        writeln!(f, "{},k{},{}", i, i * 7919 % 1500, i % 613).unwrap();
    }
    drop(f);
    let file = path.to_str().unwrap();
    for prog in [
        "{ c[$2]++ } END { for (k in c) print k, c[k] }",
        "{ n[$3] += 1 } END { for (k in n) print k, n[k] }",
        "{ s[$2] = \"v\" $2 } END { for (k in s) print k, s[k] }",
    ] {
        for backend in BACKEND_ARGS {
            let mut first: Option<Vec<u8>> = None;
            for (jobs, chunk) in [("2", "64"), ("8", "64"), ("3", "1000"), ("8", "100000")] {
                for _ in 0..3 {
                    let out = zawk(
                        &[],
                        &[
                            backend,
                            "-pr",
                            "-j",
                            jobs,
                            "--chunk-size",
                            chunk,
                            "-F,",
                            prog,
                            file,
                        ],
                    );
                    assert!(
                        out.status.success(),
                        "{}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    match &first {
                        None => first = Some(out.stdout),
                        Some(want) => assert!(
                            *want == out.stdout,
                            "{} {} -j {} --chunk-size {}: the order of the keys changed",
                            prog,
                            backend,
                            jobs,
                            chunk
                        ),
                    }
                }
            }
            // The same keys as the serial run (in their own order).
            let serial = zawk(&[], &[backend, "-p0", "-F,", prog, file]);
            assert_eq!(
                sorted_lines(first.as_ref().unwrap()),
                sorted_lines(&serial.stdout),
                "{}",
                prog
            );
        }
    }
}
