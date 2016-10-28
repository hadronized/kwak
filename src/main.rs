extern crate clap;
#[macro_use]
extern crate lazy_static;
extern crate html_entities;
extern crate hyper;
extern crate regex;
extern crate serde_json;
extern crate time;

use clap::{App, Arg};
use html_entities::decode_html_entities;
use hyper::client;
use hyper::header;
use hyper::mime;
use regex::Regex;
use serde_json::de;
use serde_json::ser;
use std::ascii::AsciiExt;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use time::now;

macro_rules! opt {
  ($option:expr) => {
    match $option {
      ::std::option::Option::Some(a) => a,
      ::std::option::Option::None => return ::std::option::Option::None
    }
  }
}

lazy_static!{
  static ref RE_URL: Regex = Regex::new("(^|\\s+)https?://[^ ]+\\.[^ ]+").unwrap();
  static ref RE_TITLE: Regex = Regex::new("<title>([^<]*)</title>").unwrap();
  static ref RE_YOUTUBE: Regex = Regex::new("https?://www\\.youtube\\.com/watch.+v=([^&?]+)").unwrap();
  static ref RE_YOUTU_BE: Regex = Regex::new("https?://youtu\\.be/(.+)").unwrap();
}

// TODO: split that into IRCClient containing only IRC stuff and TellCfg or something like that for
// tells as we don’t want to carry them around all the fucking place
struct IRCClient {
  stream: BufReader<net::TcpStream>,
  line_buf: String,
  nick: String,
  channel: String,
  tells: Tells,
  quotes_file: PathBuf,
  last_instant: Instant,
}

impl IRCClient {
  fn connect(addr: &str, port: u16, nick: &str, channel: &str, tells_file: &str, quotes_file: &str) -> Self {
    let stream = BufReader::new(net::TcpStream::connect((addr, port)).unwrap());

    IRCClient {
      stream: stream,
      line_buf: String::with_capacity(1024),
      nick: nick.to_owned(),
      channel: channel.to_owned(),
      tells: read_tells(tells_file),
      quotes_file: Path::new(quotes_file).to_owned(),
      last_instant: Instant::now(),
    }
  }

  fn try_clone(&self) -> Option<Self> {
    self.stream.get_ref().try_clone().map(|stream| {
      IRCClient {
        stream: BufReader::new(stream),
        line_buf: self.line_buf.clone(),
        nick: self.nick.clone(),
        channel: self.channel.clone(),
        tells: self.tells.clone(),
        quotes_file: self.quotes_file.clone(),
        last_instant: self.last_instant,
      }
    }).ok()
  }

  fn save_tells(&mut self) {
    save_tells("tells.json", &self.tells);
  }

  fn read_line(&mut self) -> String {
    self.line_buf.clear();
    let _ = self.stream.read_line(&mut self.line_buf);
    self.line_buf.trim().to_owned()
  }

  fn write_line(&mut self, msg: &str) {
    let stream = self.stream.get_mut();
    let _ = stream.write(msg.as_bytes());
    let _ = stream.write("\n".as_bytes());
  }

  fn init(&mut self) {
    let nick = self.nick.clone();

    self.write_line("USER a b c :d");
    self.write_line(&format!("NICK {}", nick));

    self.rejoin();
  }

  fn rejoin(&mut self) {
    let chan = self.channel.clone();
    self.write_line(&format!("JOIN {}", chan));
  }

  fn handle_ping(&mut self, ping: String) {
    let pong = "PO".to_owned() + &ping[2..];
    println!("\x1b[36msending PONG: {}\x1b[0m", pong);
    self.write_line(&pong);
  }

  fn say(&mut self, msg: &str, priv_user: Option<&str>) {
    if self.last_instant.elapsed() >= Duration::from_millis(500) {
      self.last_instant = Instant::now();
      let header = "PRIVMSG ".to_owned();

      match priv_user {
        Some(user) => {
          self.write_line(&(header + user + " :" + msg));
        },
        None => {
          let channel = &self.channel.clone();
          self.write_line(&(header + channel + " :" + msg));
        }
      }
    }
  }

  fn log_msg(&self, msg: &str) {
    let mut file = OpenOptions::new()
      .create(true)
      .append(true)
      .open(&self.quotes_file).unwrap();
    let t = now();
    let bytes = format!("{:02}:{:02}:{:02} {}", t.tm_hour, t.tm_min, t.tm_sec, msg);

    let _ = file.write_all(bytes.as_bytes());
  }
}

fn is_ping(msg: &str) -> bool {
  msg.starts_with("PING")
}

