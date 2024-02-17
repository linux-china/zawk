use std::str;
use csv::{ReaderBuilder, WriterBuilder};
use crate::runtime::{Float, Int, IntMap, Str};

pub fn from_csv<'a>(text: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut reader = ReaderBuilder::new()
        .has_headers(false)
        .from_reader(text.as_bytes());
    if let Some(record) = reader.records().next() {
        for (i, item) in record.unwrap().iter().enumerate() {
            map.insert((i + 1) as i64, Str::from(item.to_string()));
        }
    }
    map
}

pub fn map_int_int_to_csv(csv: &IntMap<Int>) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = csv.to_vec();
    keys.sort();
    for key in keys {
        items.push(csv.get(&key).to_string());
    }
    items.join(",")
}

pub fn map_int_float_to_csv(csv: &IntMap<Float>) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = csv.to_vec();
    keys.sort();
    for key in keys {
        items.push(csv.get(&key).to_string());
    }
    items.join(",")
}

pub fn map_int_str_to_csv(csv: &IntMap<Str>) -> String {
    let mut items: Vec<&str> = vec![];
    let mut keys = csv.to_vec();
    keys.sort();
    for key in keys {
        items.push(csv.get(&key).as_str());
    }
    vec_to_csv(&items)
}


pub fn vec_to_csv(csv: &[&str]) -> String {
    let mut wtr = WriterBuilder::new().has_headers(false).from_writer(vec![]);
    let mut record = csv::StringRecord::new();
    for value in csv {
        record.push_field(*value);
    }
    wtr.write_record(&record).unwrap();
    let bytes = wtr.into_inner().unwrap();
    str::from_utf8(&bytes[0..bytes.len() - 1]).unwrap().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_csv() {
        let csv_text = "first,second";
        let map = from_csv(csv_text);
        println!("{:?}", map);
        let csv_text2 = map_int_str_to_csv(&map);
        assert_eq!(csv_text, csv_text2);
    }

    #[test]
    fn test_vec_to_csv() {
        let items = vec!["first", "second"];
        println!("{}", vec_to_csv(&items));
    }

    #[test]
    fn test_parse() {
        let mut reader = ReaderBuilder::new()
            .has_headers(false)
            .from_reader("Libing Chen,first".as_bytes());
        let record = reader.records().next().unwrap().unwrap();
        for item in record.iter() {
            println!("{}", item);
        }
    }

    #[test]
    fn test_write() {
        let mut wtr = WriterBuilder::new().has_headers(false).from_writer(vec![]);
        let line = vec!["first", "se,cond"];
        wtr.write_record(&line).unwrap();
        let bytes = wtr.into_inner().unwrap();
        let data = str::from_utf8(&bytes[0..bytes.len() - 1]).unwrap();
        println!("{}", data);
    }
}