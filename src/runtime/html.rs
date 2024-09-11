use crate::runtime::{IntMap, Str};

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
}
