use std::str;
use csv::{ReaderBuilder, WriterBuilder};
use prometheus_parse::{Labels, Value};
use crate::runtime::{stdlib_warning, Float, Int, IntMap, Str};
use crate::runtime::str_escape::escape_csv;

pub(crate) fn from_csv<'a>(text: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut reader = ReaderBuilder::new()
        .has_headers(false)
        .from_reader(text.as_bytes());
    match reader.records().next() {
        Some(Ok(record)) => {
            for (i, item) in record.iter().enumerate() {
                map.insert((i + 1) as i64, Str::from(item.to_string()));
            }
        }
        Some(Err(e)) => stdlib_warning("from_csv", e),
        None => {}
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
    let mut keys = csv.to_vec();
    keys.sort();
    let values: Vec<Str> = keys.iter().map(|key| csv.get(key)).collect();
    let items: Vec<_> = values.iter().map(|value| value.as_str()).collect();
    let items: Vec<&str> = items.iter().map(|item| item.as_ref()).collect();
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
    let text = if url_or_file.starts_with("http://") || url_or_file.starts_with("https://") {
        reqwest::blocking::get(url_or_file).and_then(|resp| resp.text()).map_err(|e| e.to_string())
    } else {
        std::fs::read_to_string(url_or_file).map_err(|e| e.to_string())
    };
    match text {
        Ok(text) => parse_prometheus_text(&text),
        Err(e) => {
            stdlib_warning("parse_prometheus", format!("{}: {}", url_or_file, e));
            String::new()
        }
    }
}

pub fn parse_prometheus_text(text: &str) -> String {
    let mut items = vec!["name, labels, type, value1, value2".to_owned()];
    let lines: Vec<_> = text.lines().map(|s| Ok(s.to_string())).collect();
    let metrics = match prometheus_parse::Scrape::parse(lines.into_iter()) {
        Ok(metrics) => metrics,
        Err(e) => {
            stdlib_warning("parse_prometheus", e);
            return String::new();
        }
    };
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
                if let Some(histogram_count) = histogram.first() {
                    items.push(format!("{}, {}, histogram, {}, {}", metric.metric, labels, histogram_count.less_than, histogram_count.count));
                }
            }
            Value::Summary(summary) => {
                if let Some(summary_count) = summary.first() {
                    items.push(format!("{}, {}, summary, {}, {}", metric.metric, labels, summary_count.count, summary_count.quantile));
                }
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
    #[ignore]
    fn test_parse_prometheus() {
        let csv = parse_prometheus("http://localhost:8081/actuator/prometheus");
        println!("{}", csv);
    }
}