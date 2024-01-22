use reqwest::blocking::Response;
use reqwest::header::{HeaderMap, HeaderName};
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
    resp_obj.insert(Str::from("status"), Str::from(status.to_string()));
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
}