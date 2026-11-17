//! Token-aware representation selection (spec §10).
//!
//! Exact token counts depend on the target model's tokenizer, so selection is
//! parameterized by [`TokenCounter`]. [`HeuristicCounter`] is a deterministic
//! stand-in for BPE tokenizers that is good enough to rank representations;
//! plug in a real tokenizer for production decisions.

use crate::writer::{Profile, to_text};
use novafile_value::Value;

pub trait TokenCounter {
    fn count(&self, text: &str) -> usize;
}

/// Approximates subword tokenizers: letter runs cost ~1 token per 6 chars,
/// digit runs 1 per 3, each punctuation mark 1, a newline plus indentation 1,
/// and single spaces fold into the following word.
pub struct HeuristicCounter;

impl TokenCounter for HeuristicCounter {
    fn count(&self, text: &str) -> usize {
        let mut n = 0;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_alphabetic() {
                let mut len: usize = 1;
                while chars.peek().is_some_and(|c| c.is_alphabetic()) {
                    chars.next();
                    len += 1;
                }
                n += len.div_ceil(6);
            } else if c.is_ascii_digit() {
                let mut len: usize = 1;
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                    len += 1;
                }
                n += len.div_ceil(3);
            } else if c == '\n' {
                while chars.peek() == Some(&' ') {
                    chars.next();
                }
                n += 1;
            } else if c == ' ' {
                if chars.peek() == Some(&' ') {
                    n += 1;
                    while chars.peek() == Some(&' ') {
                        chars.next();
                    }
                }
            } else {
                n += 1;
            }
        }
        n
    }
}

/// A candidate representation and its measured cost.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub profile: Profile,
    pub text: String,
    pub tokens: usize,
}

/// Render `v` in every value-preserving text profile and return them sorted
/// by token count (cheapest first). Callers that also weigh expected output
/// tokens or failure rates (spec §10) can re-rank the list.
pub fn rank_profiles(v: &Value, counter: &dyn TokenCounter) -> Vec<Candidate> {
    let mut c: Vec<Candidate> = [Profile::Compact, Profile::Human]
        .into_iter()
        .map(|p| {
            let text = to_text(v, p);
            Candidate {
                profile: p,
                tokens: counter.count(&text),
                text,
            }
        })
        .collect();
    c.sort_by_key(|c| c.tokens);
    c
}
