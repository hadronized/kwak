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
