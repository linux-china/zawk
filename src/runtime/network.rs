use std::collections::HashMap;
use std::sync::Mutex;
use lazy_static::lazy_static;
use reqwest::blocking::Response;
use reqwest::header::{HeaderMap, HeaderName};
use url::Url;
use crate::runtime::{Str, StrMap};

pub fn local_ip() -> String {
    if let Ok(my_ip) = local_ip_address::local_ip() {
        return my_ip.to_string();
    }
    "127.0.0.1".to_owned()
}

pub(crate) fn http_get<'a>(url: &str, headers: &StrMap<'a, Str<'a>>) -> StrMap<'a, Str<'a>> {
    use reqwest::blocking::Client;
    let client = Client::new();
    let resp_obj: StrMap<Str> = StrMap::default();
    let mut builder = client.get(url);
    if headers.len() > 0 {
        builder = builder.headers(convert_to_http_headers(headers));
    }
    if let Ok(resp) = builder.send() {
        fill_response(resp, &resp_obj);
    } else {
        resp_obj.insert(Str::from("status"), Str::from("0"));
    }
    resp_obj
}


pub(crate) fn http_post<'a>(url: &str, headers: &StrMap<'a, Str<'a>>, body: &Str) -> StrMap<'a, Str<'a>> {
    use reqwest::blocking::Client;
    let client = Client::new();
    let resp_obj: StrMap<Str> = StrMap::default();
    let mut builder = client.post(url);
    if headers.len() > 0 {
        builder = builder.headers(convert_to_http_headers(headers));
    }
    if !body.is_empty() {
        builder = builder.body(body.to_string());
    }
    if let Ok(resp) = builder.send() {
        fill_response(resp, &resp_obj);
    } else {
        resp_obj.insert(Str::from("status"), Str::from("0"));
    }
    resp_obj
}

fn convert_to_http_headers<'a>(headers: &StrMap<'a, Str<'a>>) -> HeaderMap {
    let mut request_headers = HeaderMap::new();
    for name in &headers.to_vec() {
        request_headers.insert(HeaderName::from_bytes(name.to_string().as_bytes()).unwrap(), headers.get(name).to_string().parse().unwrap());
    }
    request_headers
}

fn fill_response(resp: Response, resp_obj: &StrMap<Str>) {
    let status = resp.status();
    resp_obj.insert(Str::from("status"), Str::from(status.as_u16().to_string()));
    let response_headers = resp.headers();
    for (name, value) in response_headers.into_iter() {
        resp_obj.insert(Str::from(name.to_string()), Str::from(value.to_str().unwrap().to_string()));
    }
    if let Ok(body) = resp.text() {
        if !body.is_empty() {
            resp_obj.insert(Str::from("text"), Str::from(body.clone()));
        }
    }
}

// todo graceful shutdown
lazy_static! {
    static ref NATS_CONNECTIONS: Mutex<HashMap<String, nats::Connection>> = Mutex::new(HashMap::new());
}

pub(crate) fn publish(namespace: &str, body: &str) {
    if namespace.starts_with("nats://") || namespace.starts_with("nats+tls://") {
        if let Ok(url) = &Url::parse(namespace) {
            let schema = url.scheme();
            let topic = if url.path().starts_with('/') {
                url.path()[1..].to_string()
            } else {
                url.path().to_string()
            };
            let conn_url = if schema.contains("tls") {
                format!("tls://{}:{}", url.host().unwrap(), url.port().unwrap_or(4443))
            } else {
                format!("{}:{}", url.host().unwrap(), url.port().unwrap_or(4222))
            };
            let mut pool = NATS_CONNECTIONS.lock().unwrap();
            let nc = pool.entry(conn_url.clone()).or_insert_with(|| {
                nats::connect(&conn_url).unwrap()
            });
            nc.publish(&topic, body).unwrap();
        }
    } else {
        notify_rust::Notification::new()
            .summary(namespace)
            .body(body)
            .show().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use local_ip_address::local_ip;
    use super::*;

    #[test]
    fn test_local_ip() {
        let my_local_ip = local_ip().unwrap();
        println!("This is my local IP address: {:?}", my_local_ip);
    }

    #[test]
    fn test_http_get() {
        let url = "https://httpbin.org/ip";
        let headers: StrMap<Str> = StrMap::default();
        let resp = http_get(url, &headers);
        println!("{}", resp.get(&Str::from("text")));
    }

    #[test]
    fn test_http_post() {
        let url = "https://httpbin.org/post";
        let headers: StrMap<Str> = StrMap::default();
        let body = Str::from("Hello");
        let resp = http_post(url, &headers, &body);
        println!("{}", resp.get(&Str::from("text")));
    }

    #[test]
    fn test_publish_nats() {
        let url = "nats://localhost:4222/topic1";
        publish(url, "Hello World!");
    }
}
