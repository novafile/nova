use crate::Value;

/// A record: string keys mapped to values. Keys are unique. Entry order is
/// retained for presentation but is not part of the value (spec §2.10).
#[derive(Clone, Debug, Default)]
pub struct Record {
    entries: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateKey(pub String);

impl std::fmt::Display for DuplicateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "duplicate key {:?}", self.0)
    }
}

impl std::error::Error for DuplicateKey {}

impl Record {
    pub fn new() -> Self {
        Record::default()
    }

    pub fn from_entries(entries: Vec<(String, Value)>) -> Result<Self, DuplicateKey> {
        let mut r = Record::new();
        for (k, v) in entries {
            r.insert(k, v)?;
        }
        Ok(r)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Insert a new key; fails if the key already exists.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) -> Result<(), DuplicateKey> {
        let key = key.into();
        if self.contains_key(&key) {
            return Err(DuplicateKey(key));
        }
        self.entries.push((key, value));
        Ok(())
    }

    /// Insert or replace; returns the previous value.
    pub fn set(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        let key = key.into();
        match self.get_mut(&key) {
            Some(slot) => Some(std::mem::replace(slot, value)),
            None => {
                self.entries.push((key, value));
                None
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let idx = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(idx).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Value)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(k, _)| k)
    }

    pub fn sort_keys(&mut self) {
        self.entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    }

    pub fn into_entries(self) -> Vec<(String, Value)> {
        self.entries
    }
}
