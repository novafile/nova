//! Identifier case styles and conversion.

/// A naming convention for keys and variant tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// `user_id` (the NOVA convention).
    Snake,
    /// `userId`
    Camel,
    /// `user-id`
    Kebab,
}

impl Case {
    pub fn parse(s: &str) -> Option<Case> {
        Some(match s {
            "snake" | "snake_case" => Case::Snake,
            "camel" | "camelCase" => Case::Camel,
            "kebab" | "kebab-case" => Case::Kebab,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Case::Snake => "snake_case",
            Case::Camel => "camelCase",
            Case::Kebab => "kebab-case",
        }
    }

    /// Convert an identifier to this case.
    pub fn convert(self, s: &str) -> String {
        let words = split_words(s);
        match self {
            Case::Snake => words.join("_"),
            Case::Kebab => words.join("-"),
            Case::Camel => words
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    if i == 0 {
                        w.clone()
                    } else {
                        let mut c = w.chars();
                        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                    }
                })
                .collect(),
        }
    }

    /// Is `s` already in this case?
    pub fn matches(self, s: &str) -> bool {
        !s.is_empty() && self.convert(s) == s
    }
}

/// Split `userID`, `HTTPServer2`, `user-name`, `user_name` into lowercase words.
pub fn split_words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let boundary = c.is_uppercase()
            && !cur.is_empty()
            && (prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) || (prev.is_some_and(char::is_uppercase) && next.is_some_and(char::is_lowercase)));
        if boundary {
            words.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        for (src, snake, camel, kebab) in [
            ("userId", "user_id", "userId", "user-id"),
            ("UserID", "user_id", "userId", "user-id"),
            ("HTTPServer", "http_server", "httpServer", "http-server"),
            ("ipv4Address", "ipv4_address", "ipv4Address", "ipv4-address"),
            ("user-name", "user_name", "userName", "user-name"),
            ("already_snake", "already_snake", "alreadySnake", "already-snake"),
            ("x", "x", "x", "x"),
        ] {
            assert_eq!(Case::Snake.convert(src), snake, "{src}");
            assert_eq!(Case::Camel.convert(src), camel, "{src}");
            assert_eq!(Case::Kebab.convert(src), kebab, "{src}");
        }
        assert!(Case::Snake.matches("deployed_at") && !Case::Snake.matches("deployedAt"));
    }
}
