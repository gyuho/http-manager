use std::{
    fs::File,
    io::{self, copy, Cursor, Error, ErrorKind},
    time::Duration,
};

use hyper::{body::Bytes, client::HttpConnector, Body, Client, Method, Request, Response};
use hyper_tls::HttpsConnector;
use reqwest::{header::CONTENT_TYPE, ClientBuilder};
use tokio::time::timeout;
use url::Url;

/// Creates a simple HTTP GET request with no header and no body.
pub fn create_get(url: &str, path: &str) -> io::Result<Request<Body>> {
    let uri = match join_uri(url, path) {
        Ok(u) => u,
        Err(e) => return Err(e),
    };

    let req = match Request::builder()
        .method(Method::GET)
        .uri(uri.as_str())
        .body(Body::empty())
    {
        Ok(r) => r,
        Err(e) => {
            return Err(Error::new(
                ErrorKind::Other,
                format!("failed to create request {}", e),
            ));
        }
    };

    Ok(req)
}

const JSON_CONTENT_TYPE: &str = "application/json";

/// Creates a simple HTTP POST request with JSON header and body.
pub fn create_json_post(url: &str, path: &str, d: &str) -> io::Result<Request<Body>> {
    let uri = join_uri(url, path)?;

    let req = match Request::builder()
        .method(Method::POST)
        .header("content-type", JSON_CONTENT_TYPE)
        .uri(uri.as_str())
        .body(Body::from(String::from(d)))
    {
        Ok(r) => r,
        Err(e) => {
            return Err(Error::new(
                ErrorKind::Other,
                format!("failed to create request {}", e),
            ));
        }
    };

    Ok(req)
}

/// Sends a HTTP request, reads response in "hyper::body::Bytes".
pub async fn read_bytes(
    req: Request<Body>,
    timeout_dur: Duration,
    is_https: bool,
    check_status_code: bool,
) -> io::Result<Bytes> {
    let resp = send_req(req, timeout_dur, is_https).await?;
    if !resp.status().is_success() {
        log::warn!(
            "unexpected HTTP response code {} (server error {})",
            resp.status(),
            resp.status().is_server_error()
        );
        if check_status_code {
            return Err(Error::new(
                ErrorKind::Other,
                format!(
                    "unexpected HTTP response code {} (server error {})",
                    resp.status(),
                    resp.status().is_server_error()
                ),
            ));
        }
    }

    // set timeouts for reads
    // https://github.com/hyperium/hyper/issues/1097
    let future_task = hyper::body::to_bytes(resp);
    let ret = timeout(timeout_dur, future_task).await;

    let bytes;
    match ret {
        Ok(result) => match result {
            Ok(b) => bytes = b,
            Err(e) => {
                return Err(Error::new(
                    ErrorKind::Other,
                    format!("failed to read response {}", e),
                ));
            }
        },
        Err(e) => {
            return Err(Error::new(
                ErrorKind::Other,
                format!("failed to read response {}", e),
            ));
        }
    }

    Ok(bytes)
}

/// Sends a HTTP(s) request and wait for its response.
async fn send_req(
    req: Request<Body>,
    timeout_dur: Duration,
    is_https: bool,
) -> io::Result<Response<Body>> {
    // ref. https://github.com/tokio-rs/tokio-tls/blob/master/examples/hyper-client.rs
    // ref. https://docs.rs/hyper/latest/hyper/client/struct.HttpConnector.html
    // ref. https://github.com/hyperium/hyper-tls/blob/master/examples/client.rs
    let mut connector = HttpConnector::new();
    // ref. https://github.com/hyperium/hyper/issues/1097
    connector.set_connect_timeout(Some(Duration::from_secs(5)));

    let task = {
        if !is_https {
            let cli = Client::builder().build(connector);
            cli.request(req)
        } else {
            // TODO: implement "curl --insecure"
            let https_connector = HttpsConnector::new_with_connector(connector);
            let cli = Client::builder().build(https_connector);
            cli.request(req)
        }
    };

    let res = timeout(timeout_dur, task).await?;
    match res {
        Ok(resp) => Ok(resp),
        Err(e) => {
            return Err(Error::new(
                ErrorKind::Other,
                format!("failed to fetch response {}", e),
            ))
        }
    }
}

#[test]
fn test_read_bytes_timeout() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .is_test(true)
        .try_init();

    macro_rules! ab {
        ($e:expr) => {
            tokio_test::block_on($e)
        };
    }

    let ret = join_uri("http://localhost:12", "invalid");
    assert!(ret.is_ok());
    let u = ret.unwrap();
    let u = u.to_string();

    let ret = Request::builder()
        .method(hyper::Method::POST)
        .uri(u)
        .body(Body::empty());
    assert!(ret.is_ok());
    let req = ret.unwrap();
    let ret = ab!(read_bytes(req, Duration::from_secs(1), false, true));
    assert!(!ret.is_ok());
}

