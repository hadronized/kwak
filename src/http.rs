use hyper::client::Client;
use hyper::error::Result;
use hyper::header::{Accept, AcceptCharset, Charset, ContentLength, ContentType, Headers, UserAgent,
                    qitem};
use hyper::mime::{Mime, SubLevel, TopLevel};

pub use hyper::client::response::Response;

pub fn http_get(url: &str) -> Result<Response> {
  let mut headers = Headers::new();

  headers.set(
    Accept(vec![
      qitem(Mime(TopLevel::Text, SubLevel::Html, vec![]))
    ])
  );
  headers.set(
    AcceptCharset(vec![
      qitem(Charset::Ext("utf-8".to_owned())),
      qitem(Charset::Ext("iso-8859-1".to_owned())),
      qitem(Charset::Iso_8859_1)
    ])
  );
  headers.set(UserAgent("hyper/0.9.10".to_owned()));

  println!("\x1b[36mGET {}\x1b[0m", url);

  let client = Client::new();
  client.get(url).headers(headers).send()
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
    if ty.0 != TopLevel::Text && ty.1 != SubLevel::Html {
      println!("\x1b[31m{} is not plain HTML: {:?}\x1b[0m", url, ty);
      return false;
    }
  }

  true
}
