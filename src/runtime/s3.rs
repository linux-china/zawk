use minio::s3::builders::ObjectContent;
use minio::s3::client::Client;
use minio::s3::creds::StaticProvider;
use minio::s3::error::Error;
use minio::s3::http::BaseUrl;
use minio::s3::response::{PutObjectContentResponse};
use minio::s3::types::S3Api;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

fn s3_client() -> Result<Client, Error> {
    let s3_endpoint = std::env::var("S3_ENDPOINT").unwrap();
    let s3_access_key = std::env::var("S3_ACCESS_KEY_ID").unwrap();
    let s3_access_secret = std::env::var("S3_ACCESS_KEY_SECRET").unwrap();
    let s3_region = std::env::var("S3_REGION").unwrap();
    let mut base_url = s3_endpoint.parse::<BaseUrl>()?;
    base_url.region = s3_region;
    let static_provider = StaticProvider::new(&s3_access_key, &s3_access_secret, None);
    let client = Client::new(
        base_url.clone(),
        Some(Box::new(static_provider)),
        None,
        None,
    )?;
    Ok(client)
}

pub fn get_object(
    bucket_name: &str,
    object_name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = s3_client().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let response = client.get_object(bucket_name, object_name).send().await?;
        let content = response.content.to_segmented_bytes().await?.to_bytes();
        let result = String::from_utf8(content.to_vec()).unwrap();
        Ok(result)
    })
}

pub fn put_object(
    bucket_name: &str,
    object_name: &str,
    body: &str,
) -> Result<PutObjectContentResponse, Box<dyn std::error::Error + Send + Sync>> {
    let client = s3_client().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut file = NamedTempFile::new().unwrap();
        let _ = file.write_all(body.as_bytes());
        let file_path = file.path().to_str().unwrap().to_string();
        // upload args
        let content = ObjectContent::from(Path::new(file_path.as_str()));
        let response = client
            .put_object_content(bucket_name, object_name, content)
            .send()
            .await
            .unwrap();
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
    fn test_s3_get() {
        dotenv::dotenv().ok();
        let text = get_object(BUCKET, OBJECT_NAME).unwrap();
        assert_eq!(text, BODY);
    }

    #[test]
    fn test_s3_put() {
        dotenv::dotenv().ok();
        let _ = put_object(BUCKET, OBJECT_NAME, BODY).unwrap();
        let text = get_object(BUCKET, OBJECT_NAME).unwrap();
        assert_eq!(text, BODY);
    }
}
