use reqwest::{Client, Response, Result};
use reqwest::header::{
  ACCEPT, ACCEPT_CHARSET,CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT, HeaderMap, HeaderValue
};

pub fn http_get(url: &str) -> Result<Response> {
  let mut headers = HeaderMap::new();

  headers.insert(USER_AGENT, HeaderValue::from_static("kwak/0.1"));

  headers.insert(ACCEPT, HeaderValue::from_static("text/html"));
  headers.append(ACCEPT, HeaderValue::from_static("text/html; charset=utf-8"));
  headers.append(ACCEPT, HeaderValue::from_static("text/html; charset=UTF-8"));

  headers.insert(ACCEPT_CHARSET, HeaderValue::from_static("utf-8"));
  headers.insert(ACCEPT_CHARSET, HeaderValue::from_static("UTF-8"));
  headers.append(ACCEPT_CHARSET, HeaderValue::from_static("iso-8859-1"));
  headers.append(ACCEPT_CHARSET, HeaderValue::from_static("ISO-8859-1"));

  println!("\x1b[36mGET {}\x1b[0m", url);

  Client::builder()
    .default_headers(headers)
    .build()?
    .get(url)
    .send()
}

/// Check that a HTTP GET response is valid – i.e. check for the HTTP headers that we can read the
/// body.
pub fn is_http_response_valid(url: &str, headers: &HeaderMap) -> bool {
  // inspect the header to deny big things
  let bytes: Option<u32> =
    headers.get(CONTENT_LENGTH)
           .and_then(|v| v.to_str().ok())
           .and_then(|h| h.parse().ok());

  if let Some(bytes) = bytes {
    println!("\x1b[36msize {} bytes\x1b[0m", bytes);

    // deny anything >= 1MB
    if bytes >= 1000000 {
      println!("\x1b[31m{} denied because too big ({} bytes)\x1b[0m", url, bytes);
      return false;
    }
  }

  // inspect mime (we only want HTML)
  let mime = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());

  if let Some(mime) = mime {
    println!("\x1b[36mmime {:?}\x1b[0m", mime);

    // deny anything that is not HTML
    if !mime.starts_with("text/html") {
      println!("\x1b[31m{} is not plain HTML: {:?}\x1b[0m", url, mime);
      return false;
    }
  }

  true
}
