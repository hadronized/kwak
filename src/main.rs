#![feature(proc_macro)]

extern crate clap;
#[macro_use]
extern crate lazy_static;
extern crate html_entities;
extern crate hyper;
extern crate rand;
extern crate regex;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate time;

use clap::{App, Arg};
use html_entities::decode_html_entities;
use hyper::client;
use hyper::header;
use hyper::mime;
use rand::distributions::{IndependentSample, Range};
use regex::Regex;
use serde_json::de;
use serde_json::ser;
use std::ascii::AsciiExt;
use std::collections::{HashMap, LinkedList};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::iter::repeat;
use std::net;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use time::now;

mod markov;
mod tells;

const MIN_MS_BETWEEN_SAYS: u64 = 500;
const MAX_SENTENCE_WORDS_LEN: usize = 64;
const MAX_TRIES: usize = 100;
const FIRST_PROB_THRESHOLD: f32 = 0.5;
const LAST_PROB_THRESHOLD: f32 = 0.5;

lazy_static!{
  static ref RE_URL: Regex = Regex::new("(^|\\s+)https?://[^ ]+\\.[^ ]+").unwrap();
  static ref RE_TITLE: Regex = Regex::new("<title>([^<]*)</title>").unwrap();
  static ref RE_YOUTUBE: Regex = Regex::new("https?://www\\.youtube\\.com/watch.+v=([^&?]+)").unwrap();
  static ref RE_YOUTU_BE: Regex = Regex::new("https?://youtu\\.be/([^&?]+)").unwrap();
}

macro_rules! opt {
  ($option:expr) => {
    match $option {
      ::std::option::Option::Some(a) => a,
      ::std::option::Option::None => return ::std::option::Option::None
    }
  }
}

/// IRC client.
struct IRC {
  stream: BufReader<net::TcpStream>,
  line_buf: String,
  nick: String,
  channel: String,
  last_say_instant: Instant,
  log_path: PathBuf,
}

impl IRC {
  /// Connect to an IRC server with a given name, then join the given channel. All messages will be
  /// registered to the given log path.
  fn connect(addr: &str, port: u16, nick: &str, channel: &str, log_path: &str) -> Self {
    let stream = BufReader::new(net::TcpStream::connect((addr, port)).unwrap());

    IRCClient {
      stream: stream,
      line_buf: String::with_capacity(1024),
      nick: nick.to_owned(),
      channel: channel.to_owned(),
      last_say_instant: Instant::now(),
      log_path: Path::new(log_path).to_owned(),
    }
  }

  // FIXME: do we really need that?
  /// Try to clone this IRC client.
  fn try_clone(&self) -> Option<Self> {
    self.stream.get_ref().try_clone().map(|stream| {
      IRCClient {
        stream: BufReader::new(stream),
        line_buf: self.line_buf.clone(),
        nick: self.nick.clone(),
        channel: self.channel.clone(),
        last_say_instant: self.last_say_instant,
        log_path: self.log_path.clone(),
      }
    }).ok()
  }

  /// IRC initialization protocol implementation. Also, this function automatically joins the
  /// channel.
  fn init(&mut self) {
    let nick = self.nick.clone();

    self.write_line("USER a b c :d");
    self.write_line(&format!("NICK {}", nick));

    self.rejoin();
  }

  /// Read a line from IRC.
  fn read_line(&mut self) -> String {
    self.line_buf.clear();
    let _ = self.stream.read_line(&mut self.line_buf);
    self.line_buf.trim().to_owned()
  }

  /// Write a raw line to IRC.
  fn write_line(&mut self, msg: &str) {
    let stream = self.stream.get_mut();
    let _ = stream.write(msg.as_bytes());
    let _ = stream.write(&[b'\n']);
  }

  /// Join the channel.
  fn rejoin(&mut self) {
    let chan = self.channel.clone();
    self.write_line(&format!("JOIN {}", chan));
  }

  /// Handle an IRC ping by sending the approriate pong.
  fn handle_ping(&mut self, ping: String) {
    let pong = "PO".to_owned() + &ping[2..];
    println!("\x1b[36msending PONG: {}\x1b[0m", pong);
    self.write_line(&pong);
  }

