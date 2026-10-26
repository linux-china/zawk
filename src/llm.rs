//! `zawk llm`: a prompt that asks an LLM to write a zawk program answering a question about
//! the input files.
//!
//! The prompt is printed to standard output, to be piped to an LLM tool, e.g.
//! `zawk llm -i csv -H family.csv 'how many adults?' | llm`. It has:
//!
//! * the command line that runs the program, and what its options mean: the input format, the
//!   field separator, the header (`-H`) and the output format;
//! * every input file: the first lines (the first rows of a Parquet file), and for CSV, TSV,
//!   JSON, JSON Lines and Parquet the schema and the summary of `zawk inspect`;
//! * the zawk language notes and the reference of the builtin functions;
//! * how to check the program with `zawk lint`, and how to answer;
//! * the question.

use std::io;

use crate::function_docs;
use crate::inspect::{self, Format, Sample};

pub struct Options {
    /// `-i`, as given.
    pub input_format: Option<String>,
    /// `-H`, as given.
    pub header: bool,
    /// `-F`, as given, and with its escapes processed.
    pub field_sep: Option<(String, String)>,
    /// `-o`, as given.
    pub output_format: Option<String>,
    /// The number of sample lines of each file.
    pub sample_rows: usize,
    /// The number of rows read for the summary.
    pub max_rows: Option<u64>,
    pub files: Vec<String>,
    pub question: String,
}

/// The longest sample line, in characters.
const MAX_LINE: usize = 500;

/// Returns the prompt.
pub fn prompt(opts: &Options) -> io::Result<String> {
    // As for a run, JSON input is the default when every input file ends with `.json`.
    let input_format = opts.input_format.clone().or_else(|| {
        let json = !opts.files.is_empty()
            && opts.files.iter().all(|f| f.split('#').next().unwrap_or(f).ends_with(".json"));
        json.then(|| "json".to_string())
    });
    let format = input_format.as_deref().map(|f| match f {
        "ndjson" => "jsonl",
        "md" => "markdown",
        f => f,
    });
    let implied_header = matches!(format, Some("json" | "jsonl" | "parquet" | "markdown"));
    let header = opts.header || implied_header;

    let mut options = Vec::new();
    if let Some(f) = &opts.input_format {
        options.push(format!("-i {}", f));
    }
    if opts.header {
        options.push("-H".to_string());
    }
    if let Some((fs, _)) = &opts.field_sep {
        options.push(format!("-F {}", shell_quote(fs)));
    }
    let files: Vec<String> = opts.files.iter().map(|f| shell_quote(f)).collect();
    let join = |cmd: &str, extra: &[String]| {
        let mut parts = vec![cmd.to_string()];
        parts.extend(options.iter().cloned());
        parts.extend(extra.iter().cloned());
        parts.push("'<PROGRAM>'".to_string());
        parts.extend(files.iter().cloned());
        parts.join(" ")
    };
    let output: Vec<String> = opts.output_format.iter().map(|o| format!("-o {}", o)).collect();
    let command = join("zawk", &output);
    let lint_command = join("zawk lint", &[]);

    let mut run = Vec::new();
    run.push(format!("Input format: {}", input_notes(format, input_format.is_some() && opts.input_format.is_none())));
    run.push(format!("Field separator: {}", fs_notes(format, opts.field_sep.as_ref())));
    run.push(format!("Header: {}", header_notes(format, opts.header, implied_header)));
    run.push(format!("Output format: {}", output_notes(opts.output_format.as_deref())));
    run.push(if files.is_empty() {
        "Input files: none, the input is read from standard input.".to_string()
    } else {
        format!("Input files: {}.", files.iter().map(|f| format!("`{}`", f)).collect::<Vec<_>>().join(", "))
    });

    let mut data = String::new();
    for file in &opts.files {
        data.push_str(&file_section(file, format, header, opts)?);
    }
    if data.is_empty() {
        data.push_str("No input file is given: the program reads standard input, whose content is unknown.\n");
    }

    // The categories of the reference are headings under "Builtin functions".
    let functions: String = function_docs::render_text(&function_docs::find(None))
        .lines()
        .map(|l| match l.strip_prefix("# ") {
            Some(category) => format!("## {}\n", category),
            None => format!("{}\n", l),
        })
        .collect();
    let run: String = run.iter().map(|l| format!("- {}\n", l)).collect();
    Ok(fill(
        include_str!("templates/llm.md"),
        &[
            ("command", &command),
            ("run", &run),
            ("data", data.trim_end()),
            ("functions", functions.trim_end()),
            ("lint_command", &lint_command),
            ("question", opts.question.trim()),
        ],
    ))
}

