use minio::s3::builders::ObjectContent;
use minio::s3::MinioClient;
use minio::s3::creds::StaticProvider;
use minio::s3::http::BaseUrl;
use minio::s3::response::{PutObjectContentResponse};
use minio::s3::types::{Region, S3Api};
use std::sync::OnceLock;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

static S3_CLIENT: OnceLock<MinioClient> = OnceLock::new();

fn env_var(name: &str) -> Result<String, BoxError> {
    std::env::var(name).map_err(|_| format!("environment variable {} is not set", name).into())
}

fn build_s3_client() -> Result<MinioClient, BoxError> {
    let s3_endpoint = env_var("S3_ENDPOINT")?;
    let s3_access_key = env_var("S3_ACCESS_KEY_ID")?;
    let s3_access_secret = env_var("S3_ACCESS_KEY_SECRET")?;
    let s3_region = env_var("S3_REGION")?;
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

pub fn get_object(bucket_name: &str, object_name: &str) -> Result<String, BoxError> {
    let client = s3_client()?;
    crate::runtime::TOKIO_RUNTIME.block_on(async {
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
    crate::runtime::TOKIO_RUNTIME.block_on(async {
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
