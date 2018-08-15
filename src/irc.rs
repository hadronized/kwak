use html_entities::decode_html_entities;
use regex::Regex;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use time::now;

use http::{is_http_response_valid, http_get};
use markov::MarkovChain;
use tells::{Tells, Nick};

const MIN_MS_BETWEEN_SAYS: u64 = 500;

lazy_static!{
  static ref RE_URL: Regex = Regex::new("(^|\\s+)https?://[^ ]+\\.[^ ]+").unwrap();
  static ref RE_TITLE: Regex = Regex::new("<title>([^<]*)</title>").unwrap();
  static ref RE_YOUTUBE: Regex = Regex::new("https?://www\\.youtube\\.com/watch.+v=([^&?]+)").unwrap();
  static ref RE_YOUTU_BE: Regex = Regex::new("https?://youtu\\.be/([^&?]+)").unwrap();
  static ref RE_I_IMGUR: Regex = Regex::new("https?://i\\.imgur\\.com/([^.]+)").unwrap();
}

macro_rules! opt {
  ($option:expr) => {
    match $option {
      ::std::option::Option::Some(a) => a,
      ::std::option::Option::None => return ::std::option::Option::None
    }
  }
}

type Cmd = String;

/// Supported orders.
#[derive(Debug)]
enum Order {
  // from, to, content
  Tell(Nick, Nick, String),
  PrependTopic(String),
  ResetTopic(String),
  BotQuote(Vec<String>),
  ThisIsFine
}

struct IRCReader {
  /// TCP stream holding the IRC connection.
  stream: BufReader<TcpStream>,
  /// Line buffer used to read from IRC.
  line_buf: String
}

impl IRCReader {
  /// Read a line from IRC.
  fn read_line(&mut self) -> String {
    self.line_buf.clear();
    let _ = self.stream.read_line(&mut self.line_buf);
    let line = self.line_buf.trim().to_owned();

    println!("<-- {}", line);

    line
  }
}

struct IRCWriter {
  /// TCP stream holding the IRC connection.
  stream: TcpStream,
  /// Last time we said something.
  last_say_instant: Instant,
}

impl IRCWriter {
  /// Write a line to IRC.
  fn write_line(&mut self, msg: &str) {
    let _ = self.stream.write(msg.as_bytes());
    let _ = self.stream.write(&[b'\n']);

    println!("--> {}", msg);
  }

  /// Say something on IRC.
  fn say(&mut self, msg: &str, dest: &str) {
    if self.last_say_instant.elapsed() >= Duration::from_millis(MIN_MS_BETWEEN_SAYS) {
      self.write_line(&format!("PRIVMSG {} :{}", dest, msg));
      self.last_say_instant = Instant::now();
    }
  }
}

/// IRC client.
pub struct IRC {
  /// IRC reader.
  reader: Arc<Mutex<IRCReader>>,
  /// IRC writer.
  writer: Arc<Mutex<IRCWriter>>,
  /// Our nickname.
  nick: String,
  /// The current host we’re connected to.
  host: String,
  /// The current channel we’re connected to.
  channel: String,
  /// Tells.
  tells: Tells,
  /// Markov chain.
  markov_chain: Option<MarkovChain>,
  /// Path to the log file.
  log_path: PathBuf,
}

impl IRC {
  /// Connect to an IRC server with a given name, then join the given channel.
  pub fn connect(addr: &str, port: u16, nick: &str, channel: &str, tells: Tells, markov_chain: Option<MarkovChain>, log_path: &str) -> Self {
    let stream = TcpStream::connect((addr, port)).unwrap();
    let writer = Arc::new(Mutex::new(IRCWriter {
      stream: stream.try_clone().unwrap(),
      last_say_instant: Instant::now()
    }));
    let reader = Arc::new(Mutex::new(IRCReader {
      stream: BufReader::new(stream),
      line_buf: String::with_capacity(1024),
    }));

    IRC {
      reader: reader,
      writer: writer,
      nick: nick.to_owned(),
      host: addr.to_owned(),
      channel: channel.to_owned(),
      tells: tells,
      markov_chain: markov_chain,
      log_path: Path::new(log_path).to_owned()
    }
  }

