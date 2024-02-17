use std::path::PathBuf;
use crate::runtime;
use crate::runtime::{SharedMap, Str};

pub fn os() -> String {
    std::env::consts::OS.to_string()
}

pub fn os_family() -> String {
    match std::env::consts::OS {
        "windows" => "windows".to_string(),
        _ => "unix".to_string()
    }
}

pub fn arch() -> String {
    std::env::consts::ARCH.to_string()
}

pub fn pwd() -> String {
    std::env::current_dir().unwrap().to_str().unwrap().to_string()
}

pub fn user_home() -> String {
    match dirs::home_dir() {
        Some(path) => path.to_str().unwrap().to_string(),
        None => "".to_string()
    }
}

pub fn path<'b>(text: &str) -> runtime::StrMap<'b, Str<'b>> {
    let mut map = hashbrown::HashMap::new();
    let path_buf = PathBuf::from(text);
    if path_buf.exists() {
        map.insert(Str::from("exists"), Str::from("1"));
        if let Ok(full_path) = path_buf.canonicalize() {
            if let Some(full_path_text) = full_path.to_str() {
                map.insert(Str::from("full_path"), Str::from(full_path_text.to_string()));
            }
            if let Some(parent_path) = full_path.parent() {
                if let Some(parent_path_text) = parent_path.to_str() {
                    map.insert(Str::from("parent"), Str::from(parent_path_text.to_string()));
                }
            }
        }
        if let Some(file_name) = path_buf.file_name() {
            if let Some(file_name_text) = file_name.to_str() {
                map.insert(Str::from("file_name"), Str::from(file_name_text.to_string()));
                let file_stem = file_name_text.split('.').collect::<Vec<&str>>()[0];
                map.insert(Str::from("file_stem"), Str::from(file_stem.to_string()));
            }
        }
        if let Some(name_extension) = path_buf.extension() {
            if let Some(file_ext_text) = name_extension.to_str() {
                map.insert(Str::from("file_ext"), Str::from(file_ext_text.to_string()));
                let content_type = mime_guess::from_ext(file_ext_text).first_or_octet_stream().to_string();
                map.insert(Str::from("content_type"), Str::from(content_type));
            }
        }
    } else {
        map.insert(Str::from("exists"), Str::from("0"));
    }
    return SharedMap::from(map);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_os() {
        println!("{}", os());
    }

    #[test]
    fn test_arch() {
        println!("{}", arch());
    }

    #[test]
    fn test_path() {
        let text = "./demo.awk";
        let map = path(text);
        println!("{:?}", map);
    }
}