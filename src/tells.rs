use serde_json::{de, ser};
use std::ascii::AsciiExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs::File;

pub type Nick = String;
pub type Message = String;

pub struct Tells {
  tells: HashMap<Nick, Vec<(Nick, Message)>>,
  tells_path: PathBuf
}

impl Tells {
  /// Create a new, empty set of tells.
  pub fn new<P>(path: P) -> Self where P: AsRef<Path> {
    Tells {
      tells: HashMap::new(),
      tells_path: path.as_ref().to_owned()
    }
  }

  /// Read tells from a JSON-formatted file.
  pub fn new_from_path<P>(path: P) -> Self where P: AsRef<Path> {
    match File::open(&path) {
      Ok(file) => {
        let tells = de::from_reader(file).unwrap();

        Tells {
          tells: tells,
          tells_path: path.as_ref().to_owned()
        }
      },
      Err(e) => {
        println!("\x1b[31munable to read tells from {:?}: {}\x1b[0m", path.as_ref(), e);
        Self::new(&path)
      }
    }
  }

  pub fn get(&self, nick: &str) -> Option<&Vec<(Nick, Message)>> {
    self.tells.get(nick)
  }

  pub fn remove(&mut self, nick: &str) {
    let _ = self.tells.remove(nick);
  }

  /// Save tells to a JSON-formatted file.
  pub fn save(&self) {
    match File::create(&self.tells_path) {
      Ok(mut file) => {
        let _ = ser::to_writer(&mut file, &self.tells);
      },
      Err(e) => {
        println!("\x1b[31munable to save tells to {:?}: {}\x1b[0m", &self.tells_path, e);
      }
    }
  }

  pub fn record(&mut self, from: &str, to: &str, content: &str) {
    let mut msgs = self.tells.get(to).map_or(Vec::new(), |x| x.clone());
    msgs.push((from.to_ascii_lowercase(), content.to_owned()));
    self.tells.insert(to.to_ascii_lowercase(), msgs);

    self.save();
  }
}
