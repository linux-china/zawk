use itertools::Itertools;
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug)]
pub struct CommentTag {
    pub type_name: String,
    pub value1: String,
    pub value2: Option<String>,
    pub description: Option<String>,
}

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new("\\s+").unwrap());

fn parse_comment_tags(awk_code: &str) -> Vec<CommentTag> {
    let mut tags: Vec<CommentTag> = vec![];
    for line in awk_code.lines() {
        let Some(tag_declare) = line.strip_prefix("# @") else {
            continue;
        };
        let (tag_name, rest) = match tag_declare.split_once(' ') {
            Some((name, rest)) => (name, Some(rest.trim())),
            None => (tag_declare, None),
        };
        if tag_name == "desc" {
            tags.push(CommentTag {
                type_name: "desc".to_owned(),
                value1: "".to_owned(),
                value2: None,
                description: rest.map(str::to_string),
            });
            continue;
        }
        // `var`, `env` and `meta` need a name, e.g. `# @meta version 1.0`
        let Some(rest) = rest.filter(|rest| !rest.is_empty()) else {
            continue;
        };
        if tag_name == "var" || tag_name == "env" {
            let comment_parts: Vec<&str> = WHITESPACE.splitn(rest, 2).collect();
            tags.push(CommentTag {
                type_name: tag_name.to_string(),
                value1: comment_parts[0].to_string(),
                value2: None,
                description: comment_parts.get(1).map(|item| item.trim().to_string()),
            });
        } else if tag_name == "meta" {
            let comment_parts: Vec<&str> = WHITESPACE.splitn(rest, 3).collect();
            tags.push(CommentTag {
                type_name: "meta".to_owned(),
                value1: comment_parts[0].to_string(),
                value2: comment_parts.get(1).map(|item| item.trim().to_string()),
                description: comment_parts.get(2).map(|item| item.trim().to_string()),
            });
        }
    }
    tags
}

/// Prints the help of the program `awk_code` read from `awk_file`, from its comment tags.
pub fn print_awk_file_help(awk_file: &str, awk_code: &str) {
    let tags = parse_comment_tags(awk_code);
    let mut awk_file_desc: Option<String> = None;
    let mut version: Option<String> = None;
    let mut author: Option<String> = None;
    for tag in &tags {
        if tag.type_name == "meta" {
            if tag.value1 == "version" {
                version = tag.value2.clone();
            } else if tag.value1 == "author" {
                author = tag.value2.clone();
            }
        }
        if tag.type_name == "desc" {
            awk_file_desc = tag.description.clone();
        }
    }
    let var_tags: Vec<&CommentTag> = tags.iter().filter(|tag| tag.type_name == "var").collect();
    let env_tags: Vec<&CommentTag> = tags.iter().filter(|tag| tag.type_name == "env").collect();
    println!("{awk_file} {}", version.unwrap_or("".to_string()));
    if let Some(author_name) = &author {
        println!("{author_name}");
    }
    if let Some(desc) = &awk_file_desc {
        println!("{desc}");
    }
    if !var_tags.is_empty() {
        let params = var_tags
            .iter()
            .map(|tag| format!("-v {}=[value]", tag.value1))
            .join(" ");
        println!();
        println!("USAGE: {awk_file} {} <input-file>", params);
        println!();
        println!("ARGS:");
        for var_tag in var_tags {
            println!(
                "  [{}]  {}",
                var_tag.value1,
                var_tag.description.clone().unwrap_or("".to_string())
            )
        }
        println!();
    }

    if !env_tags.is_empty() {
        println!("Environment Variables:");
        for env_tag in env_tags {
            println!(
                "  [{}]  {}",
                env_tag.value1,
                env_tag.description.clone().unwrap_or("".to_string())
            )
        }
    }
}

/// Prints the `# @meta version` of the program `awk_code`.
pub fn print_awk_file_version(awk_code: &str) {
    let version = parse_comment_tags(awk_code)
        .into_iter()
        .find(|tag| tag.type_name == "meta" && tag.value1 == "version")
        .and_then(|tag| tag.value2)
        .unwrap_or("No version found".to_owned());
    println!("{version}");
}

