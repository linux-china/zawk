//! Reading input files from `http://` and `https://` URLs.
use std::io;
use std::sync::OnceLock;
use std::time::Duration;

pub fn is_http_url(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

/// The client shared by all input URLs. The body of a response is streamed as the program reads
/// it, so there is no overall timeout (the default of 30s would cut off large downloads).
fn client() -> io::Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    crate::runtime::tls::install_default_provider();
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("zawk/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .timeout(None)
        .build()
        .map_err(io::Error::other)?;
    Ok(CLIENT.get_or_init(|| client))
}

fn open_error(url: &str, e: impl std::fmt::Display) -> io::Error {
    let msg = format!("cannot open file `{}' for reading: {}", url, e);
    io::Error::other(crate::common::InputOpenError(msg))
}

/// Sends a GET request for `url`; the returned response streams the body. Error statuses (404,
/// proxy errors, ...) are reported as errors rather than read as input.
pub fn open(url: &str) -> io::Result<reqwest::blocking::Response> {
    client()?
        .get(url)
        .send()
        .and_then(|resp| resp.error_for_status())
        .map_err(|e| open_error(url, without_url(e)))
}

/// Reads the whole body at `url` into memory (for formats that need random access, such as
/// Parquet).
pub fn read_bytes(url: &str) -> io::Result<bytes::Bytes> {
    open(url)?.bytes().map_err(|e| open_error(url, without_url(e)))
}

// The URL is already part of the message.
fn without_url(e: reqwest::Error) -> reqwest::Error {
    e.without_url()
}