fn extract_user_msg(msg: &str) -> Option<(Nick, Cmd, Vec<String>)> {
  if !msg.starts_with(":") {
    println!("a");
    return None;
  }

  let msg = &msg[1..]; // remove the first ':'

  // find the index of the first ! to extract nick (left part) and content (right part)
  let bang_index = opt!(msg.find('!'));
  let nick = msg[0 .. bang_index].to_owned();
  let content = &msg[bang_index + 1 ..];

  // [host, cmd, …]
  let items: Vec<_> = content.split(' ').collect();

  if items.len() >= 2 {
    let cmd = items[1].to_owned();

    if items.len() >= 3 {
      let args: Vec<_> = items[2..].iter().map(|&x| x.to_owned()).collect();
      Some((nick, cmd, args))
    } else {
      Some((nick, cmd, Vec::new()))
    }
  } else {
    None
  }
}

fn dispatch_user_msg(irc: &mut IRCClient, nick: Nick, cmd: Cmd, args: Vec<String>) {
  match &cmd[..] {
    "JOIN" => {
      println!("\x1b[36m{} joined!\x1b[0m", nick);
    },
    "PRIVMSG" => {
      treat_privmsg(irc, nick, args);
    },
    "QUIT" => {
      treat_quit(irc, nick);
    },
    _ => {}
  }
}

fn treat_privmsg(irc: &mut IRCClient, nick: Nick, mut args: Vec<String>) {
  // early return to prevent us from talking to ourself
  if nick == irc.nick {
    return;
  }

  args[1].remove(0); // remove the leading ':'
  let order = extract_order(nick.clone(), &args[1..]);

  match order {
    Some(Order::Tell(from, to, content)) => add_tell(irc, from, to, content),
    None => {
      // someone just said something, see whether we should say something
      if let Some(msgs) = irc.tells.get(&nick.to_ascii_lowercase()).cloned() {
        for &(ref from, ref msg) in msgs.iter() {
          irc.say(&format!("\x02\x036{}\x0F: \x02\x032{}\x0F", from, msg), Some(&nick));
        }

        irc.tells.remove(&nick.to_ascii_lowercase());
        irc.save_tells();
      }
    }
  }

  // grab the content for further processing
  let content = args[1..].join(" ").to_owned();

  // log what the user said for future quotes
  irc.log_msg(&format!("{} {}\n", nick, content));

  // look for URLs to scan
  let private = &args[0] == &irc.nick;
  scan_url(irc, nick, private, content);
}

fn scan_url(irc: &mut IRCClient, nick: Nick, private: bool, content: String) {
  let re_match = RE_URL.find(&content);

  if let Some((start_index, end_index)) = re_match {
    // clone a few stuff to bring with us in the thread
    let channel = if private { Some(nick.clone()) } else { None };

    if let Some(mut irc) = irc.try_clone() {
      let _ = thread::spawn(move || {
        let mut url = String::from(&content[start_index .. end_index]);
        let url2 = url.clone();
        let youtube_video_id = RE_YOUTUBE.captures(&url2);
        let youtu_be_video_id = RE_YOUTU_BE.captures(&url2);

        // partial fix for youtube videos; we cannot directly hit Google’s servers; use a shitty
        // web service instead
        match (&youtube_video_id, &youtu_be_video_id) {
          (&Some(ref captures), _) | (_, &Some(ref captures)) => {
            if let Some(video_id) = captures.at(1) {
              url = format!("http://www.infinitelooper.com/?v={}", video_id);
            }
          },
          _ => {}
        }

        match http_get(&url) {
          Ok(mut response) => {
            // inspect the header to deny big things
            if !is_http_response_valid(&url, &response.headers) {
              return;
            }

            let mut body = String::new();
            let _ = response.read_to_string(&mut body);

            // find the title and send it back to the main thread if found any
            if let Some(captures) = RE_TITLE.captures(&body) {
              if let Some(title) = captures.at(1) {
                let cleaned_title: String = title.chars().filter(|c| *c != '\n').collect();

                // decode entities ; if we cannot, just dump the title as-is
                let cleaned_title = decode_html_entities(&cleaned_title).unwrap_or(cleaned_title);
                let mut cleaned_title = cleaned_title.trim();

                // partial fix for youtube; remove the "InfiniteLooper - " header from the title
                if youtube_video_id.is_some() || youtu_be_video_id.is_some() {
                  cleaned_title = &cleaned_title[17..];
                }

                match channel {
                  Some(nick) => irc.say(&format!("\x037«\x036 {} \x037»\x0F", cleaned_title), Some(&nick)),
                  None => irc.say(&format!("\x037«\x036 {} \x037»\x0F", cleaned_title), None),
                }
              }
            }
          },
          Err(e) => {
            println!("\x1b[31munable to get {}: {:?}\x1b[0m", url, e);
          }
        }
      });
    }
  }
}