pub fn join_uri(url: &str, path: &str) -> io::Result<Url> {
    let mut uri = match Url::parse(url) {
        Ok(u) => u,
        Err(e) => {
            return Err(Error::new(
                ErrorKind::Other,
                format!("failed to parse client URL {}", e),
            ))
        }
    };

    if !path.is_empty() {
        match uri.join(path) {
            Ok(u) => uri = u,
            Err(e) => {
                return Err(Error::new(
                    ErrorKind::Other,
                    format!("failed to join parsed URL {}", e),
                ));
            }
        }
    }

    Ok(uri)
}

#[test]
fn test_join_uri() {
    let ret = Url::parse("http://localhost:9850/ext/X/sendMultiple");
    let expected = ret.unwrap();

    let ret = join_uri("http://localhost:9850/", "/ext/X/sendMultiple");
    assert!(ret.is_ok());
    let t = ret.unwrap();
    assert_eq!(t, expected);

    let ret = join_uri("http://localhost:9850", "/ext/X/sendMultiple");
    assert!(ret.is_ok());
    let t = ret.unwrap();
    assert_eq!(t, expected);

    let ret = join_uri("http://localhost:9850", "ext/X/sendMultiple");
    assert!(ret.is_ok());
    let t = ret.unwrap();
    assert_eq!(t, expected);
}

/// Downloads a file to the "file_path".
pub async fn download_file(ep: &str, file_path: &str) -> io::Result<()> {
    log::info!("downloading the file via {}", ep);
    let resp = reqwest::get(ep)
        .await
        .map_err(|e| Error::new(ErrorKind::Other, format!("failed reqwest::get {}", e)))?;

    let mut content = Cursor::new(
        resp.bytes()
            .await
            .map_err(|e| Error::new(ErrorKind::Other, format!("failed bytes {}", e)))?,
    );

    let mut f = File::create(file_path)?;
    copy(&mut content, &mut f)?;

    Ok(())
}

/// TODO: implement this with native Rust
pub async fn get_non_tls(url: &str, url_path: &str) -> io::Result<Vec<u8>> {
    let joined = join_uri(url, url_path)?;
    log::debug!("non-TLS HTTP get for {:?}", joined);

    let output = {
        if url.starts_with("https") {
            log::info!("sending via danger_accept_invalid_certs");
            let cli = ClientBuilder::new()
                .user_agent(env!("CARGO_PKG_NAME"))
                .danger_accept_invalid_certs(true)
                .timeout(Duration::from_secs(15))
                .connection_verbose(true)
                .build()
                .map_err(|e| {
                    Error::new(
                        ErrorKind::Other,
                        format!("failed ClientBuilder build {}", e),
                    )
                })?;
            let resp = cli.get(joined.as_str()).send().await.map_err(|e| {
                Error::new(ErrorKind::Other, format!("failed ClientBuilder send {}", e))
            })?;
            let out = resp.bytes().await.map_err(|e| {
                Error::new(ErrorKind::Other, format!("failed ClientBuilder send {}", e))
            })?;
            out.into()
        } else {
            let req = create_get(url, url_path)?;
            let buf = match read_bytes(
                req,
                Duration::from_secs(15),
                url.starts_with("https"),
                false,
            )
            .await
            {
                Ok(b) => b,
                Err(e) => return Err(e),
            };
            buf.to_vec()
        }
    };
    Ok(output)
}

/// RUST_LOG=debug cargo test --lib -- test_get_non_tls --exact --show-output
#[test]
fn test_get_non_tls() {
    use tokio::runtime::Runtime;

    let _ = env_logger::builder().is_test(true).try_init();

    let rt = Runtime::new().unwrap();
    let out = rt
        .block_on(get_non_tls(
            "https://api.github.com",
            "repos/ava-labs/avalanchego/releases/latest",
        ))
        .unwrap();
    println!("out: {}", String::from_utf8(out).unwrap());
}

/// Posts JSON body.
pub async fn post_non_tls(url: &str, url_path: &str, data: &str) -> io::Result<Vec<u8>> {
    let joined = join_uri(url, url_path)?;
    log::debug!("non-TLS HTTP post {}-byte data to {:?}", data.len(), joined);

    let output = {
        if url.starts_with("https") {
            log::info!("sending via danger_accept_invalid_certs");

            let cli = ClientBuilder::new()
                .user_agent(env!("CARGO_PKG_NAME"))
                .danger_accept_invalid_certs(true)
                .timeout(Duration::from_secs(15))
                .connection_verbose(true)
                .build()
                .map_err(|e| {
                    Error::new(
                        ErrorKind::Other,
                        format!("failed ClientBuilder build {}", e),
                    )
                })?;
            let resp = cli
                .post(joined.as_str())
                .header(CONTENT_TYPE, "application/json")
                .body(data.to_string())
                .send()
                .await
                .map_err(|e| {
                    Error::new(ErrorKind::Other, format!("failed ClientBuilder send {}", e))
                })?;
            let out = resp.bytes().await.map_err(|e| {
                Error::new(ErrorKind::Other, format!("failed ClientBuilder send {}", e))
            })?;
            out.into()
        } else {
            let req = create_json_post(url, url_path, data)?;
            let buf = match read_bytes(req, Duration::from_secs(15), false, false).await {
                Ok(b) => b,
                Err(e) => return Err(e),
            };
            buf.to_vec()
        }
    };
    Ok(output)
}
