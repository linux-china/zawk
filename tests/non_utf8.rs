//! Functions that take strings must not corrupt input that is not valid UTF-8 (Latin-1/GBK logs,
//! binary data): invalid bytes are kept as is, and byte-defined functions (hashes, encodings)
//! work on the raw bytes.
use assert_cmd::Command;
use std::collections::BTreeSet;

const BACKEND_ARGS: &[&str] = &["-Binterp", "-Bcranelift"];

/// "caf" + Latin-1 'é' (0xE9) + "x", not valid UTF-8.
const INPUT: &[u8] = b"caf\xe9x\n";

/// `(function, expression, expected output)`; `None` only checks that the output has no U+FFFD.
const CASES: &[(&str, &str, Option<&[u8]>)] = &[
    ("length", "length($0)", Some(b"5")),
    ("strlen", "strlen($0)", Some(b"5")),
    ("char_at", "char_at($0, 4)", Some(b"\xe9")),
    ("chars", "chars($0)[4]", Some(b"\xe9")),
    ("match", "match($0, /x/)", Some(b"5")),
    ("substr", "substr($0, 3)", Some(b"f\xe9x")),
    ("sub", "sub(/c/, \"C\")", Some(b"1")),
    ("gsub", "gsub(/x/, \"y\")", Some(b"1")),
    ("gensub", "gensub(/c/, \"C\", \"g\", $0)", Some(b"Caf\xe9x")),
    ("index", "index($0, \"x\")", Some(b"5")),
    ("last_index", "last_index($0, \"x\")", Some(b"5")),
    ("split", "split($0, a, \"f\") a[2]", Some(b"2\xe9x")),
    ("patsplit", "patsplit($0, a, /a/, s) a[1] s[1]", Some(b"1af\xe9x")),
    ("asort", "(m[1] = $0) (m[2] = \"d\") asort(m, v, \"@val_str_desc\") v[1]", Some(b"caf\xe9xd2d")),
    ("asorti", "(m[$0] = 1) asorti(m, k, \"@ind_str_desc\") k[1]", Some(b"11caf\xe9x")),
    ("last_part", "last_part(\"a/\" $0, \"/\")", Some(b"caf\xe9x")),
    ("sprintf", "sprintf(\"[%s]\", $0)", Some(b"[caf\xe9x]")),
    ("join_fields", "join_fields(1, 1)", Some(b"caf\xe9x")),
    ("tolower", "tolower($0)", Some(b"caf\xe9x")),
    ("toupper", "toupper($0)", Some(b"CAF\xe9X")),
    ("trim", "trim(\" \" $0 \" \")", Some(b"caf\xe9x")),
    ("truncate", "truncate($0, 4, \"\")", Some(b"caf\xe9")),
    ("capitalize", "capitalize($0)", Some(b"Caf\xe9x")),
    ("uncapitalize", "uncapitalize(\"C\" $0)", Some(b"ccaf\xe9x")),
    ("camel_case", "camel_case($0)", None),
    ("kebab_case", "kebab_case($0)", None),
    ("snake_case", "snake_case($0)", None),
    ("title_case", "title_case($0)", None),
    ("pascal_case", "pascal_case($0)", None),
    ("is", "is(\"email\", $0)", Some(b"0")),
    ("starts_with", "starts_with($0, \"caf\")", Some(b"1")),
    ("ends_with", "ends_with($0, substr($0, 4))", Some(b"1")),
    ("contains", "contains($0, char_at($0, 4))", Some(b"1")),
    ("mask", "mask($0)", Some(b"*af\xe9x")),
    ("pad", "pad($0, 7, \"*\")", Some(b"*caf\xe9x*")),
    ("pad_start", "pad_start($0, 7, \"*\")", Some(b"**caf\xe9x")),
    ("pad_end", "pad_end($0, 7, \"*\")", Some(b"caf\xe9x**")),
    ("strcmp", "strcmp($0, \"caf\")", Some(b"1")),
    ("lines", "lines($0)[1]", Some(b"caf\xe9x")),
    ("words", "words(\"a \" $0)[1]", Some(b"a")),
    ("repeat", "repeat($0, 2)", Some(b"caf\xe9xcaf\xe9x")),
    ("default_if_empty", "default_if_empty($0, \"d\")", Some(b"caf\xe9x")),
    ("append_if_missing", "append_if_missing($0, \"!\")", Some(b"caf\xe9x!")),
    ("prepend_if_missing", "prepend_if_missing($0, \"!\")", Some(b"!caf\xe9x")),
    ("remove_if_end", "remove_if_end($0, \"x\")", Some(b"caf\xe9")),
    ("remove_if_begin", "remove_if_begin($0, \"c\")", Some(b"af\xe9x")),
    ("quote", "quote($0)", Some(b"'caf\xe9x'")),
    ("double_quote", "double_quote($0)", Some(b"\"caf\xe9x\"")),
    ("parse", "parse($0, \"{a}\")[\"a\"]", Some(b"caf\xe9x")),
    ("rparse", "rparse($0, \"(.*)\")[1]", Some(b"caf\xe9x")),
    ("to_bytes", "to_bytes($0)", None),
    ("figlet", "figlet($0)", None),
    ("escape", "escape(\"json\", $0) escape(\"xml\", $0) escape(\"sql\", $0)", Some(b"caf\xe9xcaf\xe9xcaf\xe9x")),
    ("escape_csv", "escape_csv($0)", Some(b"caf\xe9x")),
    ("escape_tsv", "escape_tsv($0)", Some(b"caf\xe9x")),
    ("shlex", "shlex(\"a \" $0)[2]", Some(b"caf\xe9x")),
    ("pairs", "pairs(\"k=\" $0)[\"k\"]", Some(b"caf\xe9x")),
    ("record", "record(\"k=\" $0)[\"k\"]", None),
    ("message", "message(\"k=\" $0)[\"k\"]", None),
    ("func", "func(\"f(\" $0 \")\")[0]", None),
    ("tuple", "tuple(\"(\" $0 \")\")[1]", None),
    ("variant", "variant(\"V(\" $0 \")\")[\"name\"]", None),
    ("parse_array", "parse_array(\"[\" $0 \"]\")[1]", None),
    ("flags", "length(flags(\"{\" $0 \"}\"))", Some(b"1")),
    ("bf_insert", "bf_insert($0)", Some(b"")),
    ("bf_contains", "bf_insert($0) bf_contains(substr($0, 1, 3) \"\\xea\" \"x\")", Some(b"0")),
    ("bf_icontains", "bf_icontains($0) bf_icontains($0)", Some(b"01")),
    ("hex", "hex($0)", None),
    ("float", "float($0)", None),
    ("strtonum", "strtonum($0)", None),
    ("mkbool", "mkbool($0)", Some(b"1")),
    ("encode", "encode(\"hex\", $0) encode(\"base64\", $0) encode(\"url\", $0)", Some(b"636166e978Y2Fm6Xg=caf%E9x")),
    ("decode", "decode(\"hex\", \"636166e978\") decode(\"base64\", \"Y2Fm6Xg=\") decode(\"url\", \"caf%E9x\")", Some(b"caf\xe9xcaf\xe9xcaf\xe9x")),
    ("digest", "digest(\"md5\", $0)", Some(b"22c60c88342da269fc62268257f5625a")),
    ("hash", "hash(\"md5\", $0)", Some(b"22c60c88342da269fc62268257f5625a")),
    ("hmac", "hmac(\"HmacSHA256\", \"k\", $0) == hmac(\"HmacSHA256\", \"k\", substr($0, 1, 3) \"\\xea\" \"x\")", Some(b"0")),
    ("encrypt", "decrypt(\"aes-128-gcm\", encrypt(\"aes-128-gcm\", $0, \"k\"), \"k\")", Some(b"caf\xe9x")),
    ("decrypt", "decrypt(\"aes-256-cbc\", encrypt(\"aes-256-cbc\", $0, \"k\"), \"k\")", Some(b"caf\xe9x")),
];

