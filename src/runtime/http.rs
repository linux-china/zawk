//! Reading input files from `http://` and `https://` URLs.
use std::io::{self, Read};
use std::sync::OnceLock;

pub fn is_http_url(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

/// The client shared by all input URLs. The body of a response is streamed as the program reads
/// it: the blocking client applies `timeout` to sending the request and to each read of the body
/// separately, so it bounds idle time rather than the whole download, and large files still work.
fn client() -> io::Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    crate::runtime::tls::install_default_provider();
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("zawk/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(crate::runtime::network::CONNECT_TIMEOUT)
        .timeout(crate::runtime::network::READ_TIMEOUT)
        .build()
        .map_err(io::Error::other)?;
    Ok(CLIENT.get_or_init(|| client))
}

fn open_error(url: &str, e: impl std::fmt::Display) -> io::Error {
    let msg = format!(
        "cannot open file `{}' for reading: {}",
        crate::runtime::network::redact_url(url),
        e
    );
    io::Error::other(crate::common::InputOpenError(msg))
}

/// Sends a GET request for `url`; the returned response streams the body. Error statuses (404,
/// proxy errors, ...) are reported as errors rather than read as input.
pub fn open(url: &str) -> io::Result<Body> {
    client()?
        .get(url)
        .send()
        .and_then(|resp| resp.error_for_status())
        .map(|resp| Body {
            resp,
            url: crate::runtime::network::redact_url(url).into_owned(),
        })
        .map_err(|e| open_error(url, without_url(e)))
}

/// The streamed body of a response. A read fails when the server sends nothing for
/// `READ_TIMEOUT`; that error names the URL, instead of reqwest's "error decoding response body".
pub struct Body {
    resp: reqwest::blocking::Response,
    url: String,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.resp.read(buf).map_err(|e| {
            let timed_out = e
                .get_ref()
                .and_then(|e| e.downcast_ref::<reqwest::Error>())
                .is_some_and(|e| e.is_timeout());
            if timed_out {
                let secs = crate::runtime::network::READ_TIMEOUT.as_secs();
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("no data from `{}' for {}s", self.url, secs),
                )
            } else {
                e
            }
        })
    }
}

/// Reads the whole body at `url` into memory (for formats that need random access, such as
/// Parquet).
/// Read in chunks rather than with `Response::bytes`, which would apply the timeout to the whole
/// download.
pub fn read_bytes(url: &str) -> io::Result<bytes::Bytes> {
    let mut data = Vec::new();
    open(url)?.read_to_end(&mut data)?;
    Ok(data.into())
}

// The URL is already part of the message.
fn without_url(e: reqwest::Error) -> reqwest::Error {
    e.without_url()
}