  /// IRC initialization protocol implementation. Also, this function automatically joins the
  /// channel.
  pub fn init(&self) {
    {
      let mut writer = self.writer.lock().unwrap();
      writer.write_line(&format!("USER a {0:} {0:} :d", self.host));
      writer.write_line(&format!("NICK {}", self.nick));
    }

    loop {
      let line = {
        let mut reader = self.reader.lock().unwrap();
        reader.read_line()
      };

      if Self::is_ping(&line) {
        self.handle_ping(&line);
        break;
      } else if Self::is_welcome(&line) {
        break;
      }
    }

    let mut writer = self.writer.lock().unwrap();
    writer.write_line(&format!("JOIN {}", self.channel));
  }

  /// Run IRC.
  pub fn run(&mut self) -> ! {
    loop {
      let line = { self.reader.lock().unwrap().read_line() };

      if Self::is_ping(&line) {
        self.handle_ping(&line);
      } else if let Some((nick, cmd, args)) = Self::extract_user_msg(&line) {
        self.dispatch_user_msg(nick, cmd, args);
      }
    }
  }

  /// Handle an IRC ping by sending the approriate pong.
  pub fn handle_ping(&self, ping: &str) {
    let pong = "PO".to_owned() + &ping[2..];
    println!("\x1b[36msending PONG: {}\x1b[0m", pong);

    let mut writer = self.writer.lock().unwrap();
    writer.write_line(&pong);
  }

  /// Say something on irc.
  ///
  /// This function expects the message to say and the destination. It can be user logged on the
  /// server (not necessarily in the same channel) or nothing – in that case, the message will be
  /// delivered to the channel.
  fn say(&self, msg: &str, dest: Option<&str>) {
    let mut writer = self.writer.lock().unwrap();
    let dest = if let Some(nick) = dest { nick.to_owned() } else { self.channel.clone() };

    writer.say(msg, &dest);
  }

  /// Save a message to the log.
  fn log_msg(&self, msg: &str) {
    let mut file = OpenOptions::new()
      .create(true)
      .append(true)
      .open(&self.log_path).unwrap();
    let t = now();
    let bytes = format!("{:02}:{:02}:{:02} {}", t.tm_hour, t.tm_min, t.tm_sec, msg);

    let _ = file.write_all(bytes.as_bytes());
  }

  /// Is a message a PING notification? This function is intended to be used with a raw IRC line.
  fn is_ping(msg: &str) -> bool {
    msg.starts_with("PING")
  }

  /// Is a message a WELCOME notification? This function is intended to be used with a raw IRC line.
  fn is_welcome(msg: &str) -> bool {
    let words: Vec<_> = msg.split_whitespace().collect();;

    words.len() > 1 && words[1] == "001"
  }

  /// Scan a URL and try to retrieve its title. A thread is spawned to do the work and the function
  /// immediately returns.
  fn url_scan(&self, nick: Nick, private: bool, content: String) {
    let re_match = RE_URL.find(&content);
  
    if let Some(rem) = re_match {
      // clone a few stuff to bring with us in the thread
      let dest = if private { nick.clone() } else { self.channel.clone() };
      let writer = self.writer.clone();
  
      let url = (&content[rem.start() .. rem.end()]).to_owned();
      let _ = thread::spawn(move || Self::url_scan_work(nick, writer, url, dest));
    }
  }

  fn url_scan_work(nick: Nick, writer: Arc<Mutex<IRCWriter>>, url: String, dest: String) {
    // fix some URLs that might cause problems
    let (url, fixed_url_method) = Self::fix_url(&url);

    match http_get(&url) {
      Ok(mut response) => {
        // inspect the header to deny big things
        if !is_http_response_valid(&url, &response.headers()) {
          return;
        }

        let mut body = String::new();
        let _ = response.read_to_string(&mut body);

        // find the title
        let title = Self::find_title(&body, fixed_url_method);

        match title {
          Some(title) => {
            writer.lock().unwrap().say(&format!("{}: \x037«\x036 {} \x037»\x0F", nick, title), &dest);
          },
          None => {
            writer.lock().unwrap().say(&format!("\x036I’ve found nothing…\x0F"), &dest);
          }
        }
      },
      Err(e) => {
        println!("\x1b[31munable to get {}: {:?}\x1b[0m", url, e);
      }
    }
  }

