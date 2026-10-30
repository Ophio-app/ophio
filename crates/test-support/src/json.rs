// SPDX-License-Identifier: Apache-2.0
//! Generated input for parsers of another program's JSON output.
//!
//! Variants start from lines the program really writes, so they reach the
//! parser's branches, then replace, remove or add values anywhere in the tree.
//! Replacements include what parsers most often get wrong: integers past 2^53
//! and past 64 bits, fractions, empty, long and multibyte strings, control and
//! direction characters, containers where scalars belong, and the line's own
//! words in other places. The sequence is fixed by its seed, so a failure
//! repeats; report the failing variant with it.
use serde_json::{Map, Value, json};

#[derive(Debug)]
pub struct Variants {
    state: u64,
    words: Vec<String>,
}

impl Variants {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed | 1,
            words: Vec::new(),
        }
    }

    /// A copy of `line` with some of its values changed.
    pub fn of(&mut self, line: &Value) -> Value {
        self.words.clear();
        collect_words(line, &mut self.words);
        self.vary(line)
    }

    fn vary(&mut self, value: &Value) -> Value {
        if self.below(6) == 0 {
            return self.any(2);
        }
        match value {
            Value::Object(fields) => {
                let mut out = Map::new();
                for (key, field) in fields {
                    match self.below(12) {
                        0 => {}
                        1 => {
                            out.insert(key.clone(), self.any(2));
                        }
                        _ => {
                            out.insert(key.clone(), self.vary(field));
                        }
                    }
                }
                if self.below(8) == 0 {
                    let key = self.word();
                    out.insert(key, self.any(2));
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                let mut out: Vec<Value> = Vec::new();
                for item in items {
                    if self.below(12) != 0 {
                        out.push(self.vary(item));
                    }
                    if self.below(12) == 0 {
                        out.push(self.vary(item));
                    }
                }
                Value::Array(out)
            }
            scalar => scalar.clone(),
        }
    }

    fn any(&mut self, depth: u32) -> Value {
        match self.below(if depth == 0 { 4 } else { 6 }) {
            0 => Value::Null,
            1 => Value::Bool(self.below(2) == 0),
            2 => self.number(),
            3 => Value::String(self.string()),
            4 => (0..self.below(3)).map(|_| self.any(depth - 1)).collect(),
            _ => (0..self.below(3))
                .map(|_| (self.word(), self.any(depth - 1)))
                .collect::<Map<_, _>>()
                .into(),
        }
    }

    fn number(&mut self) -> Value {
        let edges = [
            json!(0),
            json!(1),
            json!(-1),
            json!(65_536),
            json!(4_294_967_296_u64),
            json!(9_007_199_254_740_991_u64),
            json!(9_007_199_254_740_992_u64),
            json!(i64::MAX),
            json!(i64::MIN),
            json!(u64::MAX),
            json!(0.5),
            json!(-2.5e-8),
            json!(1e300),
            json!(-1e300),
        ];
        let index = self.below(edges.len() as u64) as usize;
        edges[index].clone()
    }

    fn string(&mut self) -> String {
        match self.below(7) {
            0 => String::new(),
            1 => "é😀\u{202e}\u{0}\u{7}\n\r\t\"\\".into(),
            2 => "x".repeat(40_000),
            3 => "9007199254740993".into(),
            4 => "é".repeat(20_000),
            _ => self.word(),
        }
    }

    fn word(&mut self) -> String {
        if self.words.is_empty() {
            return "word".into();
        }
        let index = self.below(self.words.len() as u64) as usize;
        self.words[index].clone()
    }

    fn below(&mut self, n: u64) -> u64 {
        // xorshift64*
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1)
    }
}

fn collect_words(value: &Value, words: &mut Vec<String>) {
    match value {
        Value::String(text) if text.len() <= 64 => words.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_words(item, words)),
        Value::Object(fields) => {
            for (key, field) in fields {
                words.push(key.clone());
                collect_words(field, words);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_repeat_for_a_seed_and_change_the_line() {
        let line = json!({"type":"result","usage":{"input_tokens":1},"list":[1,"a"]});
        let first: Vec<_> = (0..64)
            .scan(Variants::new(7), |v, _| Some(v.of(&line)))
            .collect();
        let again: Vec<_> = (0..64)
            .scan(Variants::new(7), |v, _| Some(v.of(&line)))
            .collect();
        assert_eq!(first, again);
        assert!(first.iter().any(|v| *v != line));
        assert!(first.iter().any(|v| v["type"] == "result"));
    }
}