  /// Say something on IRC.
  ///
  /// This function expects the message to say and the destination. It can be user logged on the
  /// server (not necessarily in the same channel) or nothing – in that case, the message will be
  /// delivered to the channel.
  fn say(&mut self, msg: &str, dest: Option<&str>) {
    if self.last_say_instant.elapsed() >= Duration::from_millis(MIN_MS_BETWEEN_SAYS) {
      self.last_say_instant = Instant::now();
      let header = "PRIVMSG ".to_owned();

      match dest {
        Some(user) => {
          self.write_line(&format!("PRIVMSG {} :{}", user, msg));
        },
        None => {
          let channel = &self.channel.clone();
          self.write_line(&format!("PRIVMSG {} :{}", channel, msg));
        }
      }
    }
  }

  /// Read the topic on the current channel.
  fn read_topic(&mut self) -> Option<String> {
    // ask for the topic
    let chan = irc.channel.clone();
    self.write_line(&format!("TOPIC {}", chan));
  
    // get the topic
    let topic = self.read_line();
  
    // clean it
    (&topic[1..]).find(':').map(|colon_ix| { // [1..] removes the first ':' (IRC proto)
      let colon_ix = colon_ix + 2;
      String::from((&topic[colon_ix..]).trim())
    })
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
}

/// Supported orders.
#[derive(Debug)]
enum Order {
  // from, to, content
  Tell(Nick, Nick, String),
  PrependTopic(String),
  ResetTopic(String),
  BotQuote(Vec<String>)
}

type Cmd = String;

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

/// Dispatch what users say.
fn dispatch_user_msg(irc: &mut IRCClient, nick: Nick, cmd: Cmd, args: Vec<String>) {

  match &cmd[..] {
    "PRIVMSG" if nick != irc.nick => {
      treat_privmsg(irc, nick, args);
    },
    "QUIT" if nick == irc.nick => {
      on_quit(irc, nick);
    },
    _ => {}
  }
}

// FIXME: too long function, we need to split it up
/// Action to take when a user said something.
fn treat_privmsg(irc: &mut IRCClient, nick: Nick, mut args: Vec<String>) {
  args[1].remove(0); // remove the leading ':' // FIXME: I guess we can remove that line
  let order = extract_order(nick.clone(), &args[1..]);

  match order {
    Some(Order::Tell(from, to, content)) => add_tell(irc, from, to, content),
    Some(Order::PrependTopic(topic)) => prepend_topic(irc, topic),
    Some(Order::ResetTopic(topic)) => reset_topic(irc, topic),
    Some(Order::BotQuote(words)) => bot_quote(irc, &words),
    None => {
      // someone just said something, and it’s not an order, see whether we should say something
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

  // add what that people say to the markov model
  {
    let words = &args[1..];
    
    if words.len() > 1 {
      irc.markov_chain.seen(&words[0]);
      irc.markov_chain.seen_first(&words[0]);

      for (word, next) in words.iter().zip(&words[1..]) {
        irc.markov_chain.seen(next);
        irc.markov_chain.account_next(word, next);
        irc.markov_chain.account_prev(next, word);
      }

      irc.markov_chain.seen_last(&words[words.len()-1]);
    }
  }

  // check whether we should say something stupid
  if irc.last_intervention.elapsed() >= Duration::from_secs(10) {
    let between = Range::new(0., 1.);
    let mut rng = rand::thread_rng();
    let speak_prob = between.ind_sample(&mut rng);

    println!("speak prob: {}", speak_prob);

    if speak_prob >= 0.98 {
      bot_quote(irc, &args[1..]);
    }
  }

  // look for URLs to scan
  let private = &args[0] == &irc.nick;
  scan_url(irc, nick, private, content);
}

// FIXME: too long
/// Scan a URL and try to retrieve its title. A thread is spawned to do the work and the function
/// immediately returns.
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

                // remove internal consecutive spaces
                let mut space_prev = false;
                let final_title: String = cleaned_title.chars().filter(move |&c| {
                  if c == ' ' {
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
                }).collect();

                match channel {
                  Some(nick) => irc.say(&format!("\x037«\x036 {} \x037»\x0F", final_title), Some(&nick)),
                  None => irc.say(&format!("\x037«\x036 {} \x037»\x0F", final_title), None),
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

/// Perform a HTTP GET at the given URL.
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

/// Check that a HTTP GET response is valid – i.e. check for the HTTP headers that we can read the
/// body.
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

/// What to do after we’ve quitted the channel.
fn on_quit(irc: &mut IRCClient, nick: String) {
  thread::sleep(Duration::from_secs(1));
  irc.rejoin();
}

/// Extract – if possible – an order from a message from someone.
fn extract_order(from: Nick, msg: &[String]) -> Option<Order> {
  match &(msg[0])[..] {
    "!tell" if msg.len() >= 3 => {
      // we have someone to tell something
      let to = msg[1].to_owned();
      let content = msg[2..].join(" ");

      Some(Order::Tell(from, to, content))
    },
    "!tropico" if msg.len() >= 2 => {
      // we want to prepend something to the current topic
      let content = msg[1..].join(" ");
      Some(Order::PrependTopic(content))
    },
    "!tropicoset" if msg.len() >= 2 => {
      // want to reset the current topic
      let content = msg[1..].join(" ");
      Some(Order::ResetTopic(content))
    },
    "!q" if msg.len() >= 2 => {
      // nothing to do; just ask for a uber cool quote!
      let words = (&msg[1..]).to_owned();
      Some(Order::BotQuote(words))
    },
    _ => None
  }
}

/// Record a tell message.
fn add_tell(irc: &mut IRCClient, from: Nick, to: Nick, content: String) {
  let mut msgs = irc.tells.get(&to).map_or(Vec::new(), |x| x.clone());
  msgs.push((from.to_ascii_lowercase(), content));
  irc.tells.insert(to.to_ascii_lowercase(), msgs);
  irc.save_tells();
}

/// Prepend a topic fragment to the current topic held in the channel.
fn prepend_topic(irc: &mut IRCClient, topic_part: String) {
  match read_topic(irc) {
    Some(ref topic) if topic_part != *topic => {
      let chan = irc.channel.clone();

      if topic.is_empty() {
        irc.write_line(&format!("TOPIC {} :{}", chan, topic_part)); // FIXME: sanitize
      } else {
        irc.write_line(&format!("TOPIC {} :{} · {}", chan, topic_part, topic)); // FIXME: sanitize
      }
    },
    _ => {
      println!("\x1b[31munable to get topic\x1b[0m");
    }
  }
}

/// Reset the current topic of the channel.
fn reset_topic(irc: &mut IRCClient, new_topic: String) {
  let chan = irc.channel.clone();
  irc.write_line(&format!("TOPIC {} :{}", chan, new_topic)); // FIXME: sanitize
}

// FIXME: #7
/// Generate something that we must say!
fn bot_quote(irc: &mut IRCClient, ctx_words: &[String]) {
  let mut rng = rand::thread_rng();

  let first_word = ctx_words[0].clone();
  let last_word = ctx_words[ctx_words.len()-1].clone();

  let mut prev_word = first_word.clone();
  let mut next_word = last_word.clone();

  let mut words: LinkedList<_> = ctx_words.iter().cloned().collect();

  let (mut hit_first, mut hit_last) = (false, false);

  let mut try_nb = 0;
  loop {
    if try_nb >= MAX_TRIES {
      words.clear();
      break;
    }

    if words.len() >= MAX_SENTENCE_WORDS_LEN {
      println!("exhausted words");

      prev_word = first_word.clone();
      next_word = last_word.clone();

      words.clear();
      words = ctx_words.iter().cloned().collect();

      hit_first = false;
      hit_last = false;

      try_nb += 1;
    }


    let next_words = irc.markov_chain.next_words(&next_word);
    if irc.markov_chain.prob_last(&next_word) >= LAST_PROB_THRESHOLD {
      hit_last = true;
    }

    let prev_words = irc.markov_chain.prev_words(&prev_word);
    if irc.markov_chain.prob_first(&prev_word) >= FIRST_PROB_THRESHOLD {
      hit_first = true;
    }

    // take the next word if we haven’t found the last word of the sentence yet
    if !hit_last {
      if next_words.is_empty() {
        // no more words and we’re not satisfied with the word we have; just go back
        next_word = words.pop_back().unwrap_or(last_word.clone());
        try_nb += 1;
        continue;
      }

      // spawn words with their frequencies so that we correctly pick up one
      let possible_words: Vec<_> = next_words.into_iter().flat_map(|(w, f)| repeat(w).take((f * 100.) as usize).collect::<Vec<_>>()).collect();

      // FIXME
      if possible_words.len() == 0 {
        try_nb += 1;
        continue;
      }

      let between = Range::new(0, possible_words.len());
      let next_word_index = between.ind_sample(&mut rng);

      next_word = possible_words[next_word_index].clone();
      words.push_back(next_word.clone());

      // if that word has a very high terminal probability, stop appending
      if irc.markov_chain.prob_last(&next_word) >= LAST_PROB_THRESHOLD {
        hit_last = true;
      }
    }

    // take the previous word if we haven’t found the first word of the sentence yet
    if !hit_first {
      if prev_words.is_empty() {
        // no more words and we’re not satisfied with the word we have; just go back
        prev_word = words.pop_front().unwrap_or(first_word.clone());
        try_nb += 1;
        continue;
      }

      // spawn words with their frequencies so that we correctly pick up one
      let possible_words: Vec<_> = prev_words.into_iter().flat_map(|(w, f)| repeat(w).take((f * 100.) as usize).collect::<Vec<_>>()).collect();

      // FIXME
      if possible_words.len() == 0 {
        try_nb += 1;
        continue;
      }

      let between = Range::new(0, possible_words.len());
      let prev_word_index = between.ind_sample(&mut rng);

      prev_word = possible_words[prev_word_index].clone();
      words.push_front(prev_word.clone());

      // if that word has a very high terminal probability, stop appending
      if irc.markov_chain.prob_first(&prev_word) >= FIRST_PROB_THRESHOLD {
        hit_first = true;
      }
    }

    if hit_first && hit_last {
      break;
    }
  }

  if !words.is_empty() {
    let words: Vec<String> = words.into_iter().collect();
    irc.say(&words.join(" "), None);
    irc.last_intervention = Instant::now();
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
    .arg(Arg::with_name("markov")
         .short("m")
         .long("markov")
         .value_name("FILE")
         .help("Path to the Markov graph")
         .takes_value(true))
    .get_matches();

  let host = options.value_of("host").unwrap();
  let channel = options.value_of("channel").unwrap();
  let nick = options.value_of("nick").unwrap();
  let tells_file = options.value_of("tells").unwrap_or("tells.json");
  let quotes_file = options.value_of("quotes").unwrap_or("quotes.log");

  // build the markov model using the log
  let markov_chain = if let Ok(file) = File::open(quotes_file) {
    let mut file = BufReader::new(file);
    MarkovChain::from_buf_read(&mut file)
  } else {
    println!("\x1b[31mno Markov data! learning from nothing\x1b[0m");
    MarkovChain::new()
  };

  // dump the markov chain into a file
  if let Ok(mut file) = File::create("/tmp/markov.txt") {
    ser::to_writer_pretty(&mut file, &markov_chain.chain).unwrap();
  }

  let port = 6667;
  let mut irc = IRCClient::connect(host, port, nick, channel, tells_file, quotes_file, markov_chain);

  irc.init();

  loop {
    let line = irc.read_line();
    println!("{}", line);

    if IRC::is_ping(&line) {
      irc.handle_ping(line);
      continue;
    }

    if let Some(user_msg) = extract_user_msg(&line) {
      dispatch_user_msg(&mut irc, user_msg.0, user_msg.1, user_msg.2);
    }
  }
}
