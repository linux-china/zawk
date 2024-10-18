export LLVM_SYS_150_PREFIX := "/opt/homebrew/Cellar/llvm@15/15.0.7"

build:
  cargo build

# build on Windows platform
build-windows:
  cross build --no-default-features --target x86_64-pc-windows-gnu

# build with LLVM backend
build-llvm:
  cargo build --features llvm_backend

release:
  cargo build --release
  ls -al target/release/zawk
  cp target/release/zawk ~/bin/

begin-file:
  cargo run --package zawk --bin zawk -- '{print $1}' demo.txt

dump-prometheus:
  cargo run --package zawk --bin zawk -- dump --prometheus http://localhost:8081/actuator/prometheus

display-awk-file-help:
  cargo run --package zawk --bin zawk -- -f demo.awk --help

run-local:
  cargo run --package zawk --bin zawk -- -f demo.awk demo.txt

run-local-2-file:
  rm -rf output.txt
  cargo run --package zawk --bin zawk -- -f demo.awk --out-file output.txt demo.txt
  cat output.txt

run-uuid:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print uuid(), uuid("v7") }' demo.txt

run-ulid:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print ulid() }' demo.txt

run-tsid:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print tsid() }' demo.txt

run-snowflake:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print snowflake(11) }' demo.txt

run-fend:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print fend("1 + 1.1 + 23") }' demo.txt

run-systime:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print systime() }' demo.txt

run-mktime:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print mktime("2012 12 21 0 0 0") }' demo.txt

run-mktime-nushell:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print mktime("2024-04-27 17:07:25.684184848 +08:00") }' demo.txt

run-strftime:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print strftime(), strftime("%Y-%m-%d %H:%M:%S"), strftime("%Y-%m-%d %H:%M:%S", 1621530000) }' demo.txt

run-abs:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print ceil(-2.1) }' demo.txt

run-trim:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print trim(":hello:",":"), "world" }' demo.txt

run-truncate:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print truncate("hello World",10) }' demo.txt

run-base64:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print encode("base64","hello")}' demo.txt

run-escape-sql:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print escape("sql","good morning")}' demo.txt

run-sha256:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print hash("sha246","hello")}' demo.txt

run-hmac:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print hmac("HmacSHA256","password-1", "hello")}' demo.txt

run-sprintf:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print sprintf("%.1f", 10.3456) }' demo.txt

run-max:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print min(9, 10.01) }' demo.txt

run-url:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print url("https://xxx.yyy.zz@example.com/hello")["user"] }' demo.txt

run-path:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print path("./demo.awk")["full_path"] }' demo.txt

run-shlex:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print shlex("echo hello world")[2] }' demo.txt

run-to-json:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr["name"]="jackie"; arr["age"]= 11; print to_json(arr) }' demo.txt

run-from-json:
  cargo run --package zawk --bin zawk -- 'BEGIN{  arr=from_json("{\"name\": \"jackie\", \"age\": 18}"); print arr["name"] }' demo.txt

run-from-csv:
  cargo run --package zawk --bin zawk -- 'BEGIN{  arr=from_csv("first,second"); print arr[1] }' demo.txt

run-to-csv:
  cargo run --package zawk --bin zawk -- 'BEGIN{  arr[1]= 8; arr[2]= 4; print to_csv(arr) }' demo.txt

run-asort:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr[1]= 8; arr[2]= 4;  arr[4]= 2; n = asort(arr); print arr[1], arr[2], arr[3] }' demo.txt

run-join:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr[1]= 8; arr[2]= 4;  arr[4]= 2;  print arr[1], arr[2], arr[4];  print _join(arr, ",") }' demo.txt

run-uniq:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr[1]= "first"; arr[2]= "second";  arr[3]= "first";  arr2 = uniq(arr); print arr2[1], length(arr2) }' demo.txt

run-local-ip:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print local_ip() }' demo.txt

run-whoami:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print whoami() }' demo.txt

run-kv-get:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print kv_get("namespace1","nick") }' demo.txt

run-redis-kv-get:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print kv_get("redis://localhost:6379/demo1","nick") }' demo.txt

run-nats-kv-get:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print kv_get("nats://localhost:4222/bucket1","nick") }' demo.txt

run-publish:
  cargo run --package zawk --bin zawk -- 'END{ publish("notification", "Done") }' demo.txt

run-mkbool:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print mkbool("No") }' demo.txt

run-seq:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr = seq(-1, 2, 10.0); print arr[1], arr[2] }' demo.txt

run-capitalize:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print capitalize("hello world!"), uncapitalize("Hello world!") }' demo.txt

run-strtonum:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print strtonum("0x11"), strtonum("0x11"), strtonum("1.7560473e+07") }' demo.txt

run-s3-get:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print s3_get("mj-artifacts","health2.txt") }' demo.txt

run-s3-put:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print s3_put("mj-artifacts","health2.txt","Hello AWK") }' demo.txt

run-typeof:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr[1]=1;  print typeof(arr) }' demo.txt

run-isarray:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr[1]=1;  print isarray(1) }' demo.txt

run-isint:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print isint("222.0") }' demo.txt

run-isnum:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print isnum("u0.9") }' demo.txt

run-datetime:
  cargo run --package zawk --bin zawk -- 'BEGIN{ dt=datetime("2019-11-29");  print dt["year"], datetime()["year"]  }' demo.txt

run-data-url:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print data_url("data:text/plain;base64,SGVsbG8sIFdvcmxkIQ==")["data"] }' demo.txt

run-jwt:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr["iat"] = 12345565; arr["name"] = "Jackie";  print jwt("HS256","123456", arr) }' demo.txt

