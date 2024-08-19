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
                if parts.len() >= 2 {
                    let key = *comment_parts.get(0).unwrap();
                    let value = *comment_parts.get(1).unwrap();
                    let comment_tag = CommentTag {
                        type_name: "meta".to_owned(),
                        value1: key.to_string(),
                        value2: Some(value.to_string()),
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
        let var_tags: Vec<&CommentTag> = tags.iter()
            .filter(|tag| tag.type_name == "var")
            .collect();
        let env_tags: Vec<&CommentTag> = tags.iter()
            .filter(|tag| tag.type_name == "env")
            .collect();
        println!("{awk_file} {}", version.unwrap_or("".to_string()));
        if let Some(author_name) = &author {
            println!("{author_name}");
        }
        if let Some(desc) = &awk_file_desc {
            println!("{desc}");
        }
        if !var_tags.is_empty() {
            let params = var_tags.iter().map(|tag| format!("-v {}=[value]", tag.value1)).join(" ");
            println!();
            println!("USAGE: {awk_file} {} <input-file>", params);
            println!();
            println!("ARGS:");
            for var_tag in var_tags {
                println!("  [{}]  {}", var_tag.value1, var_tag.description.clone().unwrap_or("".to_string()))
            }
            println!();
        }

        if !env_tags.is_empty() {
            println!("Environment Variables:");
            for env_tag in env_tags {
                println!("  [{}]  {}", env_tag.value1, env_tag.description.clone().unwrap_or("".to_string()))
            }
        }
    }
}

pub fn print_awk_file_version(awk_file: &str) {
    if let Ok(awk_code) = std::fs::read_to_string(awk_file) {
        let tags = parse_comment_tags(&awk_code);
        let version = tags.iter()
            .find(|tag| tag.type_name == "meta" && tag.value1 == "version")
            .map(|tag| tag.value2.clone().unwrap_or("No version found".to_string()))
            .unwrap_or("No version found".to_owned());
        println!("{version}");
    } else {
        eprintln!("Failed to read {} file", awk_file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_print_awk_file_help() {
        print_awk_file_help("demo.awk");
    }

    #[test]
    fn test_parse_tags() {
        let awk_code = r#"
#!/usr/bin/env zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @var nick user name
# @var email user email
# @env DB_NAME db name


BEGIN {

}
"#;
        let tags = parse_comment_tags(awk_code);
        for tag in tags {
            println!("{:?}", tag);
        }
    }
}
