---
slug: zawk-grok-log-analysis
title: "用 zawk + Grok 分析日志：不用 Logstash 也能把日志变成表"
authors: [ linux_china ]
tags: [ zawk, awk, logs ]
---

排查线上问题时，第一手资料往往就是日志。可日志偏偏是最难用 AWK 处理的数据：Nginx 的 access log 里空格、方括号和带引号的字符串混在一起，
应用日志的结尾是一段任意长度的自由文本，中间还夹着 Java 的堆栈。`awk '{ print $9 }'` 拿到的是不是状态码，全凭运气。

zawk 内置了 [Grok](https://www.elastic.co/guide/en/logstash/current/plugins-filters-grok.html)，就是 Logstash 和
Elasticsearch ingest pipeline 用来解析日志的那套命名模式。日志解析和 AWK 的过滤、聚合合在一条命令里，不用部署 ELK，也不用写
Python 脚本。

<!-- truncate -->

## 按空格切分为什么不靠谱

先看一行再普通不过的 access log：

```text
10.0.0.5 - alice [15/Nov/2026:10:00:04 +0800] "GET /index.html HTTP/1.1" 200 1024 "https://example.com/" "Mozilla/5.0"
```

按空格切分时，时间被拆成了 `$4` 和 `$5` 两列，请求被拆成了三列，User-Agent 里只要多一个空格，后面的列号就整体错位。经典的写法是
`$9` 当状态码、`$10` 当字节数，日志格式稍微一改就会悄悄算错。

Grok 换了个思路：不去猜分隔符，直接描述这一行**长什么样**：

```text
%{IP:client} %{USER:ident} %{USER:auth} \[%{HTTPDATE:timestamp}\] "%{WORD:verb} %{NOTSPACE:request} ...
```

`%{NAME:key}` 表示用内置模式 `NAME` 去匹配，匹配到的内容保存为 `key`。zawk 自带完整的 Logstash 模式库：`IP`、`IPORHOST`、
`WORD`、`INT`、
`NUMBER`、`URIPATHPARAM`、`HTTPDATE`、`TIMESTAMP_ISO8601`、`LOGLEVEL`、`QS`、`GREEDYDATA`，还有
`COMMONAPACHELOG`、`COMBINEDAPACHELOG`、`SYSLOGBASE` 这类整行模式。不带 key 的 `%{NAME}` 只匹配、不保存，模式里也可以直接混写正则。

zawk 提供两种用法：

- `--grok PATTERN`：用一个模式切分**每一行输入**，命名捕获就是列，用 `$FI["key"]` 读取。
- `grok(s, pattern[, defs])`：匹配**任意一个字符串**，返回一个由捕获组成的数组。

## `--grok`：一个模式，整份日志变成表

下面这份 `access.log` 是 Nginx 默认的 combined 格式，中间还混进了一行日志轮转留下的标记：

```text title="access.log"
192.168.1.10 - - [15/Nov/2026:10:00:01 +0800] "GET /api/orders?page=1 HTTP/1.1" 200 5120 "-" "Mozilla/5.0"
192.168.1.11 - - [15/Nov/2026:10:00:02 +0800] "POST /api/login HTTP/1.1" 401 64 "-" "curl/8.4.0"
192.168.1.11 - - [15/Nov/2026:10:00:03 +0800] "POST /api/login HTTP/1.1" 401 64 "-" "curl/8.4.0"
10.0.0.5 - alice [15/Nov/2026:10:00:04 +0800] "GET /index.html HTTP/1.1" 200 1024 "https://example.com/" "Mozilla/5.0"
192.168.1.11 - - [15/Nov/2026:10:00:05 +0800] "POST /api/login HTTP/1.1" 200 312 "-" "curl/8.4.0"
10.0.0.7 - - [15/Nov/2026:10:01:10 +0800] "GET /api/orders/42 HTTP/1.1" 500 128 "-" "okhttp/4.12"
10.0.0.7 - - [15/Nov/2026:10:01:12 +0800] "GET /api/orders/43 HTTP/1.1" 502 0 "-" "okhttp/4.12"
192.168.1.10 - - [15/Nov/2026:10:02:30 +0800] "GET /static/app.js HTTP/1.1" 304 0 "-" "Mozilla/5.0"
-- log rotated --
10.0.0.5 - alice [15/Nov/2026:10:03:01 +0800] "DELETE /api/orders/42 HTTP/1.1" 204 0 "-" "Mozilla/5.0"
```

用 `%{COMBINEDAPACHELOG}` 统计状态码：

```shell
$ zawk --grok '%{COMBINEDAPACHELOG}' '{ c[$FI["response"]]++ } END { for (s, n in c) print s, n }' access.log | sort
200 3
204 1
304 1
401 2
500 1
502 1
```

把所有 5xx 请求找出来：

```shell
$ zawk --grok '%{COMBINEDAPACHELOG}' '$FI["response"] >= 500 { print $FI["timestamp"], $FI["verb"], $FI["request"], $FI["response"] }' access.log
15/Nov/2026:10:01:10 +0800 GET /api/orders/42 500
15/Nov/2026:10:01:12 +0800 GET /api/orders/43 502
```

统计哪个 IP 在反复尝试登录失败：

```shell
$ zawk --grok '%{COMBINEDAPACHELOG}' \
    '$FI["request"] == "/api/login" && $FI["response"] == 401 { fail[$FI["clientip"]]++ }
     END { for (ip, n in fail) print ip, n }' access.log
192.168.1.11 2
```

几条规则：

- **`--grok` 隐含 `-H`**。日志本身没有表头，zawk 在读输入之前编译模式，直接用命名捕获构造 `FI`。
- **列的顺序就是捕获在模式里出现的顺序**，引用到的子模式也会展开。`%{COMBINEDAPACHELOG}` 依次得到 `clientip`、`ident`、
  `auth`、
  `timestamp`、`verb`、`request`、`httpversion`、`rawrequest`、`response`、`bytes`、`referrer`、`agent`，一共 12 列，
  所以 `$1` 是客户端 IP，`$9` 是状态码。写 `$FI["response"]` 更不容易出错。
- **`$0` 是原始的整行**，保持不变。
- **不匹配的行会被跳过**，比如上面的 `-- log rotated --`。这些行不算记录，也不计入 `NR`。
- **`:type` 后缀**（`%{NUMBER:bytes:int}`）是为了兼容 Logstash，只用来给列命名。值都是字符串，参与数值运算时 AWK 会自动转换。
- **同名捕获**会自动编号：`%{IP:ip} -> %{IP:ip}` 中第二个 `ip` 叫 `ip[1]`。

模式写错时，zawk 在读取任何输入之前就会报错：

```shell
$ zawk --grok '%{IP} %{WORD}' '{ print }' access.log
zawk: invalid --grok pattern "%{IP} %{WORD}": no named captures: name the parts to extract, as in %{IP:client}
```

## 不必每次都用整行模式

整行模式省事，但很多时候你只关心开头几个字段。自己写一个更短的、以 `^` 锚定的模式，匹配更快，意图也更清楚：

```shell
$ cat access.log | zawk --grok '^%{IPORHOST:client} %{USER} %{USER:user} \[%{HTTPDATE:ts}\] "%{WORD:verb} %{NOTSPACE:path}' \
    '$FI["user"] != "-" { print $FI["user"], $FI["verb"], $FI["path"] }'
alice GET /index.html
alice DELETE /api/orders/42
```

## `--grok-defs`：给自己的应用日志建一个模式库

真正难处理的是业务日志。每个团队的格式都不一样，比如这样：

```text title="app.log"
2026-11-15 10:00:01,120 INFO  [order-service] trace=7f3a order=A-1001 user=42 took=35ms Order created
2026-11-15 10:00:02,450 WARN  [order-service] trace=9b21 order=A-1002 user=7 took=1250ms Payment slow
2026-11-15 10:00:03,001 ERROR [payment] trace=9b21 order=A-1002 user=7 took=3001ms Gateway timeout
	at com.example.Gateway.call(Gateway.java:42)
	at com.example.OrderService.pay(OrderService.java:88)
2026-11-15 10:00:04,777 INFO  [order-service] trace=c0de order=A-1003 user=42 took=18ms Order created
2026-11-15 10:00:05,310 INFO  [payment] trace=c0de order=A-1003 user=42 took=220ms Payment done
2026-11-15 10:00:06,002 INFO  [order-service] trace=d00d order=A-1004 user=9 took=41ms Order created
```

把模式写进一个文件，格式和 Logstash 的 patterns 文件一样：每行一个 `NAME regex`，regex 里可以引用其他模式，空行和 `#`
注释会被忽略：

```text title="app.grok"
# app.log 的模式库
ORDER_ID [A-Z]-\d+
APPLOG %{TIMESTAMP_ISO8601:ts} %{LOGLEVEL:level}\s+\[%{DATA:service}\] trace=%{WORD:trace} order=%{ORDER_ID:order} user=%{INT:user} took=%{INT:took}ms %{GREEDYDATA:msg}
```

用 `--grok-defs` 加载后，`--grok` 就可以按名字引用这些模式。堆栈行不匹配 `APPLOG`，会被自动跳过：

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok \
    '$FI["level"] == "ERROR" { print $FI["ts"], $FI["trace"], $FI["order"], $FI["msg"] }' app.log
2026-11-15 10:00:03,001 9b21 A-1002 Gateway timeout
```

按服务统计调用次数、平均耗时和最大耗时：

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok \
    '{ s = $FI["service"]; n[s]++; sum[s] += $FI["took"]; if ($FI["took"] > max[s]) max[s] = $FI["took"] + 0 }
     END { for (s, c in n) printf "%-14s %d %8.1fms %6dms\n", s, c, sum[s] / c, max[s] }' app.log
order-service  4    336.0ms   1250ms
payment        2   1610.5ms   3001ms
```

zawk 标准库里的统计函数可以直接用，比如中位数和 P99：

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok '{ lat[NR] = $FI["took"] } END { print _median(lat), _quantile(lat, 0.99) }' app.log
130.5 2913.45
```

按 trace id 把一次请求经过的服务串起来，相当于一个简易的链路追踪：

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok \
    '{ t = $FI["trace"]; path[t] = path[t] (path[t] == "" ? "" : " -> ") $FI["service"] "(" $FI["took"] "ms)" }
     END { for (t, p in path) print t, p }' app.log
c0de order-service(18ms) -> payment(220ms)
7f3a order-service(35ms)
d00d order-service(41ms)
9b21 order-service(1250ms) -> payment(3001ms)
```

模式文件可以放在脚本旁边，纳入版本管理，团队共用。和内置模式同名的定义会覆盖内置模式。

## 把日志转成结构化数据

`--grok` 负责把日志变成列，zawk 的输出格式负责把列变成表格、JSON Lines、CSV 或 Parquet。输出的第一条记录就是表头：

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok -o table \
    'NR == 1 { print "order", "level", "service", "took" } $FI["took"] > 200 { print $FI["order"], $FI["level"], $FI["service"], $FI["took"] }' app.log
┌────────┬───────┬───────────────┬──────┐
│ order  │ level │ service       │ took │
├────────┼───────┼───────────────┼──────┤
│ A-1002 │ WARN  │ order-service │ 1250 │
│ A-1002 │ ERROR │ payment       │ 3001 │
│ A-1003 │ INFO  │ payment       │  220 │
└────────┴───────┴───────────────┴──────┘
```

表头里的 `:number` 后缀会让 JSON 输出数字，而不是字符串：

```shell
$ zawk --grok '%{COMBINEDAPACHELOG}' -o jsonl \
    'NR == 1 { print "client", "verb", "path", "status:number", "bytes:number" }
     { print $FI["clientip"], $FI["verb"], $FI["request"], $FI["response"], $FI["bytes"] }' access.log
{"client":"192.168.1.10","verb":"GET","path":"/api/orders?page=1","status":200,"bytes":5120}
{"client":"192.168.1.11","verb":"POST","path":"/api/login","status":401,"bytes":64}
...
```

换成 `-o parquet --out-file access.parquet`，就得到一个可以直接交给 DuckDB、Spark 或 Pandas 的 Parquet 文件。换成 `-o md`，
就是一张可以贴进故障复盘文档的 Markdown 表格：

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok -o md \
    'NR == 1 { print "level", "count" } { c[$FI["level"]]++ } END { for (l, n in c) print l, n }' app.log
| level | count |
| --- | --- |
| INFO | 4 |
| WARN | 1 |
| ERROR | 1 |
```

## 和标准库组合：按分钟看流量和错误

Grok 只负责切出字段，后续处理交给 zawk 的 190 多个内置函数。例如用 `strptime` 解析 `HTTPDATE`，再用 `date_trunc` 按分钟分桶：

```shell
$ zawk --grok '%{COMBINEDAPACHELOG}' \
    '{ t = strptime($FI["timestamp"], "%d/%b/%Y:%H:%M:%S %z"); m = strftime("%H:%M", date_trunc("minute", t))
       req[m]++; if ($FI["response"] >= 500) err[m]++ }
     END { for (m, n in req) printf "%s %d %d\n", m, n, err[m] }' access.log | sort
10:00 5 0
10:01 2 2
10:02 1 0
10:03 1 0
```

（`strftime` 按本机时区输出，上面的结果是在 UTC+8 下得到的。）

`url()` 可以拆解请求路径里的查询参数，`digest()` 可以给用户 ID 做脱敏，`http_post()` 可以把告警推给 Webhook。日志解析和后续处理都在同一个
AWK 程序里完成。

## `grok()`：一份日志里有多种格式

`--grok` 适合格式统一的日志。`/var/log/auth.log` 这类系统日志不一样，sshd、sudo、cron 混在一起，每种程序的消息格式都不同：

```text title="auth.log"
Nov 15 10:00:01 web1 sshd[1201]: Failed password for root from 203.0.113.9 port 52211 ssh2
Nov 15 10:00:03 web1 sshd[1201]: Failed password for root from 203.0.113.9 port 52213 ssh2
Nov 15 10:00:05 web1 sshd[1203]: Failed password for invalid user admin from 198.51.100.4 port 40022 ssh2
Nov 15 10:00:07 web1 sshd[1201]: Failed password for root from 203.0.113.9 port 52219 ssh2
Nov 15 10:00:09 web1 CRON[1300]: (root) CMD (/usr/local/bin/backup.sh)
Nov 15 10:00:12 web1 sshd[1210]: Accepted publickey for deploy from 10.0.0.5 port 50100 ssh2
Nov 15 10:00:15 web1 sudo:   deploy : TTY=pts/0 ; PWD=/home/deploy ; USER=root ; COMMAND=/bin/systemctl restart nginx
```

这时用 `grok()` 函数：先用 AWK 的 pattern 选出行，再给每类行套上各自的 grok 模式。下面的程序同时检测 SSH 暴力破解并审计
sudo 命令：

```shell
$ zawk '/sshd/ {
          m = grok($0, "Failed password for (invalid user )?%{USERNAME:user} from %{IP:ip}")
          if (length(m)) { fail[m["ip"]]++; users[m["ip"]] = m["user"] }
        }
        /sudo:/ {
          s = grok($0, "%{USERNAME:who} : .*COMMAND=%{GREEDYDATA:cmd}")
          print "sudo:", s["who"], s["cmd"]
        }
        END { for (ip, n in fail) if (n >= 3) print "brute force?", ip, users[ip], n }' auth.log
sudo: deploy /bin/systemctl restart nginx
brute force? 203.0.113.9 root 3
```

`%{SYSLOGBASE}` 可以拆出 syslog 的公共前缀：

```shell
$ zawk '{ m = grok($0, "%{SYSLOGBASE}"); print m["program"], m["pid"], m["logsource"], m["timestamp"] }' auth.log | head -3
sshd 1201 web1 Nov 15 10:00:01
sshd 1201 web1 Nov 15 10:00:03
sshd 1203 web1 Nov 15 10:00:05
```

`grok()` 的规则：

- 返回一个数组，key 是捕获名。不匹配时返回空数组，用 `length(m)` 就能判断。
- 可选部分没有参与匹配时，对应的 key 不会出现，用 `("key" in m)` 可以区分"没匹配到"和"匹配到空串"。
- 第三个参数 `defs` 和 `--grok-defs` 文件的格式相同，可以用 `read_all("app.grok")` 复用同一个模式库。
- 编译后的模式会被缓存，每条记录都调用 `grok()`，同一个模式也只编译一次。
- 模式无效时不会中断运行：返回空数组，打印警告并设置 `ERRNO`。

### 把堆栈挂回它的错误日志

`--grok` 会跳过不匹配的行，可有时这些行恰恰最有价值，比如 Java 堆栈。用 `grok()` 可以自己决定如何处理它们：

```shell
$ zawk '{ m = grok($0, "%{APPLOG}", read_all("app.grok")) }
        length(m) { last = m["order"]; if (m["level"] == "ERROR") err[last] = m["msg"]; next }
        /^\s+at / && last in err { err[last] = err[last] " | " trim($0) }
        END { for (o, e in err) print o ": " e }' app.log
A-1002: Gateway timeout | at com.example.Gateway.call(Gateway.java:42) | at com.example.OrderService.pay(OrderService.java:88)
```

### 解析结构化日志里的某个字段

日志已经是 JSON 了，但 `msg` 字段里还是一段文本？`grok()` 可以作用于任何字符串，包括 CSV 或 JSON 记录中的一个字段：

```shell
$ zawk -i jsonl '{ m = grok($FI["msg"], "%{WORD:verb} %{URIPATH:path} %{INT:status} %{INT:ms}ms")
                  if (m["status"] >= 500) print $FI["ts"], m["path"], m["ms"] }' events.jsonl
2026-11-15T10:00:02Z /api/pay 3001
```

## 模式覆盖了多少行？

写好一个模式后，最关心的是它有没有漏掉什么。`--stats` 会把 `--grok` 跳过的行统计为 `rejected_records`：

```shell
$ zawk --stats --grok '%{COMBINEDAPACHELOG}' 'END { print NR }' access.log
9
{"input_records":9,"output_records":1,"rejected_records":1,...}
```

要看具体是哪些行，用 `grok()` 取反：

```shell
$ zawk '!length(grok($0, "%{COMBINEDAPACHELOG}"))' access.log
-- log rotated --
```

## 大日志：流式、压缩、远程、并行

`--grok` 按行读取，zawk 支持的输入它都支持：

```shell
tail -F /var/log/nginx/access.log | zawk --grok '%{COMBINEDAPACHELOG}' '$FI["response"] >= 500'
zawk --grok '%{COMBINEDAPACHELOG}' '{ b += $FI["bytes"] } END { print b }' access.log.gz
zawk --grok '%{APPLOG}' --grok-defs app.grok '$FI["level"] == "ERROR"' s3://my-bucket/logs/app-2026-11-15.log
zawk -p r -j 8 --grok '%{COMBINEDAPACHELOG}' '{ c[$FI["response"]]++ } END { for (s, n in c) print s, n }' access.log.zst
```

匹配 grok 模式本质上是对整行做一次正则搜索，比按分隔符切分慢。几个提升性能的建议：

- 用 `^` 锚定模式开头，不可能匹配的行可以尽早被拒绝。
- 模式中间尽量用 `INT`、`NOTSPACE` 这类具体的模式，少用 `DATA` 和 `GREEDYDATA`。
- 只截取需要的部分，不要每次都用整行模式。
- 日志量大时加上 `-p r -j N` 多核并行。

## `--grok` 还是 `grok()`？

|              | `--grok PATTERN`            | `grok(s, pattern[, defs])`               |
|--------------|-----------------------------|------------------------------------------|
| 匹配对象     | 每一行输入                  | 任意字符串：`$0`、某个字段、某个 JSON 值 |
| 捕获结果     | 列：`$FI["key"]`、`$1..$NF` | 数组：`m["key"]`                         |
| 不匹配时     | 跳过该行                    | 返回空数组                               |
| 多种格式混合 | 整份输入只能用一个模式      | 每行可以用不同模式                       |
| 自定义模式   | `--grok-defs FILE`          | `defs` 文本，如 `read_all("app.grok")`   |

简单地说：日志格式统一，用 `--grok`；格式混杂、只需解析某个字段，或者不匹配的行需要自己处理，用 `grok()`。

## 小结

Logstash 在流水线里做的事是：用 Grok 把日志切成字段，再过滤、聚合、输出。zawk 把这件事压缩成了一条命令：

- Grok 负责理解日志的结构，可以直接复用 Logstash 社区积累的模式；
- AWK 负责过滤和聚合，pattern-action 模型天生适合逐行处理；
- zawk 标准库负责剩下的事：日期、统计、URL、哈希、HTTP、数据库；
- 输出格式负责交付：表格、Markdown、JSON Lines、Parquet。

下次登录服务器排查问题时，试试用 `zawk --grok`，不用再对着 `$9` 猜它到底是哪一列。

更多细节见 [Grok 教程](/docs/tutorials/grok)、[Parsers 文档](/docs/stdlib/parsers#grok-logs-and-semi-structured-text)
和 [日志分析教程](/docs/tutorials/log-analysis)。欢迎到 [GitHub](https://github.com/linux-china/zawk) 提 issue 和 PR。
