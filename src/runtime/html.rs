use crate::runtime::{stdlib_warning, IntMap, Str};
use sxd_document::parser;
use sxd_xpath::{evaluate_xpath, Value};

pub(crate) fn html_value(html_text: &str, query: &str) -> String {
    if !html_text.is_empty() {
        let Some(dom) = parse_html("html_value", html_text) else { return "".to_owned() };
        let parser = dom.parser();
        match dom.query_selector(query) {
            Some(mut nodes) => {
                if let Some(node) = nodes.next().and_then(|handle| handle.get(parser)) {
                    return node.inner_text(parser).to_string();
                }
            }
            None => stdlib_warning("html_value", format!("invalid query selector {:?}", query)),
        }
    }
    "".to_owned()
}

pub(crate) fn html_query<'a>(html_text: &str, query: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if !html_text.is_empty() {
        let Some(dom) = parse_html("html_query", html_text) else { return map };
        let parser = dom.parser();
        match dom.query_selector(query) {
            Some(nodes) => {
                for (i, node_handler) in nodes.enumerate() {
                    if let Some(node) = node_handler.get(parser) {
                        let value = node.inner_text(parser).to_string();
                        map.insert((i + 1) as i64, Str::from(value));
                    }
                }
            }
            None => stdlib_warning("html_query", format!("invalid query selector {:?}", query)),
        }
    }
    map
}

fn parse_html<'a>(func: &str, html_text: &'a str) -> Option<tl::VDom<'a>> {
    tl::parse(html_text, tl::ParserOptions::default())
        .map_err(|e| stdlib_warning(func, e))
        .ok()
}

/// Parses `xml_text` and evaluates `xpath` against it, returning the string values of the
/// result (one per node for a node set). Warns (once) and returns `None` on failure.
fn eval_xpath(func: &str, xml_text: &str, xpath: &str) -> Option<Vec<String>> {
    let package = match parser::parse(xml_text) {
        Ok(package) => package,
        Err(e) => {
            stdlib_warning(func, format!("invalid XML: {:?}", e));
            return None;
        }
    };
    let document = package.as_document();
    match evaluate_xpath(&document, xpath) {
        Ok(Value::Boolean(bool)) => Some(vec![bool.to_string()]),
        Ok(Value::Number(num)) => Some(vec![num.to_string()]),
        Ok(Value::String(text)) => Some(vec![text]),
        Ok(Value::Nodeset(node_set)) => Some(node_set.iter().map(|node| node.string_value()).collect()),
        Err(e) => {
            stdlib_warning(func, format!("{:?}: {}", xpath, e));
            None
        }
    }
}

pub(crate) fn xml_value(xml_text: &str, xpath: &str) -> String {
    if !xml_text.is_empty() {
        if let Some(values) = eval_xpath("xml_value", xml_text, xpath) {
            return values.into_iter().next().unwrap_or_default();
        }
    }
    "".to_owned()
}

pub(crate) fn xml_query<'a>(xml_text: &str, xpath: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if !xml_text.is_empty() {
        for (i, value) in eval_xpath("xml_query", xml_text, xpath).unwrap_or_default().into_iter().enumerate() {
            map.insert((i + 1) as i64, Str::from(value));
        }
    }
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
