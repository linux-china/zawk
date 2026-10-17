use assert_cmd::Command;
use rand::seq::SliceRandom;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

fn numbers_str(n: usize) -> (String, String) {
    let mut input = String::new();
    let mut sorted = String::new();
    let mut vs: Vec<_> = (0..n).collect();
    vs.shuffle(&mut rand::rng());
    for (i, n) in vs.into_iter().enumerate() {
        sorted.push_str(format!("{}\n", i).as_str());
        input.push_str(format!("{}\n", n).as_str());
    }
    (input, sorted)
}

const N: usize = 10_000;

const BACKEND_ARGS: &[&str] = &["-Binterp", "-Bcranelift"];

#[cfg(not(target_os = "windows"))]
#[test]
fn sort_command_single_threaded() {
    let (input, expected) = numbers_str(N);
    let tmpdir = tempdir().unwrap();
    let data_fname = tmpdir.path().join("numbers");
    {
        let mut file = File::create(data_fname.clone()).unwrap();
        file.write_all(input.as_bytes()).unwrap();
    }
    let prog: String = r#"{ print $0 | "sort -n"; }"#.into();
    for backend_arg in BACKEND_ARGS {
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(prog.clone())
            .arg(data_fname.clone().into_os_string().into_string().unwrap())
            .assert()
            .stdout(expected.clone());
    }
}

#[cfg(not(target_os = "windows"))]
#[test]
fn sort_command_multi_threaded() {
    let (input, expected) = numbers_str(N);
    let tmpdir = tempdir().unwrap();
    let data_fname = tmpdir.path().join("numbers");
    {
        let mut file = File::create(data_fname.clone()).unwrap();
        file.write_all(input.as_bytes()).unwrap();
    }
    let prog: String = r#"{ print $0 | "sort -n"; }"#.into();
    for backend_arg in BACKEND_ARGS {
        eprintln!("backend={:?}", backend_arg);
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(String::from(*backend_arg))
            .arg(String::from("-pr"))
            .arg(String::from("-j4"))
            .arg(prog.clone())
            .arg(data_fname.clone().into_os_string().into_string().unwrap())
            .assert()
            .stdout(expected.clone());
    }
}

#[test]
fn procinfo_sorted_in() {
    let input = "b 2\na 5\nc 1\n10 4\n9 3\n";
    let cases: &[(&str, &str)] = &[
        ("@ind_str_asc", "10 4\n9 3\na 5\nb 2\nc 1\n"),
        ("@ind_str_desc", "c 1\nb 2\na 5\n9 3\n10 4\n"),
        ("@ind_num_asc", "a 5\nb 2\nc 1\n9 3\n10 4\n"),
        ("@ind_num_desc", "10 4\n9 3\nc 1\nb 2\na 5\n"),
        ("@val_num_asc", "c 1\nb 2\n9 3\n10 4\na 5\n"),
        ("@val_num_desc", "a 5\n10 4\n9 3\nb 2\nc 1\n"),
        ("@val_str_asc", "c 1\nb 2\n9 3\n10 4\na 5\n"),
        ("@val_type_desc", "a 5\n10 4\n9 3\nb 2\nc 1\n"),
    ];
    for backend_arg in BACKEND_ARGS {
        for (order, expected) in cases {
            let prog = format!(
                r#"{{ c[$1] += $2 }} END {{ PROCINFO["sorted_in"] = "{}"; for (k in c) print k, c[k] }}"#,
                order
            );
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(*backend_arg)
                .arg(prog)
                .write_stdin(input)
                .assert()
                .stdout(expected.to_string());
        }
        // Integer keys, and switching back to the default order.
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(r#"BEGIN { a[10]; a[9]; a[100]; PROCINFO["sorted_in"] = "@ind_str_asc"; for (k in a) printf "%s ", k; PROCINFO["sorted_in"] = "@ind_num_asc"; for (k in a) printf "%s ", k; delete PROCINFO["sorted_in"]; n = 0; for (k in a) n++; print n }"#)
            .assert()
            .stdout("10 100 9 9 10 100 3\n");
        // PROCINFO set through a function parameter, with the loop in END (and in a
        // function that never mentions PROCINFO).
        Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(r#"function set(p) { p["sorted_in"] = "@ind_str_desc" } function show(arr, k) { for (k in arr) printf "%s", k; print "" } BEGIN { set(PROCINFO) } { a[$1] } END { show(a) }"#)
            .write_stdin("b\nc\na\n")
            .assert()
            .stdout("cba\n");
        // User-defined comparison functions are not supported: fail instead of ignoring them.
        let output = Command::cargo_bin("zawk")
            .unwrap()
            .arg(*backend_arg)
            .arg(r#"BEGIN { a[1]; PROCINFO["sorted_in"] = "my_cmp"; for (k in a) print k }"#)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("user-defined comparison functions"));
    }
}

#[test]
fn array_statistics() {
    let cases: &[(&str, &str, &str)] = &[
        // String values (fields) are converted to numbers.
        (
            "3\n1\n4\n1\n5\n9\n2\n6\n",
            r#"{ a[NR] = $1 } END { print _median(a), _quantile(a, 0.9), _var(a), _stddev(a), _mode(a), _count_distinct(a) }"#,
            "3.5 6.9 7.55357 2.74838 1 7\n",
        ),
        (
            "3\n1\n4\n1\n5\n9\n2\n6\n",
            r#"{ a[NR] = $1 + 0 } END { print _median(a), _quantile(a, 0.25), _quantile(a, 2) }"#,
            "3.5 1.75 9\n",
        ),
        // String keys; ties in _top are ordered by key.
        (
            "a\nb\na\nc\nb\na\nd\n",
            r#"{ c[$1]++ } END { t = _top(c, 3); for (i = 1; i <= length(t); i++) print t[i], c[t[i]]; print _count_distinct(c), length(_top(c, 0)), length(_top(c, 99)) }"#,
            "a 3\nb 2\nc 1\n3 0 4\n",
        ),
        ("", r#"BEGIN { print _median(e), _stddev(e), _mode(e), _count_distinct(e), length(_top(e, 3)) }"#, "0 0 0 0 0\n"),
        // _sum/_mean/_max/_min accept any array, not only map[int]num.
        (
            "a 1\nb -2.5\na 4\n",
            r#"{ a[NR] = $2; s[$1] += $2; i[NR] = int($2) } END { print _sum(a), _mean(a), _max(a), _min(a); print _sum(s), _mean(s), _max(s), _min(s); print _sum(i), _mean(i), _max(i), _min(i) }"#,
            "2.5 0.833333 4 -2.5\n2.5 1.25 5 -2.5\n3 1 4 -2\n",
        ),
        ("", r#"BEGIN { a[5] = -3; a[7] = -1; print _sum(e), _mean(e), _max(e), _min(e), _max(a), _min(a) }"#, "0 0 0 0 -1 -3\n"),
    ];
    for backend_arg in BACKEND_ARGS {
        for (input, prog, expected) in cases {
            Command::cargo_bin("zawk")
                .unwrap()
                .arg(*backend_arg)
                .arg(*prog)
                .write_stdin(*input)
                .assert()
                .stdout(expected.to_string());
        }
    }
}
