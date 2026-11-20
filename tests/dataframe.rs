//! DataFrame functions (`df_*`): each program is run with the bytecode interpreter and with
//! Cranelift (at two optimization levels), which must print the same output; most cases also
//! check that the `df_*` version of a task prints what the classic awk array version prints.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

const BACKENDS: &[&[&str]] = &[&["-Binterp"], &["-Bcranelift", "-O0"], &["-Bcranelift", "-O3"]];

const ORDERS: &str = "\
id,user_id,city,status,amount
1,10,Paris,paid,12.5
2,11,Rome,paid,5
3,10,Paris,refunded,7
4,12,Oslo,paid,1
5,11,Rome,paid,10
6,13,Paris,paid,2.5
";

const USERS: &str = "\
id,name
10,Ann
11,Bob
12,Cy
14,Dee
";

fn data_dir() -> TempDir {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("orders.csv"), ORDERS).unwrap();
    fs::write(dir.path().join("users.csv"), USERS).unwrap();
    dir
}

/// Runs `zawk args...` in `dir` with every backend; returns (stdout, stderr) after checking
/// that the backends agree and the run succeeded.
fn run_in(dir: &Path, args: &[&str]) -> (String, String) {
    let mut first: Option<(String, String)> = None;
    for backend in BACKENDS {
        let out = Command::new(env!("CARGO_BIN_EXE_zawk"))
            .current_dir(dir)
            .env("LC_ALL", "C.UTF-8")
            .args(*backend)
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "{:?} {:?} failed: {}",
            backend,
            args,
            stderr
        );
        match &first {
            None => first = Some((stdout, stderr)),
            Some((s, _)) => assert_eq!(s, &stdout, "{:?} differs for {:?}", backend, args),
        }
    }
    first.unwrap()
}

fn run(dir: &Path, args: &[&str]) -> String {
    run_in(dir, args).0
}

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

#[test]
fn df_group_matches_awk_arrays() {
    let dir = data_dir();
    let classic = run(
        dir.path(),
        &[
            "-i",
            "csv",
            "-H",
            r#"$FI["status"] == "paid" { s[$FI["city"]] += $FI["amount"] }
               END { PROCINFO["sorted_in"] = "@ind_str_asc"; for (c in s) print c, s[c] }"#,
            "orders.csv",
        ],
    );
    let df = run(
        dir.path(),
        &[
            r#"BEGIN {
                 d = df_filter(df_read("orders.csv"), "status == 'paid'")
                 g = df_sort(df_group(d, "city", "sum(amount) as total"), "city")
                 for (i = 1; i <= df_nrow(g); i++) print df_get(g, i, "city"), df_get(g, i, "total")
               }"#,
        ],
    );
    assert_eq!(classic, "Oslo 1\nParis 15\nRome 15\n");
    assert_eq!(df, classic);
}

#[test]
fn collect_in_main_loop() {
    let dir = data_dir();
    let out = run(
        dir.path(),
        &[
            "-i",
            "csv",
            "-H",
            r#"BEGIN { d = df_new("city,amount:float") }
               $FI["status"] == "paid" { df_append(d, $FI["city"], $FI["amount"]) }
               END {
                 g = df_group(d, "city", "sum(amount) as total, count() as n")
                 df_print(df_sort(g, "total desc, city"))
               }"#,
            "orders.csv",
        ],
    );
    assert_eq!(out, "city total n\nParis 15 2\nRome 15 2\nOslo 1 1\n");
}

#[test]
fn append_record_uses_header() {
    let dir = data_dir();
    let out = run(
        dir.path(),
        &[
            "-i",
            "csv",
            "-H",
            r#"BEGIN { d = df_new() }
               { df_append_record(d) }
               END { print df_nrow(d), df_ncol(d), df_schema(d); print df_get(d, 6, "amount") }"#,
            "orders.csv",
        ],
    );
    assert_eq!(
        out,
        "6 5 id:string,user_id:string,city:string,status:string,amount:string\n2.5\n"
    );
}

#[test]
fn null_text_round_trips() {
    // `--null`: `join_tsv` writes the null text as `\N`; `df_append_record` and `df_read` of a
    // `.tsv` read `\N` back as a null, and `df_write` writes a null or the null text as `\N`.
    let dir = data_dir();
    fs::write(path(dir.path(), "nulls.csv"), "a,b\n1,NULL\n").unwrap();
    let out = run(
        dir.path(),
        &[
            "--null",
            "NULL",
            "-i",
            "csv",
            "-H",
            r#"BEGIN { d = df_new() }
               { df_append_record(d) }
               END {
                 print df_get(d, 1, "b"); print df_nrow(df_filter(d, "is_null(b)"))
                 df_append(d, 2, "NULL")
                 df_write(d, "nulls.tsv")
                 back = df_read("nulls.tsv")
                 print df_nrow(df_filter(back, "is_null(b)")), df_get(back, 2, "b")
               }"#,
            "nulls.csv",
        ],
    );
    assert_eq!(out, "NULL\n1\n2 NULL\n");
    assert_eq!(
        fs::read_to_string(path(dir.path(), "nulls.tsv")).unwrap(),
        "a\tb\n1\t\\N\n2\t\\N\n"
    );
}

