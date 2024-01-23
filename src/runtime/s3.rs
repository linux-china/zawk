use minio::s3::client::Client;
use minio::s3::creds::StaticProvider;
use minio::s3::error::Error;
use minio::s3::http::BaseUrl;

fn s3_client() -> Result<Client, Error> {
    let s3_endpoint = std::env::var("S3_ENDPOINT").unwrap();
    let s3_access_key = std::env::var("S3_ACCESS_KEY_ID").unwrap();
    let s3_access_secret = std::env::var("S3_ACCESS_KEY_SECRET").unwrap();
    let s3_region = std::env::var("S3_REGION").unwrap();
    let mut base_url = s3_endpoint.parse::<BaseUrl>()?;
    base_url.region = s3_region;
    let static_provider = StaticProvider::new(
        &s3_access_key,
        &s3_access_secret,
        None,
    );
    let client = Client::new(
        base_url.clone(),
        Some(Box::new(static_provider)),
        None,
        None,
    )?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s3_get() {
        dotenv::dotenv().ok();
    }
}