/// Replaces the `{{name}}` placeholders of `template` in one pass, so the values (such as the
/// sample data) are never expanded.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}").and_then(|end| values.iter().find(|(k, _)| *k == &after[..end]).map(|(_, v)| (end, v))) {
            Some((end, value)) => {
                out.push_str(value);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The section of an input file.
fn file_section(file: &str, format: Option<&str>, header: bool, opts: &Options) -> io::Result<String> {
    let mut out = format!("## `{}`\n\n", file);
    if file == "-" {
        out.push_str("Standard input: its content is unknown.\n\n");
        return Ok(out);
    }
    let structured = format.and_then(Format::parse);
    if format.is_none() {
        if let Some(f) = inspect::extension_format(file) {
            out.push_str(&format!(
                "Note: the file name suggests {} data, but it is read as plain text lines; \
                 `-i {}` may read it better (say so in the answer if the question needs it).\n\n",
                f.name().to_uppercase(),
                f.name()
            ));
        }
    }
    // With a header, the header line and `sample_rows` data lines.
    let raw_lines = opts.sample_rows + usize::from(header && format != Some("markdown"));
    // A structured input is read once: its sections keep the first raw lines too (a remote input
    // is not fetched twice).
    let sections = match structured {
        Some(structured) => Some(inspect::sections(
            file,
            &inspect::Options {
                format: Some(structured),
                style: inspect::Style::Markdown,
                sample_rows: opts.sample_rows,
                max_rows: opts.max_rows,
                header,
            },
            raw_lines,
        )?),
        None => None,
    };
    if structured != Some(Format::Parquet) {
        let lines = match &sections {
            Some(sections) => sections.lines.clone(),
            None => match inspect::sample(file, None, header && format != Some("markdown"), opts.sample_rows)? {
                Sample::Lines(lines) => lines,
                Sample::Fields { .. } => Vec::new(),
            },
        };
        out.push_str(&format!("First {} lines (the raw text of the file):\n\n", lines.len()));
        out.push_str(&fence("text", &lines.iter().map(|l| truncate(l)).collect::<Vec<_>>().join("\n")));
        if format.is_none() {
            if let Some(nf) = fields_per_line(&lines, opts.field_sep.as_ref().map(|(_, fs)| fs.as_str())) {
                out.push_str(&format!("Fields per line (NF) with this FS in these lines: {}.\n\n", nf));
            }
        }
    }
    let (Some(structured), Some(sections)) = (structured, sections) else {
        return Ok(out);
    };
    let rows = if sections.limited {
        format!("at least {} (only the first {} rows were read for the summary)", sections.rows, sections.rows)
    } else {
        sections.rows.to_string()
    };
    out.push_str(&format!("Rows: {}\n\n", rows));
    out.push_str("Schema (the field number, the column name and its type):\n\n");
    out.push_str(&sections.schema);
    if structured == Format::Parquet {
        out.push_str(&format!("\nFirst {} rows, as awk sees the fields:\n\n", sections.sample.lines().count().saturating_sub(2)));
        out.push_str(&sections.sample);
    }
    out.push_str("\nSummary of the columns:\n\n");
    out.push_str(&sections.summary);
    out.push('\n');
    for note in sections.notes {
        out.push_str(&format!("- {}\n", note));
    }
    out.push('\n');
    Ok(out)
}

fn input_notes(format: Option<&str>, by_default: bool) -> String {
    let text = match format {
        None => "text (no `-i`): every line is a record, split into fields by FS",
        Some("csv") => "CSV (`-i csv`): every CSV row is a record, its fields unescaped (quoted fields may contain commas and newlines); $0 is the unescaped row; assigning to fields does nothing",
        Some("tsv") => "TSV (`-i tsv`): every line is a record, split at tabs, with `\\t`, `\\n` and `\\\\` unescaped",
        Some("json") => "JSON (`-i json`): each element of the top-level array (or the object) is a record, read as JSON Lines; `file.json#$.data` selects the records with a JSONPath",
        Some("jsonl") => "JSON Lines (`-i jsonl`): every line is a JSON object and a record; its values are the fields, in the order the keys are first seen (a key first seen in a later record is appended, and NF grows from that record on); $0 is the JSON line",
        Some("parquet") => "Parquet (`-i parquet`): every row is a record, the columns are the fields; $0 is the row as a JSON object",
        Some("markdown") => "Markdown (`-i markdown`): the first table of the file is read as CSV rows; the header row gives the column names (without `:TYPE` suffixes)",
        Some(f) => return format!("`{}`", f),
    };
    if by_default {
        format!("{} (the default, as every input file ends with `.json`)", text)
    } else {
        text.to_string()
    }
}

fn fs_notes(format: Option<&str>, fs: Option<&(String, String)>) -> String {
    match (format, fs) {
        (Some(f), _) => format!("not used, fields are split by the {} rules of `-i`", f),
        (None, None) => "the default FS (\" \"): fields are separated by runs of spaces and tabs, leading and trailing blanks are ignored".to_string(),
        (None, Some((given, fs))) => {
            let kind = if fs == " " {
                "runs of spaces and tabs, as the default"
            } else if fs.chars().count() == 1 {
                "this literal character"
            } else {
                "this regular expression"
            };
            format!("`-F {}` (FS = {:?}): fields are separated by {}", shell_quote(given), fs, kind)
        }
    }
}

fn header_notes(format: Option<&str>, given: bool, implied: bool) -> String {
    let fi = "`FI` maps each column name to its field number, so `$FI[\"age\"]` is the value of the `age` column and `FI[\"age\"]` its number; prefer `$FI[\"name\"]` to `$N` for readability";
    if implied {
        let how = if format == Some("markdown") { "the header row of the table is not a record" } else { "the column names come from the data" };
        format!("implied by `-i {}` ({}): {}; NR counts the data records only", format.unwrap_or_default(), how, fi)
    } else if given {
        format!("`-H`: the first line is the header and not a record (NR is 1 on the first data line); {}", fi)
    } else {
        "no `-H`: the first line is the first record (NR == 1), `FI` is empty, fields are `$1`, `$2`... (if the first line looks like a header, skip it with `NR > 1`)".to_string()
    }
}

fn output_notes(format: Option<&str>) -> String {
    match format {
        None => "text (no `-o`): `print` writes its arguments separated by OFS (\" \") and ends with ORS (\"\\n\"); `printf` is available".to_string(),
        Some(f) => {
            let text = match f {
                "csv" => "`print a, b` writes a CSV row, escaping the fields as needed",
                "tsv" => "`print a, b` writes a TSV row, escaping tabs and newlines",
                "jsonl" | "ndjson" => "stdout is written as JSON Lines: the first printed record is the header, a column name may end with `:number` or `:boolean` (default string), and each following record becomes a JSON object; print the header in BEGIN, e.g. `print \"name\", \"age:number\"`",
                "markdown" | "md" => "stdout is rendered as a Markdown table: the first printed record is the header row; print the header in BEGIN",
                "table" => "stdout is rendered as an aligned table with borders when the program ends: the first printed record is the header; print the header in BEGIN",
                "parquet" => "stdout is written as a Parquet file: the first printed record is the header, a column name may end with `:TYPE` (BOOLEAN, INT32, INT64, DOUBLE, DATE, TIMESTAMP, STRING...; default STRING); print the header in BEGIN",
                _ => "",
            };
            format!("`-o {}`: {}", f, text)
        }
    }
}

/// The number of fields of the lines (a range when they differ), for the default FS or a
/// one-character FS.
fn fields_per_line(lines: &[String], fs: Option<&str>) -> Option<String> {
    let counts: Vec<usize> = match fs {
        None | Some(" ") => lines.iter().map(|l| l.split([' ', '\t']).filter(|s| !s.is_empty()).count()).collect(),
        Some(fs) if fs.chars().count() == 1 => {
            lines.iter().map(|l| if l.is_empty() { 0 } else { l.split(fs).count() }).collect()
        }
        _ => return None,
    };
    let (min, max) = (*counts.iter().min()?, *counts.iter().max()?);
    Some(if min == max { min.to_string() } else { format!("{} to {}", min, max) })
}

fn truncate(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE) {
        Some((i, _)) => format!("{}… (truncated)", &line[..i]),
        None => line.to_string(),
    }
}