/// Column names referenced as string constants through `FI`, e.g. `$FI["name"]`. With `-i jsonl`
/// these always get a column, even when the first record does not contain the key.
pub fn fi_constant_keys(awk_code: &str) -> Vec<String> {
    let re = Regex::new(r#"\bFI\s*\[\s*"((?:[^"\\]|\\.)*)"\s*\]"#).unwrap();
    re.captures_iter(awk_code)
        .map(|cap| cap[1].replace("\\\"", "\"").replace("\\\\", "\\"))
        .unique()
        .collect()
}

/// The variables a program declares with `# @var name description`, which the run gets with
/// `-v` (an optional one is declared as `name?`).
pub fn declared_vars(awk_code: &str) -> Vec<String> {
    parse_comment_tags(awk_code)
        .into_iter()
        .filter(|tag| tag.type_name == "var")
        .map(|tag| tag.value1.trim_end_matches('?').to_string())
        .collect()
}

/// Checks the `# @var` / `# @env` requirements of a program. Variables count as provided when
/// they are set with `-v` (`var_decs`) or with `var=value` operands (`operand_vars`); the latter
/// are only assigned once input reading starts, so a warning is printed for those that the
/// program seems to use in `BEGIN`.
pub fn validate_awk_code(awk_code: &str, var_decs: &[String], operand_vars: &[String]) -> bool {
    let tags = parse_comment_tags(awk_code);
    if tags.is_empty() {
        return true;
    }
    let var_names: Vec<&str> = var_decs
        .iter()
        .chain(operand_vars)
        .map(|s| s.split('=').next().unwrap())
        .collect();
    let missed_var_tags: Vec<&CommentTag> = tags
        .iter()
        .filter(|tag| tag.type_name == "var")
        .filter(|tag| !var_names.contains(&tag.value1.as_str()) && !tag.value1.ends_with('?'))
        .collect();
    let missed_env_tags: Vec<&CommentTag> = tags
        .iter()
        .filter(|tag| tag.type_name == "env")
        .filter(|tag| std::env::var(&tag.value1).is_err() && !tag.value1.ends_with('?'))
        .collect();
    let satisfied = missed_var_tags.is_empty() && missed_env_tags.is_empty();
    if !satisfied {
        eprintln!("Errors:");
        if !missed_var_tags.is_empty() {
            eprintln!("Required variables were not provided: ");
            for tag in missed_var_tags {
                eprintln!("  - {}", tag.value1);
            }
        }
        if !missed_env_tags.is_empty() {
            eprintln!("Required environment variables were not provided: ");
            for tag in missed_env_tags {
                eprintln!("  - {}", tag.value1);
            }
        }
        return false;
    }
    for name in operand_only_vars_in_begin(awk_code, &tags, var_decs, operand_vars) {
        eprintln!(
            "Warning: variable `{name}` is assigned by a `{name}=value` operand, which is not visible in BEGIN; use `-v {name}=value` instead"
        );
    }
    true
}

/// Declared variables provided only by operands, which the program refers to in a `BEGIN` block.
/// The check is textual: the program has a `BEGIN` block and the name occurs in its code.
fn operand_only_vars_in_begin(
    awk_code: &str,
    tags: &[CommentTag],
    var_decs: &[String],
    operand_vars: &[String],
) -> Vec<String> {
    static BEGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bBEGIN\s*\{").unwrap());
    if operand_vars.is_empty() || !BEGIN.is_match(awk_code) {
        return Vec::new();
    }
    let code: String = awk_code
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let provided_by =
        |decs: &[String], name: &str| decs.iter().any(|s| s.split('=').next() == Some(name));
    tags.iter()
        .filter(|tag| tag.type_name == "var")
        .map(|tag| tag.value1.trim_end_matches('?'))
        .filter(|name| provided_by(operand_vars, name) && !provided_by(var_decs, name))
        .filter(|name| {
            Regex::new(&format!(r"\b{}\b", regex::escape(name)))
                .map(|re| re.is_match(&code))
                .unwrap_or(false)
        })
        .map(str::to_string)
        .collect()
}

/// Field references accepted by the column sugar: `$1`, `$NF`, `$(NF-1)`, and comma separated
/// lists of them such as `$1,$3,$NF`. At least two fields are required: a single field
/// such as `$2` is a standard awk pattern (print records whose `$2` is non-empty/non-zero).
static FIELD_LIST: LazyLock<Regex> = LazyLock::new(|| {
    let field = r"\$(?:\d+|NF|\(\s*NF\s*-\s*\d+\s*\))";
    Regex::new(&format!(r"^{field}(?:\s*,\s*{field})+$")).unwrap()
});

/// Sugar syntax for programs given on the command line:
///
/// * a program that is only a comma-separated list of two or more fields prints them:
///   `$1,$3` is `{ print $1,$3 }`. A single field (`$2`, `$NF`) is left as is, since it
///   is a standard awk pattern that filters out records with an empty/zero field.
/// * a program that is only a regex prints the matching records: `/error/` is
///   `/error/ { print $0 }` (the same as standard awk).
///
/// Anything else, e.g. `$1 > 1`, `$1 ~ /x/` or `$1 { print $2 }`, is left as is, so that
/// standard awk programs keep their meaning.
pub fn sugar_syntax_convert(awk_code: String) -> String {
    let code = awk_code.trim();
    if FIELD_LIST.is_match(code) {
        return format!("{{ print {} }}", code);
    }
    if code.len() > 1 && code.starts_with('/') && code.ends_with('/') {
        return format!("{} {{ print $0 }}", code);
    }
    awk_code
}

/// Whether the program file `name` (a local path or a URL) is a Markdown file, by its extension.
pub fn is_markdown_file(name: &str) -> bool {
    let path = name
        .split(['?', '#'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    path.ends_with(".md") || path.ends_with(".markdown")
}

/// The first fenced code block of `markdown` whose info string starts with `awk`, `zawk` or
/// `gawk`, with the lines before it blanked out, so that errors are reported at the lines of the
/// Markdown text. `None` when there is no such block.
pub fn extract_awk_block(markdown: &str) -> Option<String> {
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
    let mut events = Parser::new(markdown).into_offset_iter();
    while let Some((event, range)) = events.next() {
        let Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) = event else {
            continue;
        };
        let lang = info
            .split(|c: char| c.is_whitespace() || c == '{' || c == ',')
            .next()
            .unwrap_or("");
        if !["awk", "zawk", "gawk"]
            .iter()
            .any(|l| lang.eq_ignore_ascii_case(l))
        {
            continue;
        }
        // The line after the opening fence, for an empty block.
        let mut first_line = markdown[..range.start].matches('\n').count() + 1;
        let mut code = String::new();
        for (event, range) in events.by_ref() {
            match event {
                Event::Text(text) => {
                    if code.is_empty() {
                        first_line = markdown[..range.start].matches('\n').count();
                    }
                    code.push_str(&text);
                }
                Event::End(TagEnd::CodeBlock) => break,
                _ => {}
            }
        }
        return Some("\n".repeat(first_line) + &code);
    }
    None
}

/// Whether `text` looks like Markdown with fenced code blocks: a line starts (after at most three
/// spaces) with a ```` ``` ```` or `~~~` fence.
fn has_code_fence(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line
            .strip_prefix("   ")
            .or_else(|| line.strip_prefix("  "))
            .or_else(|| line.strip_prefix(' '))
            .unwrap_or(line);
        line.starts_with("```") || line.starts_with("~~~")
    })
}

/// A program given as text (the program operand or `-e`): `Ok(Some(block))` with the first awk
/// code block when the text is Markdown, such as the answer of an LLM, `Ok(None)` when it is not
/// Markdown (the text is the program itself), and an error when it is Markdown without an awk
/// code block.
pub fn program_text(text: &str) -> Result<Option<String>, String> {
    if !has_code_fence(text) {
        return Ok(None);
    }
    extract_awk_block(text)
        .map(Some)
        .ok_or_else(|| "no ```awk code block in the Markdown text".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_first_line_and_operand_vars() {
        let code = "# @var name the name\nBEGIN { print name }\n";
        assert!(!validate_awk_code(code, &[], &[]));
        assert!(validate_awk_code(code, &["name=bob".to_string()], &[]));
        assert!(validate_awk_code(code, &[], &["name=bob".to_string()]));
        assert!(validate_awk_code("# @var name?\n{ print }\n", &[], &[]));
        assert!(validate_awk_code("{ print }\n", &[], &[]));
    }

    #[test]
    fn test_operand_only_vars_in_begin() {
        let operands = ["name=bob".to_string()];
        let code = "# @var name the name\nBEGIN { print name }\n";
        let tags = parse_comment_tags(code);
        assert_eq!(
            operand_only_vars_in_begin(code, &tags, &[], &operands),
            vec!["name"]
        );
        assert!(operand_only_vars_in_begin(code, &tags, &operands, &operands).is_empty());
        let code = "# @var name the name\n{ print name }\n";
        let tags = parse_comment_tags(code);
        assert!(operand_only_vars_in_begin(code, &tags, &[], &operands).is_empty());
    }

    #[test]
    fn test_extract_awk_block() {
        let md = "Intro\n\n```sh\nzawk 'x'\n```\n\n```awk\nBEGIN { print 1 }\n```\n\n```awk\nBEGIN { print 2 }\n```\n";
        assert_eq!(
            extract_awk_block(md).unwrap(),
            "\n".repeat(7) + "BEGIN { print 1 }\n"
        );
        assert_eq!(
            extract_awk_block("~~~ZAWK\n{ print }\n~~~\n").unwrap(),
            "\n{ print }\n"
        );
        assert_eq!(
            extract_awk_block("````gawk {.x}\nBEGIN { s = \"```\" }\n````\n").unwrap(),
            "\nBEGIN { s = \"```\" }\n"
        );
        assert_eq!(
            extract_awk_block("* item\n\n  ```awk\n  { print $1 }\n  ```\n").unwrap(),
            "\n\n\n{ print $1 }\n"
        );
        assert_eq!(extract_awk_block("```awk\n```\n").unwrap(), "\n");
        assert_eq!(extract_awk_block("```awkward\nx\n```\n"), None);
        assert_eq!(extract_awk_block("no code\n"), None);
    }

    #[test]
    fn test_parse_comment_tags() {
        assert!(parse_comment_tags("# @meta\n# @meta \n# @var\n# @env  \n").is_empty());
        let tags = parse_comment_tags(
            "# @meta version  1.0 first release\n# @var  name  user name\n# @desc\n",
        );
        assert_eq!(tags.len(), 3);
        assert_eq!(
            (tags[0].value1.as_str(), tags[0].value2.as_deref()),
            ("version", Some("1.0"))
        );
        assert_eq!(tags[0].description.as_deref(), Some("first release"));
        assert_eq!(
            (tags[1].type_name.as_str(), tags[1].value1.as_str()),
            ("var", "name")
        );
        assert_eq!(tags[1].description.as_deref(), Some("user name"));
        assert_eq!(
            (tags[2].type_name.as_str(), tags[2].description.as_deref()),
            ("desc", None)
        );
    }

    #[test]
    fn test_program_text() {
        assert_eq!(program_text("BEGIN { print \"```\" }"), Ok(None));
        assert_eq!(program_text("BEGIN { s = \"~~~\" }\n{ print s }"), Ok(None));
        assert_eq!(
            program_text("Answer:\n```awk\nBEGIN {}\n```"),
            Ok(Some("\n\nBEGIN {}\n".to_string()))
        );
        assert_eq!(
            program_text("Answer:\n  ~~~ zawk\nBEGIN {}\n  ~~~"),
            Ok(Some("\n\nBEGIN {}\n".to_string()))
        );
        assert!(program_text("Answer:\n```bash\nzawk 'BEGIN{print 1}'\n```\n").is_err());
    }

    #[test]
    fn test_is_markdown_file() {
        assert!(is_markdown_file("tests/demo.awk.md"));
        assert!(is_markdown_file("https://host/README.MD?raw=1"));
        assert!(!is_markdown_file("demo.awk"));
    }

    #[test]
    fn test_print_awk_file_help() {
        let awk_code = std::fs::read_to_string("demo.awk").unwrap();
        print_awk_file_help("demo.awk", &awk_code);
    }

    #[test]
    fn test_validate_awk_code() {
        let awk_code = r#"
#!/usr/bin/env zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @var nick user name
# @var email? user email
# @env USER db name

"#;
        let var_decs = vec!["nick".to_owned()];
        validate_awk_code(awk_code, &var_decs, &[]);
    }

    #[test]
    fn test_parse_tags() {
        let awk_code = r#"
#!/usr/bin/env zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @meta default-subcommand
# @var nick user name
# @var email user email
# @env S3_ENDPOINT s3 endpoint

"#;
        let tags = parse_comment_tags(awk_code);
        for tag in tags {
            println!("{:?}", tag);
        }
    }

    #[test]
    fn test_sugar_syntax_convert() {
        for code in ["$1,$3", "$1, $3, $NF", " $2,$(NF-1) "] {
            let new_code = sugar_syntax_convert(code.to_owned());
            assert_eq!(new_code, format!("{{ print {} }}", code.trim()), "{}", code);
        }
        assert_eq!(
            sugar_syntax_convert("/error/".to_owned()),
            "/error/ { print $0 }"
        );
        // `/a/ ; /b/` becomes `/a/ ; /b/ { print $0 }`, the same as the plain program
        assert_eq!(
            sugar_syntax_convert("/a/ ; /b/".to_owned()),
            "/a/ ; /b/ { print $0 }"
        );
        for code in [
            "$1",
            "$NF",
            "$(NF-1)",
            "$1 {print $2}",
            "$1{print}",
            "$1 {n++} END{print n}",
            "$1 ~ /x/",
            "$1 !~ /x/",
            "$1 % 2",
            "$2 + 0",
            "$1 in a",
            "$1 = \"X\"",
            "$1;$2",
            "$1>1",
            "$1 == \"a\" {print}",
            "$1 \" \" $3",
            "/",
            "{ print }",
        ] {
            assert_eq!(sugar_syntax_convert(code.to_owned()), code, "{}", code);
        }
    }
}