/// Functions taking strings that are not covered by `CASES`: the text goes to (or comes from) an
/// external format or service that is UTF-8 by definition (paths, URLs, JSON, SQL, ...), or the
/// string is only parsed for a number, a date or an expression.
const EXEMPT: &[&str] = &[
    "url", "data_url", "path", "semver", "uuid", "_join", "eval", "fend", "strftime", "mktime",
    "duration", "hex2rgb", "fake", "jev", "jev_prob", "jev_choice", "jev_score", "from_json",
    "json_value", "json_query", "json_has", "json_get", "json_set", "json_del", "from_csv", "xml_value", "xml_query", "html_value", "html_query",
    "jwt", "dejwt", "kv_get", "kv_put", "kv_delete", "kv_clear", "http_get", "http_post",
    "send_mail", "send_email", "smtp_send", "s3_get", "s3_put", "publish", "sqlite_query",
    "sqlite_execute", "pg_query", "pg_execute", "mysql_query",
    "mysql_execute", "getenv", "system", "system2", "read_all", "write_all", "read_config",
    "close", "fflush", "log_debug", "log_info", "log_warn", "log_error", "assert",
];

fn run(backend: &str, expr: &str) -> Vec<u8> {
    let prog = format!("{{ printf \"%s\", {} }}", expr);
    let out = Command::cargo_bin("zawk")
        .unwrap()
        .args([backend, &prog])
        .write_stdin(INPUT)
        .output()
        .unwrap();
    assert!(out.status.success(), "{} {}: {}", backend, expr, String::from_utf8_lossy(&out.stderr));
    out.stdout
}

#[test]
fn non_utf8_input_is_preserved() {
    let mut failures = Vec::new();
    for backend in BACKEND_ARGS {
        for (_, expr, expected) in CASES {
            let out = run(backend, expr);
            let replaced = out.windows(3).any(|w| w == "\u{FFFD}".as_bytes());
            if replaced || expected.is_some_and(|e| e != out) {
                failures.push(format!("{} {}: got {:?}, expected {:?}", backend, expr,
                    out.escape_ascii().to_string(), expected.map(|e| e.escape_ascii().to_string())));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_string_function_is_covered() {
    let out = Command::cargo_bin("zawk").unwrap().args(["functions", "--json"]).output().unwrap();
    let json: Vec<serde_json::Value> = String::from_utf8(out.stdout).unwrap().lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let covered: BTreeSet<&str> = CASES.iter().map(|(name, _, _)| *name).chain(EXEMPT.iter().copied()).collect();
    let missing: Vec<&str> = json.iter()
        .filter(|f| f["params"].as_array().unwrap().iter().any(|p| p["type"] == "str"))
        .map(|f| f["name"].as_str().unwrap())
        .filter(|name| !covered.contains(name))
        .collect();
    assert!(missing.is_empty(), "add non-UTF-8 cases (or an exemption) for: {:?}", missing);
}