/// A fenced code block, with a fence longer than any run of backticks in `text`.
fn fence(lang: &str, text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{}{}\n{}\n{}\n\n", fence, lang, text, fence)
}

/// Quotes `s` for a POSIX shell when needed.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote() {
        assert_eq!(shell_quote("family.csv"), "family.csv");
        assert_eq!(shell_quote("s3://bucket/a.csv"), "s3://bucket/a.csv");
        assert_eq!(shell_quote("my file.csv"), "'my file.csv'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("\\t"), r"'\t'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn fields() {
        let lines = vec!["a b  c".to_string(), " d e ".to_string()];
        assert_eq!(fields_per_line(&lines, None).as_deref(), Some("2 to 3"));
        assert_eq!(fields_per_line(&["a:b:c".to_string()], Some(":")).as_deref(), Some("3"));
        assert_eq!(fields_per_line(&lines, Some("[,;]")), None);
    }

    #[test]
    fn fill_once() {
        let out = fill("a {{x}} b {{y}} {{z}}", &[("x", "{{y}}"), ("y", "2")]);
        assert_eq!(out, "a {{y}} b 2 {{z}}");
    }

    #[test]
    fn fences() {
        assert_eq!(fence("text", "a"), "```text\na\n```\n\n");
        assert_eq!(fence("text", "a ``` b"), "````text\na ``` b\n````\n\n");
    }
}
