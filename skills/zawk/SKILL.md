---
name: zawk
description: >
  zawk is AWK implement in Rust with stdlib support and gawk compatibility. It can read CSV/TSV, JSONL, 
  and stdlib has text, math, datetime, crypto, parser, encode/decode, ID, KV, SQLite/MySQL, Redis/NATS etc. support.
  It has Jev decision support with filter, rank, classify, score.
allowed-tools: Bash
---

zawk is AWK implementation with gawk compatible, and you can use zawk to replace gawk for text process.

zawk has great features:

* CSV/TSV/JSONL support
* High performance
* A standard library: text, math, datetime, crypto, parser, encode/decode, ID, KV, SQLite/MySQL, Redis/NATS, jev etc.
* i18n support: `length("你好Hello") # 7`, `substr("你好Hello", 1, 2) # 你好`

# Standard Library

### jev

- filter: `jev(array_or_text, 'the name is European')`
- rank: `jev_prob(array_or_text, 'the customer is angry')` 0 - 1
- classify: `jev_choice(array_or_text, 'which team should handle this?', ARRAY['billing', 'technical', 'security', 'sales']) `
- score: `jev_score(array_or_text, 'how luxurious is this product?', ARRAY['budget', 'mid-range', 'premium', 'luxury']) ` 1 - 5
