use bytes::Bytes;
use futures_util::StreamExt;
use minio::s3::builders::ObjectContent;
use minio::s3::MinioClient;
use minio::s3::creds::StaticProvider;
use minio::s3::http::BaseUrl;
use minio::s3::response::{PutObjectContentResponse};
use minio::s3::types::{Region, S3Api};
use std::io;
use std::sync::{LazyLock, OnceLock};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

static S3_CLIENT: OnceLock<MinioClient> = OnceLock::new();

/// All S3 requests run on this runtime. Its worker threads keep downloading objects read as
/// input (`zawk '...' s3://bucket/key`) while the program processes the bytes received so far,
/// and the pooled connections of the client stay bound to one runtime.
static S3_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("zawk-s3")
        .enable_all()
        .build()
        .expect("failed to build S3 runtime")
});

/// The value of the first environment variable of `names` that is set: the `S3_*` names, then
/// the AWS style ones.
fn env_any(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

fn env_required(names: &[&str]) -> Result<String, BoxError> {
    env_any(names).ok_or_else(|| format!("environment variable {} is not set", names.join(" or ")).into())
}

fn build_s3_client() -> Result<MinioClient, BoxError> {
    let s3_endpoint = env_required(&["S3_ENDPOINT", "S3_ENDPOINT_URL", "AWS_ENDPOINT_URL", "AWS_ENDPOINT"])?;
    let s3_access_key = env_required(&["S3_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID"])?;
    let s3_access_secret = env_required(&["S3_ACCESS_KEY_SECRET", "AWS_SECRET_ACCESS_KEY"])?;
    let s3_region = env_required(&["S3_REGION", "AWS_REGION", "AWS_DEFAULT_REGION"])?;
    let mut base_url = s3_endpoint.parse::<BaseUrl>()?;
    base_url.region = Region::new(s3_region.as_str())?;
    let static_provider = StaticProvider::new(&s3_access_key, &s3_access_secret, None);
    let client = MinioClient::new(base_url, Some(static_provider), None, None)?;
    Ok(client)
}

/// Returns a cached S3 client; it is built on first successful call and reused afterwards.
fn s3_client() -> Result<&'static MinioClient, BoxError> {
    if let Some(client) = S3_CLIENT.get() {
        return Ok(client);
    }
    let client = build_s3_client()?;
    Ok(S3_CLIENT.get_or_init(|| client))
}

pub fn is_s3_url(path: &str) -> bool {
    path.starts_with("s3://")
}

/// Splits `s3://bucket/key` into the bucket and the object key.
pub fn parse_s3_url(url: &str) -> Result<(&str, &str), String> {
    match url.strip_prefix("s3://").and_then(|rest| rest.split_once('/')) {
        Some((bucket, key)) if !bucket.is_empty() && !key.is_empty() => Ok((bucket, key)),
        _ => Err(format!("invalid S3 URL `{}': expected s3://bucket/key", url)),
    }
}