fn http_get(url: &str) -> hyper::error::Result<client::response::Response> {
  let mut headers = header::Headers::new();

  headers.set(
    header::Accept(vec![
      header::qitem(mime::Mime(mime::TopLevel::Text, mime::SubLevel::Html, vec![]))
    ])
  );
  headers.set(
    header::AcceptCharset(vec![
      header::qitem(header::Charset::Ext("utf-8".to_owned())),
      header::qitem(header::Charset::Ext("iso-8859-1".to_owned())),
      header::qitem(header::Charset::Iso_8859_1)
    ])
  );
  headers.set(header::UserAgent("hyper/0.9.10".to_owned()));

  println!("\x1b[36mGET {}\x1b[0m", url);

  let client = client::Client::new();
  client.get(url).headers(headers).send()
}

fn is_http_response_valid(url: &str, headers: &header::Headers) -> bool {
  // inspect the header to deny big things
  if let Some(&header::ContentLength(bytes)) = headers.get() {
    println!("\x1b[36msize {} bytes\x1b[0m", bytes);

    // deny anything >= 1MB
    if bytes >= 1000000 {
      println!("\x1b[31m{} denied because too big ({} bytes)\x1b[0m", url, bytes);
      return false;
    }
  }

  // inspect mime (we only want HTML)
  if let Some(&header::ContentType(ref ty)) = headers.get() {
    println!("\x1b[36mty {:?}\x1b[0m", ty);

    // deny anything that is not HTML
    if ty.0 != mime::TopLevel::Text && ty.1 != mime::SubLevel::Html {
      println!("\x1b[31m{} is not plain HTML: {:?}\x1b[0m", url, ty);
      return false;
    }
  }

  true
}

fn treat_quit(irc: &mut IRCClient, nick: String) {
  if nick == irc.nick {
    thread::sleep(Duration::from_secs(1));
    irc.rejoin();
  }
}

fn extract_order(from: Nick, msg: &[String]) -> Option<Order> {
  if msg[0] == "!tell" {
    if msg.len() >= 3 {
      // we have someone to tell something
      let to = msg[1].to_owned();
      let content = msg[2..].join(" ");

      return Some(Order::Tell(from, to, content));
    }
  }

  None
}

fn add_tell(irc: &mut IRCClient, from: Nick, to: Nick, content: String) {
  let mut msgs = irc.tells.get(&to).map_or(Vec::new(), |x| x.clone());
  msgs.push((from.to_ascii_lowercase(), content));
  irc.tells.insert(to.to_ascii_lowercase(), msgs);
  irc.save_tells();
}

#[derive(Debug)]
enum Order {
  // from, to, content
  Tell(Nick, Nick, String)
}

type Nick = String;
type Cmd = String;
type Message = String;
type Tells = BTreeMap<Nick, Vec<(Nick, Message)>>;

fn read_tells<P>(path: P) -> Tells where P: AsRef<Path> {
  match File::open(path.as_ref()) {
    Ok(file) => {
      de::from_reader(file).unwrap()
    },
    Err(e) => {
      println!("\x1b[31munable to read tells from {:?}: {}\x1b[0m", path.as_ref(), e);
      Tells::new()
    }
  }
}

fn save_tells<P>(path: P, tells: &Tells) where P: AsRef<Path> {
  match File::create(path.as_ref()) {
    Ok(mut file) => {
      let _ = ser::to_writer(&mut file, tells);
    },
    Err(e) => {
      println!("\x1b[31munable to save tells to {:?}: {}\x1b[0m", path.as_ref(), e);
    }
  }
}

fn main() {
  let options = App::new("kwak")
    .arg(Arg::with_name("host")
         .short("h")
         .long("host")
         .help("IRC server host to connect to")
         .required(true)
         .takes_value(true))
    .arg(Arg::with_name("channel")
         .short("c")
         .long("channel")
         .help("IRC channel to join upon connection")
         .required(true)
         .takes_value(true))
    .arg(Arg::with_name("nick")
         .short("n")
         .long("nick")
         .help("IRC nickname to use")
         .required(true)
         .takes_value(true))
    .arg(Arg::with_name("tells")
         .short("t")
         .long("tells_file")
         .value_name("FILE")
         .help("Path to the JSON file containing the tells")
         .takes_value(true))
    .arg(Arg::with_name("quotes")
         .short("q")
         .long("quotes_file")
         .value_name("FILE")
         .help("Path to the JSON file containing the quotes")
         .takes_value(true))
    .get_matches();

  let host = options.value_of("host").unwrap();
  let channel = options.value_of("channel").unwrap();
  let nick = options.value_of("nick").unwrap();
  let tells_file = options.value_of("tells").unwrap_or("tells.json");
  let quotes_file = options.value_of("quotes").unwrap_or("quotes.log");

  let port = 6667;
  let mut irc = IRCClient::connect(host, port, nick, channel, tells_file, quotes_file);

  irc.init();

  loop {
    let line = irc.read_line();
    println!("{}", line);

    if is_ping(&line) {
      irc.handle_ping(line);
      continue;
    }

    if let Some(user_msg) = extract_user_msg(&line) {
      dispatch_user_msg(&mut irc, user_msg.0, user_msg.1, user_msg.2);
    }
  }
}