run-dejwt:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print dejwt("123456", "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJleHAiOjEyMDgyMzQyMzQyMzQsIm5hbWUiOiJKb2huIERvZSIsInJhdGUiOjExLjExLCJ1c2VyX2lkIjoxMTIzNDQsInVzZXJfdXVpZCI6Ijg0NTZlYTU0LTYyZTgtNGEzMS05Y2NlLTE4ZGU3YTZhODkwZCJ9.P2e6b_I1pfbmgoyXcEwAKM1XjgNeRku0jatyf2CYD3o")["exp"] }' demo.txt

run-os-functions:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print whoami(), os(), os_family(), arch(), pwd(), user_home() }' demo.txt

run-sqlite-query:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print sqlite_query("sqlite.db", "select nick,email,age from user")[1] }' demo.txt

run-libsql-query:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print libsql_query("http://127.0.0.1:8080", "SELECT id, email FROM users")[1] }' demo.txt

run-mysql-query:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print mysql_query("mysql://root:123456@localhost:3306/test", "select id, name from people")[1] }' demo.txt

run-pg-query:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print pg_query("postgres://postgres:postgres@localhost/demo", "SELECT id, name FROM city")[1] }' demo.txt

run-semver:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print semver("1.2.3-alpha-1")["pre"] }' demo.txt

run-mask:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print mask("110")}' demo.txt

run-var-dump:
  cargo run --package zawk --bin zawk -- 'BEGIN{  var_dump(110);  log_debug("Hello"); }' demo.txt

run-pad-start:
  cargo run --package zawk --bin zawk -- 'BEGIN{  print pad("hello", 10, "*") }' demo.txt

run-array-max:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr[1]=1; arr[2]=0.2; arr[3]=0.3; print _min(arr); }' demo.txt

run-array-sum:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr[1]=1; arr[2]=0.2; arr[3]=0.3; print _sum(arr); }' demo.txt

run-camel-case:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print camel_case("Hello World"); }' demo.txt

run-words:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print words("Hello, World!")[2]; }' demo.txt

run-repeat:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print repeat("123",3); }' demo.txt

run-default-if-empty:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print default_if_empty("   ","hello"); }' demo.txt

run-append-if-missing:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print append_if_missing("https://example.com","/"); }' demo.txt

run-quote:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print quote("hello world"); }' demo.txt

run-read-all:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print read_all("demo.awk"); }' demo.txt

run-pairs:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print pairs("a=1,b=2")["a"], pairs("id=1&name=Hello%20World","&")["name"], pairs("a:1|b:2","|",":")["b"]; }' demo.txt

run-func:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print func("hello(1,2,3)")[0] }' demo.txt

run-format-bytes:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print format_bytes(1234423) }' demo.txt

run-to-bytes:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print to_bytes("10.1 KB") }' demo.txt

run-starts-with:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print starts_with("abc", "a"), ends_with("abc", "d"), contains("abc", "b") }' demo.txt

run-encrypt:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print encrypt("aes-128-cbc","Hello World", "123456") }' demo.txt

run-variant:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print variant("week(5)")["value"] }' demo.txt

run-flags:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print flags("{vip,top10}")["top10"] }' demo.txt

run-version:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print version() }' demo.txt

run-bf:
  cargo run --package zawk --bin zawk -- 'BEGIN{ bf_insert("first"); print bf_contains("first"), bf_contains(1.1) }' demo.txt

run-last-part:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print last_part("o.apache.catalina.core.StandardService") }' demo.txt

run-parse:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print parse("Hello World","{greet} {name}")["greet"] }' demo.txt

run-rparse:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print rparse("Hello World","(\\w+) (\\w+)")[1] }' demo.txt

run-duration:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print duration("2min + 12sec") }' demo.txt

run-hex2rgb:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print hex2rgb("#FF0000")[1] }' demo.txt

run-rgb2hex:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print rgb2hex(255,0,0) }' demo.txt

run-fake:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print fake("name"), fake("name","cn") }' demo.txt

run-is-format:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print is("phone","+861866666666") }' demo.txt

run-mkpass:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print mkpass(), mkpass(12) }' demo.txt

run-json-value:
  cargo run --package zawk --bin zawk -- 'BEGIN{ arr["origin"]="192.168.1.2"; json_text=to_json(arr); print json_value(json_text, "$.origin") }' demo.txt

run-html-value:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print html_value("<head><title>this is title</title></head>", "title") }' demo.txt

run-xml-value:
  cargo run --package zawk --bin zawk -- 'BEGIN{ print xml_value("<head><title>this is title</title></head>", "/head/title") }' demo.txt

run-figlet:
  cargo run --package zawk --bin zawk -- 'BEGIN { print figlet("Hello zawk"); }' demo.txt

run-read-config:
  cargo run --package zawk --bin zawk -- 'BEGIN { print read_config("tests/demo.ini")["key1"]; }' demo.txt

run-send-mail:
  DRY_RUN=1 cargo run --package zawk --bin zawk -- 'BEGIN { send_mail("support@example.com","customer@example.com", "subject -1","body" ); }' demo.txt

run-smtp-send:
  cargo run --package zawk --bin zawk -- 'BEGIN { smtp_send("smtp://localhost:1025","support@example.com","customer@example.com", "subject -1","body" ); }' demo.txt

run-run-system:
  cargo run --package zawk --bin zawk -- 'BEGIN { result = system("ls -al"); print result; }'

run-run-system2:
  cargo run --package zawk --bin zawk -- 'BEGIN { result = system2("ls -al"); print result["stdout"]; }'

run-run-http-post:
  cargo run --package zawk --bin zawk -- 'BEGIN { result = http_post("https://httpbin.org/post", "Hello"); print result["text"]; }'

run-eval:
  cargo run --package zawk --bin zawk -- 'BEGIN { arr["a"] = 1;  print eval("1 * 2"), eval("a+2", arr); }'
