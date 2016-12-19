use std::io::BufReader;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use http::{is_http_response_valid, http_get};
use tells::Nick;

const IRC_PORT: u16 = 6667;
const MIN_MS_BETWEEN_SAYS: u64 = 500;

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

type Cmd = String;

/// Supported orders.
#[derive(Debug)]
enum Order {
  // from, to, content
  Tell(Nick, Nick, String),
  PrependTopic(String),
  ResetTopic(String),
  BotQuote(Vec<String>)
}

/// IRC client.
struct IRC {
  /// TCP stream holding the IRC connection.
  stream: Arc<Mutex<TcpStream>>,
  /// Line buffer used to read from IRC.
  line_buf: String,
  /// Our nickname.
  nick: String,
  /// The current channel we’re connected to.
  channel: String,
  /// Last time we said something.
  last_say_instant: Instant,
  /// Path to the log file.
  log_path: PathBuf,
  /// Channel used when a URL has been correctly scanned.
  url_scan_channel: (Sender<String>, Receiver<String>),
}

impl IRC {
  /// Connect to an IRC server with a given name, then join the given channel. All messages will be
  /// registered to the given log path.
  fn connect(addr: &str, port: u16, nick: &str, channel: &str, log_path: &str) -> Self {
    let stream = Arc::new(Mutex::new(TcpStream::connect((addr, port)).unwrap()));

    IRCClient {
      stream: stream,
      line_buf: String::with_capacity(1024),
      nick: nick.to_owned(),
      channel: channel.to_owned(),
      last_say_instant: Instant::now(),
      log_path: Path::new(log_path).to_owned(),
      url_scan_channel: channel()
    }
  }

  /// IRC initialization protocol implementation. Also, this function automatically joins the
  /// channel.
  fn init(&mut self) {
    let nick = self.nick.clone();

    self.write_line(&format!("USER a b c :d\nNICK {}", nick));
    self.rejoin();
  }

  /// Read a line from IRC.
  fn read_line(&mut self) -> String {
    self.line_buf.clear();
    let _ = self.stream.read_line(&mut self.line_buf);
    self.line_buf.trim().to_owned()
  }

  /// Write a line to IRC.
  fn write_line(&mut self, msg: &str) {
    let stream = self.stream.lock().unwrap();
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

  // FIXME: too long
  /// Scan a URL and try to retrieve its title. A thread is spawned to do the work and the function
  /// immediately returns.
  fn url_scan(irc: &mut IRCClient, nick: Nick, private: bool, content: String) {
    let re_match = RE_URL.find(&content);
  
    if let Some((start_index, end_index)) = re_match {
      // clone a few stuff to bring with us in the thread
      let channel = if private { Some(nick.clone()) } else { None };
      let sx = self.url_scan_channel.0; // used to notify the main thread when we’re done
  
      let _ = thread::spawn(move || url_scan_work(&content));
    }
  }

  fn url_scan_work(content: &str) {
    let url = &content[start_index .. end_index];

    // fix some URLs that might cause problems
    let (url, fixed_url_method) = fix_url(&url);

    match http_get(&url) {
      Ok(mut response) => {
        // inspect the header to deny big things
        if !is_http_response_valid(&url, &response.headers) {
          return;
        }

        let mut body = String::new();
        let _ = response.read_to_string(&mut body);

        // find the title
        let title = find_title(&body, fixed_url_method);
        Some(nick) => Some((format!("\x037«\x036 {} \x037»\x0F", final_title), Some(&nick))),
      },
      Err(e) => {
        println!("\x1b[31munable to get {}: {:?}\x1b[0m", url, e);
        None
      }
    }
  }

  /// Fix some URLs
  fn fix_url(url: &str) -> (String, Option<FixedURLMethod>) {
    let youtube_video_id = RE_YOUTUBE.captures(&url);
    let youtu_be_video_id = RE_YOUTU_BE.captures(&url);

    match (&youtube_video_id, &youtu_be_video_id) {
      (&Some(ref captures), _) | (_, &Some(ref captures)) => {
        if let Some(video_id) = captures.at(1) {
          (format!("http://www.infinitelooper.com/?v={}", video_id), FixedURLMethod::Youtube)
        } else {
          (String::new(), None)
        }
      },
      _ => (url.to_owned(), None)
    }
  }

  fn find_title(body: &str, fixed_url_method: FixedURLMethod) -> String {
    if let Some(captures) = RE_TITLE.captures(body) {
      if let Some(title) = captures.at(1) {
        let mut cleaned_title: String = title.chars().filter(|c| *c != '\n').collect();

        // decode entities ; if we cannot, just dump the title as-is
        cleaned_title = decode_html_entities(&cleaned_title).unwrap_or(cleaned_title).trim();

        if let Some(FixedURLMethod::Youtube) = fixed_url_method {
          cleaned_title = fix_youtube_title(cleaned_title);
        }

        // remove internal consecutive spaces and output
        remove_consecutive_whitespaces(&cleaned_title);
      }
    }
  }

  /// Fix a Youtube title.
  fn fix_youtube_title(title: &str) -> &str {
    &title[17..]
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

  /// Dispatch what users say.
  fn dispatch_user_msg(&mut self, nick: Nick, cmd: Cmd, args: Vec<String>) {
    match &cmd[..] {
      "PRIVMSG" if nick != irc.nick => {
        self.treat_privmsg(nick, args);
      },
      "QUIT" if nick == irc.nick => self.on_quit(),
      _ => {}
    }
  }

  // FIXME: too long function, we need to split it up
  /// Action to take when a user said something.
  fn treat_privmsg(&mut self, nick: Nick, mut args: Vec<String>) {
    args[1].remove(0); // remove the leading ':' // FIXME: I guess we can remove that line
    let order = Self::extract_order(nick.clone(), &args[1..]);
  
    match order {
      //Some(Order::Tell(from, to, content)) => add_tell(irc, from, to, content), FIXME: tells
      Some(Order::PrependTopic(topic)) => self.prepend_topic(topic),
      Some(Order::ResetTopic(topic)) => self.reset_topic(topic),
      //Some(Order::BotQuote(words)) => bot_quote(irc, &words), FIXME: markov
      None => {
        // someone just said something, and it’s not an order, see whether we should say something
        if let Some(msgs) = .tells.get(&nick.to_ascii_lowercase()).cloned() {
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

  /// What to do after we’ve quitted the channel.
  fn on_quit(&mut self) {
    thread::sleep(Duration::from_secs(1));
    self.rejoin();
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

  /// Prepend a topic fragment to the current topic held in the channel.
  fn prepend_topic(&mut self, topic_part: String) {
    match self.read_topic() {
      Some(ref topic) if topic_part != *topic => {
        let chan = self.channel.clone();
  
        if topic.is_empty() {
          self.write_line(&format!("TOPIC {} :{}", chan, topic_part)); // FIXME: sanitize
        } else {
          self.write_line(&format!("TOPIC {} :{} · {}", chan, topic_part, topic)); // FIXME: sanitize
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
}

#[derive(Clone)]
enum FixedURLMethod {
  Youtube
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
