//! Compile time of the cranelift backend must stay roughly linear in the size of the program: a
//! few hundred lines of awk (reports, log parsing) used to take seconds to JIT.
use assert_cmd::Command;
use std::time::{Duration, Instant};

const N: usize = 400;

/// Generous enough for slow CI machines; before the fix these programs took 5-11s in release.
fn budget() -> Duration {
    if cfg!(debug_assertions) {
        Duration::from_secs(15)
    } else {
        Duration::from_secs(1)
    }
}

fn line(i: usize, src: &str) -> String {
    format!(
        "s{i} = substr({src}, 2, 3); t{i} = toupper(s{i}); if (t{i} ~ /EL/) a[{i}] = t{i}; n += length(a[{i}])"
    )
}

fn run(prog: &str, backend: &str) -> (String, Duration) {
    let start = Instant::now();
    let out = Command::cargo_bin("zawk")
        .unwrap()
        .arg(backend)
        .arg(prog)
        .write_stdin("hello\n")
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    (String::from_utf8(out.stdout).unwrap(), elapsed)
}

fn check(prog: &str, expected: &str) {
    let (interp, _) = run(prog, "-Binterp");
    assert_eq!(interp, expected);
    let (cranelift, elapsed) = run(prog, "-Bcranelift");
    assert_eq!(cranelift, expected);
    assert!(
        elapsed < budget(),
        "cranelift took {elapsed:?} for a {N}-line program (budget {:?})",
        budget()
    );
}

#[test]
fn long_begin_block() {
    let body: Vec<_> = (0..N).map(|i| line(i, "\"hello\"")).collect();
    let prog = format!("BEGIN {{\n{}\nprint n\n}}", body.join("\n"));
    check(&prog, &format!("{}\n", 3 * N));
}

#[test]
fn many_rules() {
    let rules: Vec<_> = (0..N).map(|i| format!("NR==1 {{ {} }}", line(i, "$0"))).collect();
    let prog = format!("{}\nEND {{ print n }}", rules.join("\n"));
    check(&prog, &format!("{}\n", 3 * N));
}