fn read_error(url: &str, e: &(dyn std::error::Error + 'static)) -> String {
    format!("cannot read `{}': {}", url, describe(e))
}

/// A one-line description of an S3 error: the error code and message returned by the server,
/// or the error with its causes (the client's errors only say e.g. "Network error occurred").
fn describe(e: &(dyn std::error::Error + 'static)) -> String {
    use minio::s3::error::{Error, S3ServerError};
    if let Some(Error::S3Server(S3ServerError::S3Error(response))) = e.downcast_ref::<Error>() {
        let code = response.code().to_string();
        return match response.message() {
            Some(message) if !message.is_empty() => format!("{}: {}", code, message),
            _ => code,
        };
    }
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// Checks up front that the object at `url` can be read (the S3 settings are complete, the
/// object exists and access is granted), so that problems are reported clearly before any input
/// is processed. Returns the size of the object.
pub fn check_object(url: &str) -> Result<u64, String> {
    let (bucket, key) = parse_s3_url(url)?;
    let client = s3_client().map_err(|e| read_error(url, &*e))?;
    S3_RUNTIME
        .block_on(async {
            let response = client.stat_object(bucket, key)?.build().send().await?;
            Ok::<_, BoxError>(response.size()?)
        })
        .map_err(|e| read_error(url, &*e))
}

/// Reads the whole object at `url` into memory (for formats that need random access, such as
/// Parquet).
pub fn read_object_bytes(url: &str) -> io::Result<Bytes> {
    let (bucket, key) = parse_s3_url(url).map_err(io::Error::other)?;
    let client = s3_client().map_err(|e| io::Error::other(read_error(url, &*e)))?;
    S3_RUNTIME
        .block_on(async {
            let response = client.get_object(bucket, key)?.build().send().await?;
            Ok::<_, BoxError>(response.into_bytes().await?)
        })
        .map_err(|e| io::Error::other(read_error(url, &*e)))
}

/// Objects are downloaded in parts of this size, with ranged GETs.
const PART_SIZE: u64 = 8 * 1024 * 1024;
/// The number of parts downloaded concurrently: a single S3 connection is usually much slower
/// than the program reading the data (e.g. ~100MB/s on AWS).
const PARALLEL_PARTS: usize = 4;
/// The number of downloaded parts waiting for the reader.
const READY_PARTS: usize = 2;

/// Streams an S3 object as a `Read`. The download starts on the first read and runs on the S3
/// runtime ahead of the reader, so network transfer and processing overlap: parts of the object
/// are fetched in parallel and handed to the reader in order. Memory use is bounded (about
/// `(PARALLEL_PARTS + READY_PARTS + 1) * PART_SIZE`) for objects of any size.
pub struct S3Reader {
    url: String,
    parts: Option<tokio::sync::mpsc::Receiver<io::Result<Bytes>>>,
    part: Bytes,
}

impl S3Reader {
    pub fn new(url: impl Into<String>) -> S3Reader {
        S3Reader { url: url.into(), parts: None, part: Bytes::new() }
    }

    fn start(&self) -> io::Result<tokio::sync::mpsc::Receiver<io::Result<Bytes>>> {
        let url = self.url.clone();
        let (bucket, key) = parse_s3_url(&url).map_err(io::Error::other)?;
        let (bucket, key) = (bucket.to_string(), key.to_string());
        let client = s3_client().map_err(|e| io::Error::other(read_error(&url, &*e)))?;
        let (tx, rx) = tokio::sync::mpsc::channel(READY_PARTS);
        S3_RUNTIME.spawn(async move {
            if let Err(e) = download(client, bucket, key, &tx).await {
                let _ = tx.send(Err(io::Error::other(read_error(&url, &*e)))).await;
            }
        });
        Ok(rx)
    }
}

/// Sends the parts of an object to `tx`, in order; stops early when the reader is gone (e.g. the
/// program exited).
async fn download(
    client: &MinioClient,
    bucket: String,
    key: String,
    tx: &tokio::sync::mpsc::Sender<io::Result<Bytes>>,
) -> Result<(), BoxError> {
    use minio::s3::response_traits::HasEtagFromHeaders;
    let stat = client.stat_object(&bucket, &key)?.build().send().await?;
    let size = stat.size()?;
    // All parts must come from the same version of the object.
    let etag = stat.etag()?.to_string();
    let parts = (0..size).step_by(PART_SIZE as usize).map(|offset| {
        let (bucket, key, etag) = (bucket.clone(), key.clone(), etag.clone());
        async move {
            let response = client
                .get_object(bucket, key)?
                .offset(offset)
                .length(PART_SIZE.min(size - offset))
                .match_etag(etag)
                .build()
                .send()
                .await?;
            Ok::<_, BoxError>(response.into_bytes().await?)
        }
    });
    let mut parts = futures_util::stream::iter(parts).buffered(PARALLEL_PARTS);
    while let Some(part) = parts.next().await {
        if tx.send(Ok(part?)).await.is_err() {
            break;
        }
    }
    Ok(())
}

impl io::Read for S3Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.part.is_empty() {
            if self.parts.is_none() {
                self.parts = Some(self.start()?);
            }
            match self.parts.as_mut().unwrap().blocking_recv() {
                Some(part) => self.part = part?,
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.part.len());
        buf[..n].copy_from_slice(&self.part[..n]);
        bytes::Buf::advance(&mut self.part, n);
        Ok(n)
    }
}

pub fn get_object(bucket_name: &str, object_name: &str) -> Result<String, BoxError> {
    let client = s3_client()?;
    S3_RUNTIME.block_on(async {
        let response = client.get_object(bucket_name, object_name)?.build().send().await?;
        let content = response.content()?.to_segmented_bytes().await?.to_bytes();
        let result = String::from_utf8(content.to_vec())?;
        Ok(result)
    })
}

pub fn put_object(
    bucket_name: &str,
    object_name: &str,
    body: &str,
) -> Result<PutObjectContentResponse, BoxError> {
    let client = s3_client()?;
    let content = ObjectContent::from(body.to_string());
    S3_RUNTIME.block_on(async {
        let response = client
            .put_object_content(bucket_name, object_name, content)?
            .build()
            .send()
            .await?;
        Ok(response)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUCKET: &str = "mj-artifacts";
    const OBJECT_NAME: &str = "health2.txt";
    const BODY: &str = "Hello World!!!";

    #[test]
    fn test_parse_s3_url() {
        assert_eq!(parse_s3_url("s3://bucket1/demo.csv"), Ok(("bucket1", "demo.csv")));
        assert_eq!(parse_s3_url("s3://bucket1/a/b/c.csv"), Ok(("bucket1", "a/b/c.csv")));
        assert!(parse_s3_url("s3://bucket1").is_err());
        assert!(parse_s3_url("s3://bucket1/").is_err());
        assert!(parse_s3_url("s3:///demo.csv").is_err());
        assert!(parse_s3_url("/tmp/demo.csv").is_err());
    }

    /// Reads an object larger than a part, which is downloaded in parallel parts.
    #[test]
    #[ignore]
    fn test_s3_reader_parts() {
        use std::io::Read;
        dotenv::dotenv().ok();
        let body: String = (0..1_500_000).map(|i| format!("{}\n", i)).collect();
        assert!(body.len() as u64 > PART_SIZE);
        put_object("bucket1", "parts.txt", &body).unwrap();
        let mut text = String::new();
        S3Reader::new("s3://bucket1/parts.txt").read_to_string(&mut text).unwrap();
        assert_eq!(text, body);
    }

    #[test]
    #[ignore]
    fn test_s3_get() {
        dotenv::dotenv().ok();
        let text = get_object(BUCKET, OBJECT_NAME).unwrap();
        assert_eq!(text, BODY);
    }

    #[test]
    #[ignore]
    fn test_s3_put() {
        dotenv::dotenv().ok();
        let _ = put_object(BUCKET, OBJECT_NAME, BODY).unwrap();
        let text = get_object(BUCKET, OBJECT_NAME).unwrap();
        assert_eq!(text, BODY);
    }
}
