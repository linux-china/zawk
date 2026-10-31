use crate::runtime;
use crate::runtime::{SharedMap, Str};
use std::path::PathBuf;

pub fn os() -> String {
    std::env::consts::OS.to_string()
}

pub fn os_family() -> String {
    match std::env::consts::OS {
        "windows" => "windows".to_string(),
        _ => "unix".to_string(),
    }
}

pub fn arch() -> String {
    std::env::consts::ARCH.to_string()
}

pub fn pwd() -> String {
    match std::env::current_dir() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(e) => {
            runtime::stdlib_warning("pwd", e);
            "".to_string()
        }
    }
}

pub fn user_home() -> String {
    match dirs::home_dir() {
        Some(path) => path.to_string_lossy().into_owned(),
        None => "".to_string(),
    }
}

pub(crate) fn path<'b>(text: &str) -> runtime::StrMap<'b, Str<'b>> {
    let mut map = crate::runtime::HashMap::default();
    let path_buf = PathBuf::from(text);
    let mut insert = |key: &str, value: &std::ffi::OsStr| {
        if let Some(value_text) = value.to_str() {
            map.insert(
                Str::from(key.to_string()),
                Str::from(value_text.to_string()),
            );
        }
    };
    // full_path needs canonicalize, so it is only available for existing paths;
    // the other fields are parsed literally, e.g. for output files to be created
    let full_path = if path_buf.exists() {
        path_buf.canonicalize().ok()
    } else {
        None
    };
    insert("exists", if path_buf.exists() { "1" } else { "0" }.as_ref());
    if let Some(full_path) = &full_path {
        insert("full_path", full_path.as_os_str());
    }
    if let Some(parent_path) = full_path.as_deref().unwrap_or(&path_buf).parent() {
        insert("parent", parent_path.as_os_str());
    }
    if let Some(file_name) = path_buf.file_name() {
        insert("file_name", file_name);
    }
    if let Some(file_stem) = path_buf.file_stem() {
        insert("file_stem", file_stem);
    }
    if let Some(name_extension) = path_buf.extension() {
        insert("file_ext", name_extension);
        if let Some(file_ext_text) = name_extension.to_str() {
            let content_type = mime_guess::from_ext(file_ext_text)
                .first_or_octet_stream()
                .to_string();
            map.insert(Str::from("content_type"), Str::from(content_type));
        }
    }
    SharedMap::from(map)
}

pub fn getenv(key: &str, default_value: &str) -> String {
    match std::env::var(key.to_uppercase()) {
        Ok(value) => value,
        Err(_) => match std::env::var(key) {
            Ok(value2) => value2.clone(),
            Err(_) => default_value.to_string(),
        },
    }
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

    #[test]
    fn test_path_file_stem() {
        let dir = std::env::temp_dir().join("zawk_test_path_file_stem");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, stem) in [
            (".bashrc", ".bashrc"),
            ("a.tar.gz", "a.tar"),
            ("demo.awk", "demo"),
        ] {
            let file = dir.join(name);
            std::fs::write(&file, "").unwrap();
            let map = path(file.to_str().unwrap());
            assert_eq!(map.get(&Str::from("file_stem")).to_string(), stem);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_path_not_exists() {
        let map = path("/zawk_not_exists/b/c.tar.gz");
        assert_eq!(map.get(&Str::from("exists")).to_string(), "0");
        assert_eq!(map.get(&Str::from("full_path")).to_string(), "");
        assert_eq!(
            map.get(&Str::from("parent")).to_string(),
            "/zawk_not_exists/b"
        );
        assert_eq!(map.get(&Str::from("file_name")).to_string(), "c.tar.gz");
        assert_eq!(map.get(&Str::from("file_stem")).to_string(), "c.tar");
        assert_eq!(map.get(&Str::from("file_ext")).to_string(), "gz");
        assert_eq!(
            map.get(&Str::from("content_type")).to_string(),
            "application/gzip"
        );
    }

    #[test]
    fn test_current_path() {
        let map = path(".");
        println!("{}", map.get(&Str::from("full_path")).to_string());
    }

    #[test]
    fn test_getenv() {
        let value = getenv("HOME", "default");
        println!("{}", value);
    }
}