#[test]
fn strnum_comparisons_and_params() {
    let dir = data_dir();
    // "10" > 9 numerically, as `$5 > 9` compares a field
    let out = run(
        dir.path(),
        &[r#"BEGIN {
               d = df_read("orders.csv")
               print df_nrow(df_filter(d, "amount > 9"))
               print df_nrow(df_filter(d, "amount > @min && city in ('Rome', 'Oslo')", "min", 2))
               print df_nrow(df_filter(d, "city ~ /^P/"))
               print typeof(d)
               df_free(d)
               print typeof(d)
             }"#],
    );
    assert_eq!(out, "2\n2\n3\ndataframe\nstring\n");
}

#[test]
fn join_select_mutate() {
    let dir = data_dir();
    let out = run(
        dir.path(),
        &[r#"BEGIN {
               o = df_read("orders.csv"); u = df_read("users.csv")
               j = df_join(o, u, "user_id=id", "left")
               j = df_mutate(j, "who = name == '' ? 'unknown' : upper(name), double = amount * 2")
               df_print(df_select(j, "id, who, double"), "csv")
               print df_nrow(df_join(u, o, "id=user_id", "anti"))
             }"#],
    );
    assert_eq!(
        out,
        "id,who,double\n1,ANN,25\n2,BOB,10\n3,ANN,14\n4,CY,2\n5,BOB,20\n6,unknown,5\n1\n"
    );
}

#[test]
fn mutate_column_number_replaces() {
    let dir = data_dir();
    let out = run(
        dir.path(),
        &[r#"BEGIN {
               d = df_new("a,b"); df_append(d, 1, 2)
               m = df_mutate(d, "$2 = $2 * 10")
               print df_ncol(m), df_get(m, 1, "b")
               m = df_mutate(m, "1 = $1 + 5")
               print df_ncol(m), df_get(m, 1, "a")
             }"#],
    );
    assert_eq!(out, "2 20\n2 6\n");
}

