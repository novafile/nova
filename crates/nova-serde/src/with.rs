//! Helpers for `#[serde(with = "...")]` to reach NOVA kinds that serde's data
//! model lacks. With other serde formats they degrade to natural forms.

/// Serialize any iterable as a NOVA `set` (duplicates are an error).
///
/// ```
/// # use serde::{Serialize, Deserialize};
/// # use std::collections::BTreeSet;
/// #[derive(Serialize, Deserialize)]
/// struct S { #[serde(with = "nova_serde::with::set")] tags: BTreeSet<String> }
/// ```
pub mod set {
    use nova_value::serde_support::magic;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    struct Seq<'a, T>(Vec<&'a T>);

    impl<T: Serialize> Serialize for Seq<'_, T> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_seq(self.0.iter())
        }
    }

    pub fn serialize<'a, S, C, T>(c: &'a C, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        &'a C: IntoIterator<Item = &'a T>,
        T: Serialize + 'a,
    {
        s.serialize_newtype_struct(magic::SET, &Seq(c.into_iter().collect()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>, C: Deserialize<'de>>(d: D) -> Result<C, D::Error> {
        C::deserialize(d)
    }
}

/// `std::time::Duration` as a NOVA `duration` (rather than `{secs, nanos}`).
pub mod duration {
    use serde::{de::Error as _, ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(d: &std::time::Duration, s: S) -> Result<S::Ok, S::Error> {
        nova_value::Duration::from_nanos(d.as_nanos() as i128).map_err(S::Error::custom)?.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<std::time::Duration, D::Error> {
        let n = nova_value::Duration::deserialize(d)?.as_nanos();
        if n < 0 {
            return Err(D::Error::custom("negative duration cannot be a std::time::Duration"));
        }
        Ok(std::time::Duration::new((n / 1_000_000_000) as u64, (n % 1_000_000_000) as u32))
    }
}

/// `std::time::SystemTime` as a NOVA `timestamp`.
pub mod system_time {
    use serde::{de::Error as _, ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let ts = match t.duration_since(UNIX_EPOCH) {
            Ok(d) => nova_value::Timestamp::new(d.as_secs() as i64, d.subsec_nanos()),
            Err(e) => {
                let d = e.duration();
                let (secs, nanos) = (d.as_secs() as i64, d.subsec_nanos());
                if nanos == 0 {
                    nova_value::Timestamp::new(-secs, 0)
                } else {
                    nova_value::Timestamp::new(-secs - 1, 1_000_000_000 - nanos)
                }
            }
        };
        ts.map_err(S::Error::custom)?.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let t = nova_value::Timestamp::deserialize(d)?;
        let (s, n) = (t.unix_seconds(), t.subsec_nanos());
        let r = if s >= 0 {
            UNIX_EPOCH.checked_add(Duration::new(s as u64, n))
        } else {
            UNIX_EPOCH
                .checked_sub(Duration::new(s.unsigned_abs(), 0))
                .and_then(|x| x.checked_add(Duration::new(0, n)))
        };
        r.ok_or_else(|| D::Error::custom("timestamp out of SystemTime range"))
    }
}