  /// Fix some URLs
  fn fix_url(url: &str) -> (String, Option<FixedURLMethod>) {
    let youtube_video_id = RE_YOUTUBE.captures(&url);
    let youtu_be_video_id = RE_YOUTU_BE.captures(&url);

    match (youtube_video_id, youtu_be_video_id) {
      (Some(ref captures), _) | (_, Some(ref captures)) => {
        if let Some(video_id) = captures.get(1) {
          return (format!("http://www.infinitelooper.com/?v={}", video_id.as_str()), Some(FixedURLMethod::Youtube));
        } else {
          return (String::new(), None);
        }
      },
      _ => {}
    }

    let i_imgur_id = RE_I_IMGUR.captures(&url);

    match i_imgur_id {
      Some(ref captures) => {
        if let Some(img_id) = captures.get(1) {
          return (format!("http://imgur.com/{}", img_id.as_str()), Some(FixedURLMethod::Imgur));
        } else {
          return (String::new(), None);
        }
      },
      _ => {}
    }

    (url.to_owned(), None)
  }

  fn find_title(body: &str, fixed_url_method: Option<FixedURLMethod>) -> Option<String> {
    if let Some(captures) = RE_TITLE.captures(body) {
      if let Some(title) = captures.get(1) {
        let mut cleaned_title: String = title.as_str().chars().filter(|c| *c != '\n').collect();

        // decode entities ; if we cannot, just dump the title as-is
        cleaned_title = decode_html_entities(&cleaned_title).unwrap_or(cleaned_title).trim().to_owned();

        if let Some(FixedURLMethod::Youtube) = fixed_url_method {
          cleaned_title = Self::fix_youtube_title(&cleaned_title);
        }

        // remove internal consecutive spaces and output
        Some(remove_consecutive_whitespaces(&cleaned_title))
      } else {
        None
      }
    } else {
      None
    }
  }

  /// Fix a Youtube title.
  fn fix_youtube_title(title: &str) -> String {
    (&title[17..]).to_owned()
  }

