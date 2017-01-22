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

use std::fs::File;
use std::io::BufReader;

mod cli;
mod http;
mod irc;
mod markov;
mod tells;

use irc::IRC;
use markov::MarkovChain;
use tells::Tells;

const IRC_PORT: u16 = 6667;

fn main() {
  let conf = cli::new().get_matches();

  let host = conf.value_of("host").unwrap();
  let channel = conf.value_of("channel").unwrap();
  let nick = conf.value_of("nick").unwrap();
  let tells_path = conf.value_of("tells").unwrap_or("tells.json");
  let log_path = conf.value_of("log").unwrap_or("log.txt");

  // build the markov model using the log
  let markov_chain = if let Ok(file) = File::open(log_path) {
    let mut file = BufReader::new(file);
    MarkovChain::from_buf_read(&mut file)
  } else {
    println!("\x1b[31mno Markov data! learning from nothing\x1b[0m");
    MarkovChain::new()
  };

  // reload the tells
  let tells = Tells::new_from_path(&tells_path);

  //// create the IRC connection
  let mut irc = IRC::connect(host, IRC_PORT, nick, channel, tells, markov_chain, log_path);

  irc.init();
  irc.run();
}