#[test]
fn print_formats_follow_output() {
    let dir = data_dir();
    let prog = r#"BEGIN { df_print(df_head(df_select(df_read("users.csv"), "id, name"), 2)) }"#;
    assert_eq!(run(dir.path(), &[prog]), "id name\n10 Ann\n11 Bob\n");
    assert_eq!(
        run(dir.path(), &["-o", "csv", prog]),
        "id,name\n10,Ann\n11,Bob\n"
    );
    assert_eq!(
        run(dir.path(), &["-o", "jsonl", prog]),
        "{\"id\":\"10\",\"name\":\"Ann\"}\n{\"id\":\"11\",\"name\":\"Bob\"}\n"
    );
    // OFS, and print and df_print interleaved on the same output
    assert_eq!(
        run(
            dir.path(),
            &[r#"BEGIN { OFS = "|"; print "before"; df_print(df_tail(df_read("users.csv"), 1)); print "after" }"#]
        ),
        "before\nid|name\n14|Dee\nafter\n"
    );
    let md = run(
        dir.path(),
        &[r#"BEGIN { df_print(df_head(df_read("users.csv"), 1), "markdown") }"#],
    );
    assert!(md.contains("| id | name |") || md.contains("| id"), "{}", md);
}

/// With `-o`, stdout is one table: `df_print` writes its header only when it starts stdout, and
/// a format other than the one of `-o` does not go through the `-o` writer.
#[test]
fn print_header_once_with_output_format() {
    let dir = data_dir();
    let twice = r#"BEGIN { d = df_new("a"); df_append(d, 1); df_print(d); df_print(d) }"#;
    assert_eq!(
        run(dir.path(), &["-o", "jsonl", twice]),
        "{\"a\":\"1\"}\n{\"a\":\"1\"}\n"
    );
    assert_eq!(run(dir.path(), &["-o", "csv", twice]), "a\n1\n1\n");
    assert_eq!(run(dir.path(), &["-o", "tsv", twice]), "a\n1\n1\n");
    // without -o, each call writes its header line
    assert_eq!(run(dir.path(), &[twice]), "a\n1\na\n1\n");
    // a header written by print starts the table
    let after_print = r#"BEGIN { print "a"; d = df_new("a"); df_append(d, 1); df_print(d) }"#;
    assert_eq!(run(dir.path(), &["-o", "jsonl", after_print]), "{\"a\":\"1\"}\n");
    assert_eq!(run(dir.path(), &["-o", "csv", after_print]), "a\n1\n");
    // the format of -o given explicitly
    let same = r#"BEGIN { d = df_new("a"); df_append(d, 1); df_print(d, "jsonl"); df_print(d, "ndjson") }"#;
    assert_eq!(
        run(dir.path(), &["-o", "jsonl", same]),
        "{\"a\":\"1\"}\n{\"a\":\"1\"}\n"
    );
    // another format: ignored by the jsonl writer, written as text with -o csv
    let table = r#"BEGIN { d = df_new("a"); df_append(d, 1); df_print(d, "table") }"#;
    let (out, err) = run_in(dir.path(), &["-o", "jsonl", table]);
    assert_eq!(out, "{\"a\":\"1\"}\n");
    assert!(err.contains("df_print") && err.contains("ignored"), "{}", err);
    let json = r#"BEGIN { d = df_new("a"); df_append(d, 1); df_print(d, "json") }"#;
    let out = run(dir.path(), &["-o", "csv", json]);
    assert!(out.starts_with("[{\"a\":") && out.ends_with("}]\n"), "{}", out);
    // a table with other columns: a warning, its rows under the first header
    let other = r#"BEGIN { d = df_new("a"); df_append(d, 1); e = df_new("b"); df_append(e, 2)
                           df_print(d); df_print(e) }"#;
    let (out, err) = run_in(dir.path(), &["-o", "csv", other]);
    assert_eq!(out, "a\n1\n2\n");
    assert!(err.contains("differ from the header"), "{}", err);
}

#[test]
fn arrays_bridge() {
    let dir = data_dir();
    let out = run(
        dir.path(),
        &[r#"BEGIN {
               d = df_read("orders.csv", "csv", "infer_types=1")
               n = df_col(d, "amount", a)
               print n, _sum(a), _median(a)
               df_row(d, 2, row); print row["city"], row["amount"]
               df_columns(d, names); print names[1], names[5]
               df_to_map(df_read("users.csv"), "id", "name", who); print who[12]
               cnt["b"] = 2; cnt["a"] = 1
               t = df_from_array(cnt, "k,n"); print df_get(t, 1, "k"), df_get(t, 2, "n")
             }"#],
    );
    assert_eq!(out, "6 38 6\nRome 5\nid amount\nCy\na 2\n");
}

#[test]
fn pivot_melt_concat_distinct() {
    let dir = data_dir();
    let out = run(
        dir.path(),
        &[r#"BEGIN {
               d = df_read("orders.csv")
               p = df_pivot(d, "city", "status", "count()")
               df_print(p, "csv")
               m = df_melt(df_select(p, "city, paid"), "city")
               print df_nrow(m), df_get(m, 1, "variable"), df_get(m, 1, "value")
               print df_nrow(df_concat(d, d)), df_nrow(df_distinct(d, "city"))
               print df_nrow(df_slice(d, 2, 3)), df_get(df_slice(d, 2, 3), 1, "id")
             }"#],
    );
    assert_eq!(
        out,
        "city,paid,refunded\nParis,2,1\nRome,2,\nOslo,1,\n3 paid 2\n12 3\n3 2\n"
    );
}

#[test]
fn write_and_read_back() {
    let dir = data_dir();
    for ext in ["csv", "tsv", "jsonl", "parquet", "avro", "json", "csv.gz"] {
        let file = format!("out.{}", ext);
        let prog = format!(
            r#"BEGIN {{
                 d = df_read("orders.csv", "", "infer_types=1")
                 df_write(d, "{f}")
                 back = df_read("{f}")
                 print df_nrow(back), df_get(back, 6, "amount"), df_get(back, 1, "city")
               }}"#,
            f = file
        );
        assert_eq!(run(dir.path(), &[&prog]), "6 2.5 Paris\n", "{}", ext);
        assert!(path(dir.path(), &file).exists());
    }
}

#[test]
fn errors_are_warnings() {
    let dir = data_dir();
    let (out, err) = run_in(
        dir.path(),
        &[r#"BEGIN {
               print df_nrow("df#999") + 0
               print (df_filter(df_read("users.csv"), "nme == 1") == "")
               r = try df_nrow("nope") else -1
               print r
             }"#],
    );
    assert_eq!(out, "0\n1\n-1\n");
    assert!(err.contains("invalid dataframe handle \"df#999\""), "{}", err);
    assert!(err.contains("did you mean \"name\""), "{}", err);
}

#[test]
fn user_function_shadows_df_builtin() {
    let dir = data_dir();
    let out = run(
        dir.path(),
        &[r#"function df_read(x) { return "mine:" x }
             BEGIN { print df_read("a") }"#],
    );
    assert_eq!(out, "mine:a\n");
}

#[test]
fn wrong_number_of_arguments() {
    let out = Command::new(env!("CARGO_BIN_EXE_zawk"))
        .args([r#"BEGIN { df_head() }"#])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("df_head"), "{}", err);
}

#[test]
fn parallel_append_lint() {
    let out = Command::new(env!("CARGO_BIN_EXE_zawk"))
        .args([
            "lint",
            "-pr",
            "-e",
            r#"BEGIN { d = df_new("a") } { df_append(d, $1) } END { df_print(d) }"#,
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("W307"), "{}", text);
}
