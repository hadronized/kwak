use std::collections::HashMap;
use std::io::BufRead;
use std::str::from_utf8;

/// A Markov chain implementation.
///
/// Each node of the chain is a word associated with a word state. There’re currently two types of
/// transitions to nagivate in the chain:
///
/// - forward transition
/// - backward transition
///
/// We call going from a word `A` to a word `B` a forward transition only if the word `B` can
/// appear *right* after `A`. We call going from a word `C` to a word `D` a backward transition only
/// if the word `C` can appear *right* before `D`.
///
/// The Markov chain also stores several useful information about word, such as the number of times
/// they appeared, in which position in the sentence, etc. That is used to compute probabilities.
#[derive(Clone, Debug)]
pub struct MarkovChain {
  chain: HashMap<Word, WordState>, // (next_word, count)
}

impl MarkovChain {
  /// Create a new empty markov chain.
  pub fn new() -> Self {
    MarkovChain {
      chain: HashMap::new()
    }
  }

  /// Create a new Markov chain from a `BufRead` value.
  pub fn from_buf_read<R>(buf_read: &mut R) -> Self where R: BufRead {
    let mut markov_chain = Self::new();

    for line in buf_read.lines() {
      let line = line.unwrap_or(String::new());
      let bytes = line.as_bytes();

      // convert the line to unicode
      let decoded = match from_utf8(bytes) {
        Ok(utf8_line) => {
          utf8_line.to_owned()
        },
        Err(e) => {
          println!("cannot decode as utf8: {:?}", e);
          bytes.iter().map(|b| *b as char).collect()
        }
      };

      let words: Vec<&str> = decoded.as_str().split_whitespace().collect();

      // if there’s at least one word (time nick word...)
      if words.len() > 2 {
        markov_chain.seen(&words[2]);
        markov_chain.seen_first(&words[2]);

        // if there’s at least two words (time nick word next...)
        if words.len() > 3 {
          for (word, next) in (&words[2..]).iter().zip(&words[3..]) {
            markov_chain.account_next(word, next);
            markov_chain.account_prev(next, word);
            markov_chain.seen(next);
          }
        }

        markov_chain.seen_last(&words[words.len()-1]);
      }
    }

    markov_chain
  }

  /// Take into account a next word for a given word.
  pub fn account_next(&mut self, word: &str, next_word: &str) {
    *self.chain.entry(word.to_owned()).or_insert(WordState::new())
      .next_words.entry(next_word.to_owned()).or_insert(0) += 1;
  }

  /// Take into account a previous word for a given word.
  pub fn account_prev(&mut self, word: &str, prev_word: &str) {
    *self.chain.entry(word.to_owned()).or_insert(WordState::new())
      .prev_words.entry(prev_word.to_owned()).or_insert(0) += 1;
  }

  /// Retrieve the list of words following a word with associated probabilities.
  pub fn next_words(&self, word: &str) -> Vec<(Word, f32)> {
    let words = self.chain.get(word).map_or(Vec::new(), |word_st| word_st.next_words.iter().collect());
    let tot = words.iter().fold(0, |tot, &(_, &count)| tot + count);

    words.into_iter().map(|(word, &count)| (word.clone(), count as f32 / tot as f32)).collect()
  }

  /// Retrieve the list of words preceding a word with associated probabilities.
  pub fn prev_words(&self, word: &str) -> Vec<(Word, f32)> {
    let words = self.chain.get(word).map_or(Vec::new(), |word_st| word_st.prev_words.iter().collect());
    let tot = words.iter().fold(0, |tot, &(_, &count)| tot + count);

    words.into_iter().map(|(word, &count)| (word.clone(), count as f32 / tot as f32)).collect()
  }

  /// Call that function when you see a word, whatever the word place is in the sentence.
  pub fn seen(&mut self, word: &str) {
    self.chain.entry(word.to_owned()).or_insert(WordState::new())
      .count += 1;
  }

  /// Call that function when you see a word at the beginning of a sentence.
  pub fn seen_first(&mut self, word: &str) {
    self.chain.entry(word.to_owned()).or_insert(WordState::new_with_count(1))
      .count_first += 1;
  }

  /// Call that function when you see a word at the end of a sentence.
  pub fn seen_last(&mut self, word: &str) {
    self.chain.entry(word.to_owned()).or_insert(WordState::new_with_count(1))
      .count_last += 1;
  }

  /// Get the probability that this word is met at the beginning of a line.
  pub fn prob_first(&self, word: &str) -> f32 {
    self.chain.get(word).map_or(0., |word_st| word_st.count_first as f32 / word_st.count as f32)
  }

  /// Get the probability that this word is met at the end of a line.
  pub fn prob_last(&self, word: &str) -> f32 {
    self.chain.get(word).map_or(0., |word_st| word_st.count_last as f32 / word_st.count as f32)
  }
}

pub type Word = String;

/// State associated with a word.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WordState {
  count: u32, // number of times this word was seen at all
  count_first: u32, // number of times this word was seen as first word of a sentence
  count_last: u32, // number of times this word was seen as last word of a sentence
  next_words: HashMap<Word, u32>, // next word with associated counts
  prev_words: HashMap<Word, u32> // previous word with associated counts
}

impl WordState {
  /// Create a new null word state.
  pub fn new() -> Self {
    WordState {
      count: 0,
      count_first: 0,
      count_last: 0,
      next_words: HashMap::new(),
      prev_words: HashMap::new(),
    }
  }

  /// Create a new null word state by providing the number of time the word was seen.
  pub fn new_with_count(count: u32) -> Self {
    WordState {
      count: count,
      count_first: 0,
      count_last: 0,
      next_words: HashMap::new(),
      prev_words: HashMap::new()
    }
  }
}
