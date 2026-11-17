//! Records: string-keyed values with unique keys and presentation order (spec §2.10).

use crate::Value;
use std::collections::HashMap;

/// A record: string keys mapped to values. Keys are unique. Entry order is
/// retained for presentation but is not part of the value (spec §2.10).
///
/// Lookups are O(1): small records scan their entries, larger ones keep a
/// key index.
#[derive(Clone, Default)]
pub struct Record {
    /// Entries in insertion (presentation) order; keys are unique.
    entries: Vec<(String, Value)>,
    /// Key-to-position index, present only above [`INDEX_THRESHOLD`] entries.
    index: Option<HashMap<String, usize>>,
}

/// Records with more entries than this keep a hash index.
const INDEX_THRESHOLD: usize = 16;

/// Error from inserting a key that is already present; holds that key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateKey(pub String);

impl std::fmt::Display for DuplicateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "duplicate key {:?}", self.0)
    }
}

impl std::error::Error for DuplicateKey {}

#[expect(clippy::missing_fields_in_debug, reason = "`index` is a lookup cache derived from `entries`")]
impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Record").field("entries", &self.entries).finish()
    }
}

impl Record {
    /// An empty record.
    #[must_use]
    pub fn new() -> Self {
        Record::default()
    }

    /// An empty record with room for `n` entries.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Record {
            entries: Vec::with_capacity(n),
            index: None,
        }
    }

    /// Build a record from entries in order.
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateKey`] for the first key that appears twice.
    pub fn from_entries(entries: Vec<(String, Value)>) -> Result<Self, DuplicateKey> {
        let mut r = Record::with_capacity(entries.len());
        for (k, v) in entries {
            r.insert(k, v)?;
        }

        Ok(r)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the record has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Position of `key` in `entries`, via the index when there is one.
    fn position(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(key).copied(),
            None => self.entries.iter().position(|(k, _)| k == key),
        }
    }

    /// Rebuild the index from `entries`, or drop it at or below the threshold.
    fn reindex(&mut self) {
        self.index = (self.entries.len() > INDEX_THRESHOLD).then(|| self.entries.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect());
    }

    /// Append an entry without a duplicate check, keeping the index in sync.
    fn push(&mut self, key: String, value: Value) {
        if let Some(ix) = &mut self.index {
            ix.insert(key.clone(), self.entries.len());
            self.entries.push((key, value));
        } else {
            self.entries.push((key, value));
            if self.entries.len() > INDEX_THRESHOLD {
                self.reindex();
            }
        }
    }

    /// The value for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    /// Mutable access to the value for `key`.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.position(key).map(move |i| &mut self.entries[i].1)
    }

    /// Whether `key` is present.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    /// Insert a new key; fails if the key already exists.
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateKey`] if `key` is already present.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) -> Result<(), DuplicateKey> {
        let key = key.into();
        if self.contains_key(&key) {
            return Err(DuplicateKey(key));
        }

        self.push(key, value);
        Ok(())
    }

    /// Insert or replace; returns the previous value.
    pub fn set(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        let key = key.into();
        if let Some(i) = self.position(&key) {
            Some(std::mem::replace(&mut self.entries[i].1, value))
        } else {
            self.push(key, value);
            None
        }
    }

    /// Remove `key`, returning its value; later entries keep their relative order.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let idx = self.position(key)?;
        let v = self.entries.remove(idx).1;
        if self.index.is_some() {
            self.reindex();
        }

        Some(v)
    }

    /// Entries in presentation order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Entries in presentation order, with mutable values.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Value)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }

    /// Keys in presentation order.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(k, _)| k)
    }

    /// Sort entries by key in UTF-8 byte order (non-recursive; see [`Value::sort_keys`]).
    pub fn sort_keys(&mut self) {
        self.entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        if self.index.is_some() {
            self.reindex();
        }
    }

    /// The entries in presentation order.
    #[must_use]
    pub fn into_entries(self) -> Vec<(String, Value)> {
        self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_stays_consistent() {
        let mut r = Record::new();
        for i in 0..100 {
            r.insert(format!("k{i}"), Value::Int(i)).unwrap();
        }

        assert!(r.insert("k5", Value::Null).is_err());
        assert_eq!(r.remove("k3"), Some(Value::Int(3)));
        assert_eq!(r.get("k99"), Some(&Value::Int(99)));
        r.set("k99", Value::Int(0));
        assert_eq!(r.get("k99"), Some(&Value::Int(0)));
        r.sort_keys();
        assert_eq!(r.get("k10"), Some(&Value::Int(10)));
        assert!(!r.contains_key("k3"));
        assert_eq!(r.len(), 99);
    }
}
