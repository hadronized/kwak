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

mod cli;
mod irc;
mod markov;
mod tells;

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
  let conf = cli::new().get_matches();

  let host = conf.value_of("host").unwrap();
  let channel = conf.value_of("channel").unwrap();
  let nick = conf.value_of("nick").unwrap();
  let tells_path = conf.value_of("tells").unwrap_or("tells.json");
  let quotes_file = conf.value_of("log").unwrap_or("quotes.log");

  // build the markov model using the log
  let markov_chain = if let Ok(file) = File::open(log_path) {
    let mut file = BufReader::new(file);
    MarkovChain::from_buf_read(&mut file)
  } else {
    println!("\x1b[31mno Markov data! learning from nothing\x1b[0m");
    MarkovChain::new()
  };

  // create the IRC connection
  let mut irc = IRCClient::connect(host, IRC_PORT, nick, channel, tells_path, log_path, markov_chain);

  irc.init();

  loop {
    let line = irc.read_line();
    println!("{}", line);

    if IRC::is_ping(&line) {
      irc.handle_ping(line);
    } else if let Some((nick, cmd, args)) = extract_user_msg(&line) {
      dispatch_user_msg(&mut irc, nick, cmd, args);
    }
  }
}
