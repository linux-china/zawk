use std::str;
use csv::{ReaderBuilder, WriterBuilder};
use prometheus_parse::{Labels, Value};
use crate::runtime::{Float, Int, IntMap, Str};
use crate::runtime::str_escape::escape_csv;

pub(crate) fn from_csv<'a>(text: &str) -> IntMap<Str<'a>> {
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

pub(crate) fn map_int_int_to_csv(csv: &IntMap<Int>) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = csv.to_vec();
    keys.sort();
    for key in keys {
        items.push(csv.get(&key).to_string());
    }
    items.join(",")
}

pub(crate) fn map_int_float_to_csv(csv: &IntMap<Float>) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = csv.to_vec();
    keys.sort();
    for key in keys {
        items.push(csv.get(&key).to_string());
    }
    items.join(",")
}

pub(crate) fn map_int_str_to_csv(csv: &IntMap<Str>) -> String {
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

pub fn parse_prometheus(url_or_file: &str) -> String {
    if url_or_file.starts_with("http://") || url_or_file.starts_with("https://") {
        let body = reqwest::blocking::get(url_or_file).unwrap().text().unwrap();
        return parse_prometheus_text(&body);
    } else {
        let text = std::fs::read_to_string(url_or_file).unwrap();
        return parse_prometheus_text(&text);
    }
}

pub fn parse_prometheus_text(text: &str) -> String {
    let mut items = vec!["name, labels, type, value1, value2".to_owned()];
    let lines: Vec<_> = text.lines().map(|s| Ok(s.to_string())).collect();
    let metrics = prometheus_parse::Scrape::parse(lines.into_iter()).unwrap();
    for metric in metrics.samples {
        let labels = if metric.labels.is_empty() {
            "".to_owned()
        } else {
            escape_csv(&labels_to_string(&metric.labels))
        };
        match metric.value {
            Value::Counter(counter) => {
                items.push(format!("{}, {}, counter, {},", metric.metric, labels, counter));
            }
            Value::Gauge(gauge) => {
                items.push(format!("{}, {}, gauge, {},", metric.metric, labels, gauge));
            }
            Value::Histogram(histogram) => {
                let histogram_count = histogram.get(0).unwrap();
                items.push(format!("{}, {}, histogram, {}, {}", metric.metric, labels, histogram_count.less_than, histogram_count.count));
            }
            Value::Summary(summary) => {
                let summary_count = summary.get(0).unwrap();
                items.push(format!("{}, {}, summary, {}, {}", metric.metric, labels, summary_count.count, summary_count.quantile));
            }
            Value::Untyped(num) => {
                items.push(format!("{}, {}, untyped, {},", metric.metric, labels, num));
            }
        }
    }
    items.join("\n")
}

fn labels_to_string(labels: &Labels) -> String {
    let mut items = vec![];
    for (key, value) in labels.iter() {
        items.push(format!("{}=\"{}\"", key, value));
    }
    format!("{{{}}}", items.join(","))
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

    #[test]
    fn test_parse_prometheus() {
        let csv = parse_prometheus("http://localhost:8081/actuator/prometheus");
        println!("{}", csv);
    }
}