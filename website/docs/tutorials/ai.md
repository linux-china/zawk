---
title: AI Integration
sidebar_label: AI integration
sidebar_position: 3
---

# AI Integration

zawk works with AI in three directions:

1. **LLMs write zawk**: `zawk llm` builds a prompt from your data and question; `zawk lint` checks the answer.
2. **Agents use zawk**: a Claude Code skill and machine-readable tooling make zawk a natural tool for agents.
3. **zawk asks LLMs**: `jev` functions make typed decisions per record, and `http_post` calls any model API.

## 1. Let an LLM write the program: `zawk llm`

`zawk llm [options] FILE... QUESTION` prints a prompt for an LLM. It contains:

- the command line of the run and what its options mean (input format, `FS`, header, output format)
- the first lines of every input file
- the schema and a summary of each column (`zawk inspect`)
- the zawk language notes and builtin functions
- how to check the program with `zawk lint`
- your question

Pipe it to any LLM tool. With [llm](https://llm.datasette.io/), `-x` extracts the first code block, which zawk
runs directly:

```shell
zawk -i csv -H "$(zawk llm -i csv -H demo.csv 'how many adults?' | llm -x)" demo.csv
```

zawk can also run the first ` ```awk ` block of a Markdown answer, so the raw answer works too:

```shell
zawk -i csv -H "$(zawk llm -i csv -H demo.csv 'average age by decade' | llm)" demo.csv
```

### With an agent

An AI agent writes the program, checks it with `zawk lint`, runs it and answers the question:

```shell
# Claude Code
zawk llm -i csv -H demo.csv 'what is the average age of adults?' | claude -p --dangerously-skip-permissions

# Qoder CLI
zawk llm -i csv -H demo.csv 'how many adults?' | qodercli -p --dangerously-skip-permissions
```

:::warning
`--dangerously-skip-permissions` lets the agent run commands without asking. Only use it with data and
questions you trust.
:::

## 2. zawk as a tool for agents

### Claude Code skill

The repository ships a skill at
[`skills/zawk/SKILL.md`](https://github.com/linux-china/zawk/blob/master/skills/zawk/SKILL.md). Copy it into
`~/.claude/skills/zawk/` (or your project's `.claude/skills/`), and Claude Code will prefer zawk over
awk/sed/cut pipelines or one-off Python scripts for line-oriented data work.

```shell
mkdir -p ~/.claude/skills/zawk
curl -fsSL https://raw.githubusercontent.com/linux-china/zawk/master/skills/zawk/SKILL.md -o ~/.claude/skills/zawk/SKILL.md
```

There is also an [`llms.txt`](https://github.com/linux-china/zawk/blob/master/llms.txt) for tools that read it.

### Machine-readable tooling

Everything an agent needs is available as structured output:

```shell
zawk functions --json json_       # signatures, params, return types and examples as JSON Lines
zawk inspect -o markdown data.csv # schema, sample rows and column summary as Markdown
zawk lint -i csv -H -f report.awk # diagnostics as JSON, exit code 1 on errors
zawk -o md '...' data.csv         # results as a Markdown table for the reply
```

`zawk lint` reports a JSON document:

```json
{
  "valid": false,
  "errors": 1,
  "warnings": 0,
  "diagnostics": [
    {
      "code": "E301", "severity": "error", "message": "Call to unknown function \"foo\"",
      "file": "demo.awk", "line": 3, "column": 9,
      "source": "  print foo(1)", "caret": "        ^"
    }
  ]
}
```

See [Tooling](../tooling.md) for all diagnostics codes.

## 3. Ask an LLM from a program

### Typed decisions: `jev`

`jev` functions ask a typed question about a record (a text, or an array sent as a JSON object) using the
[TypeSafe](https://docs.typesafe.ai/api) API. Set `TYPESAFE_API_KEY` (in the environment or `.env`).

```shell
# filter: 1 when the probability of "yes" is above jev_threshold (default 0.5)
zawk 'jev($0, "the name is European")' names.txt
zawk -v jev_threshold=0.8 'jev($0, "the customer is angry")' tickets.txt

# rank: probability of yes, 0..1
zawk '{ printf "%.2f\t%s\n", jev_prob($0, "the customer is angry"), $0 }' tickets.txt | sort -rn

# classify: pick one option
zawk '{ print jev_choice($0, "which team should handle this?", "billing,technical,security,sales"), $0 }' tickets.txt

# score: probability-weighted position on ordered levels, 0 .. length(levels) - 1
zawk '{ print jev_score($0, "how luxurious is this product?", "budget,mid-range,premium,luxury"), $0 }' products.txt

# structured record: an array is sent as JSON
zawk -i csv -H '{ r["title"] = $FI["title"]; r["body"] = $FI["body"]; if (jev(r, "this is a bug report")) print $FI["id"] }' issues.csv
```

Options can describe themselves: for an array entry whose key and value differ, the value is the description
of the key:

```awk
BEGIN {
    teams["billing"]  = "invoices, refunds, payment methods"
    teams["security"] = "account access, passwords, suspicious activity"
    teams["technical"] = "bugs, errors, performance"
}
{ print jev_choice($0, "which team should handle this ticket?", teams), $0 }
```

### Any model API with `http_post`

Combine f-strings, `escape("json", ...)` and `json_value` to call a chat completion API per record. For example
with the Claude API:

```awk
BEGIN {
    h["x-api-key"] = ENVIRON["ANTHROPIC_API_KEY"]
    h["anthropic-version"] = "2023-06-01"
}
{
    prompt = escape("json", "Summarize in one short sentence: " $0)
    body = f"{{\"model\":\"claude-haiku-5-5\",\"max_tokens\":100,\"messages\":[{{\"role\":\"user\",\"content\":\"{prompt}\"}}]}}"
    r = http_post("https://api.anthropic.com/v1/messages", body, h)
    print r["status"] == 200 ? json_value(r["text"], "$.content[0].text") : "error " r["status"]
}
```

```shell
zawk -f summarize.awk reviews.txt
```

:::tip
`escape("json", s)` returns the escaped content without the surrounding quotes, so it fits inside a JSON string
literal in an f-string. Check with `zawk 'BEGIN { print escape("json", "say \"hi\"") }'`.
:::

### Fuzzy matching without an LLM

Not every "smart" match needs a model: `similar(a, b)` (Jaro-Winkler) catches typos and name variants locally:

```shell
zawk -i csv -H 'similar(tolower($FI["company"]), "anthropic") > 0.9' leads.csv
```