  /// Extract the content of an IRC line, coming from someone saying something. The function returns
  /// a triple:
  ///
  /// - who said that?
  /// - what IRC command was used?
  /// - the rest of the line
  fn extract_user_msg(msg: &str) -> Option<(Nick, Cmd, Vec<String>)> {
    if !msg.starts_with(":") {
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

  /// Extract – if possible – an order from a message from someone.
  fn extract_order(from: Nick, msg: &[String]) -> Option<Order> {
    match &(msg[0])[..] {
      "!tell" if msg.len() >= 3 => {
        // we have someone to tell something
        let to = msg[1].to_owned();
        let content = msg[2..].join(" ");
  
        Some(Order::Tell(from, to, content))
      }

      "!tropico" if msg.len() >= 2 => {
        // we want to prepend something to the current topic
        let content = msg[1..].join(" ");
        Some(Order::PrependTopic(content))
      }

      "!tropicoset" if msg.len() >= 2 => {
        // want to reset the current topic
        let content = msg[1..].join(" ");
        Some(Order::ResetTopic(content))
      }

      "!q" if msg.len() >= 2 => {
        // nothing to do; just ask for a uber cool quote!
        let words = (&msg[1..]).to_owned();
        Some(Order::BotQuote(words))
      }

      "!fine" if msg.len() == 1 => {
        Some(Order::ThisIsFine)
      }

      _ => None
    }
  }

  /// Dispatch what users say.
  fn dispatch_user_msg(&mut self, nick: Nick, cmd: Cmd, args: Vec<String>) {
    match &cmd[..] {
      "PRIVMSG" if nick != self.nick => {
        self.treat_privmsg(nick, args);
      },
      "QUIT" if nick == self.nick => self.on_quit(),
      _ => {}
    }
  }

  // FIXME: too long function, we need to split it up
  /// Action to take when a user said something.
  fn treat_privmsg(&mut self, nick: Nick, mut args: Vec<String>) {
    args[1].remove(0); // remove the leading ':' // FIXME: I guess we can remove that line

    let dest = &args[0]; // args[0] contains the destination (channel if public, user if PM)
    let order = Self::extract_order(nick.clone(), &args[1..]);
  
    match order {
      Some(Order::Tell(from, to, content)) => self.tells.record(&from, &to, &content),
      Some(Order::PrependTopic(topic)) => self.prepend_topic(topic),
      Some(Order::ResetTopic(topic)) => self.reset_topic(topic),
      Some(Order::BotQuote(words)) => self.bot_quote(&dest, &nick, &words),
      Some(Order::ThisIsFine) => self.this_is_fine(),
      None => {
        // someone just said something, and it’s not an order, see whether we should say something
        if let Some(msgs) = self.tells.get(&nick.to_ascii_lowercase()).cloned() {
          for &(ref from, ref msg) in msgs.iter() {
            self.say(&format!("\x02\x036{}\x0F: \x02\x032{}\x0F", from, msg), Some(&nick));
          }
  
          self.tells.remove(&nick.to_ascii_lowercase());
          self.tells.save();
        }
      }
    }
  
    // grab the content for further processing
    let content = args[1..].join(" ").to_owned();
  
    // log what the user said for future quotes
    self.log_msg(&format!("{} {}\n", nick, content));
  
    // add what that people say to the markov model if enabled
    if let Some(ref mut markov) = self.markov_chain {
      let words: Vec<_> = (&args[1..]).iter().map(|s| &s[..]).collect();
      markov.treat_line(&words);
    }
  
    // look for URLs to scan
    let private = &args[0] == &self.nick;
    self.url_scan(nick, private, content);
  }

  /// What to do after we’ve quitted the channel.
  fn on_quit(&self) {
    thread::sleep(Duration::from_secs(1));
    self.init();
  }

  /// Read the topic on the current channel.
  fn read_topic(&self) -> Option<String> {
    // ask for the topic
    {
      let mut writer = self.writer.lock().unwrap();
      writer.write_line(&format!("TOPIC {}", self.channel.clone()));
    }
  
    // get the topic
    let topic = {
      self.reader.lock().unwrap().read_line()
    };
  
    // clean it
    (&topic[1..]).find(':').map(|colon_ix| { // [1..] removes the first ':' (IRC proto)
      let colon_ix = colon_ix + 2;
      String::from((&topic[colon_ix..]).trim())
    })
  }

  /// Prepend a topic fragment to the current topic held in the channel.
  fn prepend_topic(&self, topic_part: String) {
    match self.read_topic() {
      Some(ref topic) if topic_part != *topic => {
        let chan = self.channel.clone();
  
        let new_topic = if topic.is_empty() {
          format!("TOPIC {} :{}", chan, topic_part) // FIXME: sanitize
        } else {
          format!("TOPIC {} :{} · {}", chan, topic_part, topic) // FIXME: sanitize
        };

        self.writer.lock().unwrap().write_line(&new_topic);
      },
      _ => {
        println!("\x1b[31munable to get topic\x1b[0m");
      }
    }
  }

  /// Reset the current topic of the channel.
  fn reset_topic(&self, new_topic: String) {
    let mut writer = self.writer.lock().unwrap();
    writer.write_line(&format!("TOPIC {} :{}", self.channel.clone(), new_topic)); // FIXME: sanitize
  }

  /// Generate a line and say it on IRC.
  fn bot_quote(&self, dest: &str, nick: &str, words: &[String]) {
    if words.is_empty() {
      return;
    }

    if let Some(ref markov) = self.markov_chain {
      let line = markov.gen_random_line(&words);

      if dest == self.channel {
        self.say(&line, None);
      } else {
        self.say(&line, Some(nick));
      }
    }
  }

  // Show a link of the This Is Fine meme on the current channel.
  fn this_is_fine(&self) {
    self.say("https://phaazon.net/media/uploads/this_is_fine.jpg", None);
  }
}

#[derive(Clone)]
enum FixedURLMethod {
  Youtube,
  Imgur
}

fn remove_consecutive_whitespaces(input: &str) -> String {
  let mut space_prev = false;

  input.chars().filter(move |&c| {
    if c.is_whitespace() {
      if space_prev {
        // if this space is contiguous to others, just don’t include him anymore
        false
      } else {
        space_prev = true;
        true
      }
    } else {
      space_prev = false;
      true
    }
  }).collect()
}
