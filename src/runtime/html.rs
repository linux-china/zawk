use crate::runtime::{IntMap, Str};
use sxd_document::parser;
use sxd_xpath::{evaluate_xpath, Value};

pub(crate) fn html_value(html_text: &str, query: &str) -> String {
    let dom = tl::parse(html_text, tl::ParserOptions::default()).unwrap();
    let parser = dom.parser();
    if let Some(node_handler) = dom.query_selector(query).expect("Failed to parse query selector").next() {
        let node = node_handler.get(parser).unwrap();
        return node.inner_text(parser).to_string();
    }
    "".to_owned()
}

pub(crate) fn html_query<'a>(html_text: &str, query: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let dom = tl::parse(html_text, tl::ParserOptions::default()).unwrap();
    let parser = dom.parser();
    for (i, node_handler) in dom.query_selector(query).expect("Failed to parse query selector").into_iter().enumerate() {
        let node = node_handler.get(parser).unwrap();
        let value = node.inner_text(parser).to_string();
        map.insert((i + 1) as i64, Str::from(value));
    }
    map
}

pub(crate) fn xml_value(xml_text: &str, xpath: &str) -> String {
    let package = parser::parse(xml_text).unwrap();
    let document = package.as_document();
    let value = evaluate_xpath(&document, xpath).unwrap();
    match value {
        Value::Boolean(bool) => { bool.to_string() }
        Value::Number(num) => { num.to_string() }
        Value::String(text) => {
            text
        }
        Value::Nodeset(node_set) => {
            node_set.iter().next().map(|node| node.string_value()).unwrap_or("".to_owned())
        }
    }
}

pub(crate) fn xml_query<'a>(xml_text: &str, xpath: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let package = parser::parse(xml_text).unwrap();
    let document = package.as_document();
    let value = evaluate_xpath(&document, xpath).unwrap();
    match value {
        Value::Boolean(bool) => {
            map.insert(1, Str::from(bool.to_string()));
        }
        Value::Number(num) => {
            map.insert(1, Str::from(num.to_string()));
        }
        Value::String(text) => {
            map.insert(1, Str::from(text));
        }
        Value::Nodeset(node_set) => {
            for (i, node) in node_set.iter().enumerate() {
                map.insert((i + 1) as i64, Str::from(node.string_value()));
            }
        }
    };
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_value() {
        let html_code = r#"<!DOCTYPE html><html lang="en"><head><title>this is title</title></head><body><div><a id="link" href="/about">About</a><span class="welcome">hello</span></div><body></html>"#;
        let query = "span.welcome";
        let result = html_value(html_code, query);
        println!("{}", result);
    }

    #[test]
    fn test_html_query() {
        let html_code = r#"<!DOCTYPE html><html lang="en"><head><title>this is title</title></head><body><div><a id="link" href="/about">About</a><span class="welcome">hello</span></div><body></html>"#;
        let query = "title";
        let result = html_query(html_code, query);
        println!("{:?}", result);
    }

    #[test]
    fn test_xml_value() {
        let xml_text = "<books><book><title>title1</title><name>name1</name></book></books>";
        println!("{}", xml_value(xml_text, "/books/book/title"));
    }

    #[test]
    fn test_xml_query() {
        let xml_text = "<books><book><title>title1</title></book><book><title>title2</title></book></books>";
        println!("{:?}", xml_query(xml_text, "//title"));
    }
}
