use crate::runtime::str_escape::escape_csv;
use crate::runtime::{Float, Int, IntMap, Str, stdlib_warning};
use csv::{ReaderBuilder, WriterBuilder};
use prometheus_parse::{Labels, Value};
use std::str;

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

/// Max number of empty cells filled in for a sparse array; beyond it, keys are just sorted.
const MAX_CSV_GAP: i64 = 1 << 20;

/// Column positions for `to_csv`: `Some(key)` for an existing index, `None` for a hole.
/// Arrays indexed from 1 are emitted as `1..=max` so holes keep their position (`a,,c`).
fn csv_positions(mut keys: Vec<Int>) -> Vec<Option<Int>> {
    keys.sort_unstable();
    match (keys.first(), keys.last()) {
        (Some(&min), Some(&max)) if min >= 1 && max - (keys.len() as Int) <= MAX_CSV_GAP => {
            let mut positions = Vec::with_capacity(max as usize);
            let mut iter = keys.iter().peekable();
            for i in 1..=max {
                if iter.peek() == Some(&&i) {
                    iter.next();
                    positions.push(Some(i));
                } else {
                    positions.push(None);
                }
            }
            positions
        }
        _ => keys.into_iter().map(Some).collect(),
    }
}

pub(crate) fn map_int_int_to_csv(csv: &IntMap<Int>) -> String {
    let items: Vec<String> = csv_positions(csv.to_vec())
        .into_iter()
        .map(|key| key.map(|k| csv.get(&k).to_string()).unwrap_or_default())
        .collect();
    items.join(",")
}

pub(crate) fn map_int_float_to_csv(csv: &IntMap<Float>) -> String {
    let items: Vec<String> = csv_positions(csv.to_vec())
        .into_iter()
        .map(|key| key.map(|k| csv.get(&k).to_string()).unwrap_or_default())
        .collect();
    items.join(",")
}

pub(crate) fn map_int_str_to_csv(csv: &IntMap<Str>) -> String {
    let values: Vec<Str> = csv_positions(csv.to_vec())
        .into_iter()
        .map(|key| key.map(|k| csv.get(&k)).unwrap_or_default())
        .collect();
    let items: Vec<_> = values.iter().map(|value| value.as_str()).collect();
    let items: Vec<&str> = items.iter().map(|item| item.as_ref()).collect();
    vec_to_csv(&items)
}

pub fn vec_to_csv(csv: &[&str]) -> String {
    // A zero-field record has no CSV representation; csv-core would emit `""`.
    if csv.is_empty() {
        return String::new();
    }
    let mut wtr = WriterBuilder::new().has_headers(false).from_writer(vec![]);
    let mut record = csv::StringRecord::new();
    for value in csv {
        record.push_field(*value);
    }
    wtr.write_record(&record).unwrap();
    let bytes = wtr.into_inner().unwrap();
    str::from_utf8(&bytes[0..bytes.len() - 1])
        .unwrap()
        .to_string()
}

pub fn parse_prometheus(url_or_file: &str) -> String {
    let text = if url_or_file.starts_with("http://") || url_or_file.starts_with("https://") {
        crate::runtime::network::fetch_text(url_or_file)
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
    let mut items = vec!["name,labels,type,value1,value2".to_owned()];
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
                items.push(format!(
                    "{},{},counter,{},",
                    metric.metric, labels, counter
                ));
            }
            Value::Gauge(gauge) => {
                items.push(format!("{},{},gauge,{},", metric.metric, labels, gauge));
            }
            // One row per bucket / quantile: `value1` is the bucket bound or the quantile,
            // `value2` the count or the value at that quantile.
            Value::Histogram(histogram) => {
                for bucket in histogram {
                    items.push(format!(
                        "{},{},histogram,{},{}",
                        metric.metric, labels, bucket.less_than, bucket.count
                    ));
                }
            }
            Value::Summary(summary) => {
                for quantile in summary {
                    items.push(format!(
                        "{},{},summary,{},{}",
                        metric.metric, labels, quantile.quantile, quantile.count
                    ));
                }
            }
            Value::Untyped(num) => {
                items.push(format!("{},{},untyped,{},", metric.metric, labels, num));
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
    fn test_to_csv_sparse() {
        let map: IntMap<Str> = IntMap::default();
        map.insert(1, Str::from("a"));
        map.insert(3, Str::from("c"));
        assert_eq!(map_int_str_to_csv(&map).trim_end(), "a,,c");
        let map: IntMap<Int> = IntMap::default();
        map.insert(1, 1);
        map.insert(4, 4);
        assert_eq!(map_int_int_to_csv(&map), "1,,,4");
        // non-positive keys fall back to a compact, sorted output
        let map: IntMap<Int> = IntMap::default();
        map.insert(0, 0);
        map.insert(2, 2);
        assert_eq!(map_int_int_to_csv(&map), "0,2");
    }

    #[test]
    fn test_vec_to_csv() {
        let items = vec!["first", "second"];
        assert_eq!(vec_to_csv(&items), "first,second");
        assert_eq!(vec_to_csv(&[]), "");
        let map: IntMap<Str> = IntMap::default();
        assert_eq!(map_int_str_to_csv(&map), "");
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

    #[test]
    fn test_parse_prometheus_text_csv_columns() {
        let text = "http_requests_total{method=\"get\",code=\"200\"} 1027\nup 1\n";
        let csv = parse_prometheus_text(text);
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(csv.as_bytes());
        let headers: Vec<String> = rdr.headers().unwrap().iter().map(String::from).collect();
        assert_eq!(headers, ["name", "labels", "type", "value1", "value2"]);
        let rows: Vec<Vec<String>> = rdr
            .records()
            .map(|r| r.unwrap().iter().map(String::from).collect())
            .collect();
        assert_eq!(rows[0].len(), 5);
        assert_eq!(rows[0][0], "http_requests_total");
        assert!(rows[0][1].contains("method=\"get\"") && rows[0][1].contains("code=\"200\""));
        assert_eq!(rows[0][2..], ["untyped", "1027", ""]);
        assert_eq!(rows[1], ["up", "", "untyped", "1", ""]);
    }

    #[test]
    fn test_parse_prometheus_histogram_summary_all_points() {
        let text = "# TYPE lat histogram\n\
            lat_bucket{le=\"0.1\"} 1\nlat_bucket{le=\"0.5\"} 3\n\
            lat_bucket{le=\"1\"} 4\nlat_bucket{le=\"+Inf\"} 5\n\
            # TYPE rpc summary\n\
            rpc{quantile=\"0.5\"} 10\nrpc{quantile=\"0.9\"} 20\nrpc{quantile=\"0.99\"} 30\n";
        let csv = parse_prometheus_text(text);
        // Metrics come out in hash order; buckets and quantiles keep theirs within a metric.
        let mut rows: Vec<&str> = csv
            .lines()
            .filter(|l| l.contains(",histogram,") || l.contains(",summary,"))
            .collect();
        rows.sort_by_key(|l| l.starts_with("rpc,"));
        assert_eq!(
            rows,
            [
                "lat,,histogram,0.1,1",
                "lat,,histogram,0.5,3",
                "lat,,histogram,1,4",
                "lat,,histogram,inf,5",
                "rpc,,summary,0.5,10",
                "rpc,,summary,0.9,20",
                "rpc,,summary,0.99,30",
            ],
            "{csv}"
        );
    }
}
