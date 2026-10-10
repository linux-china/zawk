use itertools::Itertools;
use regex::Regex;

#[derive(Debug)]
pub struct CommentTag {
    pub type_name: String,
    pub value1: String,
    pub value2: Option<String>,
    pub description: Option<String>,
}

fn parse_comment_tags(awk_code: &str) -> Vec<CommentTag> {
    let mut tags: Vec<CommentTag> = vec![];
    for line in awk_code.lines() {
        if line.starts_with("# @") {
            let tag_declare = &line[3..];
            let parts: Vec<&str> = tag_declare.splitn(2, ' ').collect();
            let tag_name = *parts.get(0).unwrap();
            if tag_name == "desc" {
                let comment_tag = CommentTag {
                    type_name: "desc".to_owned(),
                    value1: "".to_owned(),
                    value2: None,
                    description: parts.get(1).map(|item| item.trim().to_string()),
                };
                tags.push(comment_tag);
            } else if (tag_name == "var" || tag_name == "env") && parts.len() >= 2 {
                let re = Regex::new("\\s+").unwrap();
                let comment_parts: Vec<&str> = re.splitn(parts.get(1).unwrap().trim(), 2).collect();
                let var_name = *comment_parts.get(0).unwrap();
                let comment_tag = CommentTag {
                    type_name: tag_name.to_string(),
                    value1: var_name.to_string(),
                    value2: None,
                    description: comment_parts.get(1).map(|item| item.trim().to_string()),
                };
                tags.push(comment_tag);
            } else if tag_name == "meta" {
                let re = Regex::new("\\s+").unwrap();
                let comment_parts: Vec<&str> = re.splitn(parts.get(1).unwrap().trim(), 3).collect();
                if parts.len() >= 1 {
                    let key = *comment_parts.get(0).unwrap();
                    let value = comment_parts.get(1).map(|item| item.trim().to_string());
                    let comment_tag = CommentTag {
                        type_name: "meta".to_owned(),
                        value1: key.to_string(),
                        value2: value,
                        description: comment_parts.get(2).map(|item| item.trim().to_string()),
                    };
                    tags.push(comment_tag);
                }
            }
        }
    }
    tags
}

pub fn print_awk_file_help(awk_file: &str) {
    if let Ok(awk_code) = std::fs::read_to_string(awk_file) {
        let tags = parse_comment_tags(&awk_code);
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
}

pub fn print_awk_file_version(awk_file: &str) {
    if let Ok(awk_code) = std::fs::read_to_string(awk_file) {
        let tags = parse_comment_tags(&awk_code);
        let version = tags
            .iter()
            .find(|tag| tag.type_name == "meta" && tag.value1 == "version")
            .map(|tag| tag.value2.clone().unwrap_or("No version found".to_string()))
            .unwrap_or("No version found".to_owned());
        println!("{version}");
    } else {
        eprintln!("Failed to read {} file", awk_file);
    }
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

pub fn validate_awk_code(awk_code: &str, var_decs: &[String]) -> bool {
    let mut satisfied = true;
    // check metadata requirements
    if awk_code.contains("\n# @") {
        // detect comment tag
        let var_names: Vec<String> = var_decs
            .iter()
            .map(|s| s.split('=').next().unwrap().to_string())
            .collect();
        let tags = parse_comment_tags(awk_code);
        if !tags.is_empty() {
            let missed_var_tags: Vec<&CommentTag> = tags
                .iter()
                .filter(|tag| tag.type_name == "var")
                .filter(|tag| !var_names.contains(&tag.value1) && !tag.value1.ends_with('?'))
                .collect();
            let missed_env_tags: Vec<&CommentTag> = tags
                .iter()
                .filter(|tag| tag.type_name == "env")
                .filter(|tag| std::env::var(&tag.value1).is_err() && !tag.value1.ends_with('?'))
                .collect();
            satisfied = missed_var_tags.is_empty() && missed_env_tags.is_empty();
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
            }
        }
    }
    // check S3 operation with required environment variables
    if satisfied {
        if awk_code.contains("s3_get(") || awk_code.contains("s3_put(") {
            if !(std::env::var("AWS_ACCESS_KEY_ID").is_ok()
                && std::env::var("S3_ACCESS_KEY_ID").is_ok())
            {
                eprintln!("Errors:");
                eprintln!("Required environment variables were not provided: ");
                eprintln!("  - S3_ENDPOINT");
                eprintln!("  - S3_ACCESS_KEY_ID");
                eprintln!("  - S3_ACCESS_KEY_SECRET");
                eprintln!("  - S3_REGION");
                satisfied = false;
            }
        }
    }
    satisfied
}

pub fn sugar_syntax_convert(awk_code: String) -> String {
    // output single column
    if awk_code.starts_with("$") && !contains_conditional_ops(&awk_code) {
        return format!("{{ print {} }}", awk_code);
    } else if awk_code.starts_with("/") && awk_code.ends_with("/") {
        // conditional
        return format!("{} {{ print $0 }}", awk_code);
    }
    awk_code
}

fn contains_conditional_ops(awk_code: &str) -> bool {
    awk_code.contains("==")
        || awk_code.contains("!=")
        || awk_code.contains("<")
        || awk_code.contains(">")
        || awk_code.contains("<=")
        || awk_code.contains(">=")
        || awk_code.contains("&&")
        || awk_code.contains("||")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_print_awk_file_help() {
        print_awk_file_help("demo.awk");
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
        validate_awk_code(awk_code, &var_decs);
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
# @env DB_NAME db name

"#;
        let tags = parse_comment_tags(awk_code);
        for tag in tags {
            println!("{:?}", tag);
        }
    }

    #[test]
    fn test_sugar_syntax_convert() {
        let new_code = sugar_syntax_convert("$1".to_owned());
        println!("{}", new_code);
        assert!(new_code.contains("print"));
        let new_code = sugar_syntax_convert("/error/".to_owned());
        println!("{}", new_code);
    }
}
