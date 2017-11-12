use reqwest::{Client, Response, Result};
use reqwest::header::{Accept, AcceptCharset, Charset, ContentLength, ContentType, Headers,
                      UserAgent, qitem};
use reqwest::mime::{TEXT_HTML, TEXT_HTML_UTF_8};

pub fn http_get(url: &str) -> Result<Response> {
  let mut headers = Headers::new();

  headers.set(Accept(vec![qitem(TEXT_HTML), qitem(TEXT_HTML_UTF_8)]));
  headers.set(
    AcceptCharset(vec![
      qitem(Charset::Ext("utf-8".to_owned())),
      qitem(Charset::Ext("iso-8859-1".to_owned())),
      qitem(Charset::Iso_8859_1)
    ])
  );
  headers.set(UserAgent::new("reqwest/0.8.1"));

  println!("\x1b[36mGET {}\x1b[0m", url);

  Client::builder()
    .default_headers(headers)
    .build()?
    .get(url)
    .send()
}

/// Check that a HTTP GET response is valid – i.e. check for the HTTP headers that we can read the
/// body.
pub fn is_http_response_valid(url: &str, headers: &Headers) -> bool {
  // inspect the header to deny big things
  if let Some(&ContentLength(bytes)) = headers.get() {
    println!("\x1b[36msize {} bytes\x1b[0m", bytes);

    // deny anything >= 1MB
    if bytes >= 1000000 {
      println!("\x1b[31m{} denied because too big ({} bytes)\x1b[0m", url, bytes);
      return false;
    }
  }

  // inspect mime (we only want HTML)
  if let Some(&ContentType(ref ty)) = headers.get() {
    println!("\x1b[36mty {:?}\x1b[0m", ty);

    // deny anything that is not HTML
    if *ty != TEXT_HTML && *ty != TEXT_HTML_UTF_8 {
      println!("\x1b[31m{} is not plain HTML: {:?}\x1b[0m", url, ty);
      return false;
    }
  }

  true
}
