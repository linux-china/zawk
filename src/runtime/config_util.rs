use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use anyhow::Result;
use ini::configparser::ini::Ini;
use crate::runtime::{stdlib_warning, Str, StrMap};

pub(crate) fn read_config<'b>(file: &str) -> StrMap<'b, Str<'b>> {
    let mut map = hashbrown::HashMap::new();
    if file.ends_with(".ini") {
        match read_ini(file) {
            Ok(data_map) => {
                for (key, value) in data_map {
                    map.insert(Str::from(key), Str::from(value));
                }
            }
            Err(e) => stdlib_warning("read_config", format!("{}: {}", file, e)),
        }
    } else if file.ends_with(".properties") {
        match read_properties(file) {
            Ok(data_map) => {
                for (key, value) in data_map {
                    map.insert(Str::from(key), Str::from(value));
                }
            }
            Err(e) => stdlib_warning("read_config", format!("{}: {}", file, e)),
        }
    }
    StrMap::from(map)
}

fn read_ini(ini_file: &str) -> Result<HashMap<String, String>> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut config = Ini::new();
    // You can easily load a file to get a clone of the map:
    let root_map = config.load(ini_file).map_err(anyhow::Error::msg)?;
    root_map.iter().for_each(|(root_key, child_map)| {
        child_map.iter().for_each(|(key, v)| {
            if let Some(value) = v {
                map.insert(format!("{}.{}", root_key, key), value.clone());
                if root_key == "default" {
                    map.insert(key.clone(), value.clone());
                }
            }
        });
    });
    Ok(map)
}

fn read_properties(properties_file: &str) -> Result<HashMap<String, String>> {
    let f2 = File::open(properties_file)?;
    Ok(java_properties::read(BufReader::new(f2))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_ini() {
        let map = read_ini("tests/demo.ini").unwrap();
        println!("{:?}", map);
    }

    #[test]
    fn test_read_properties() {
        let map = read_properties("tests/demo.properties").unwrap();
        println!("{:?}", map);
    }
}
