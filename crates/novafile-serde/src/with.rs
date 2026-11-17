//! Helpers for `#[serde(with = "...")]` to reach NOVA kinds that serde's data
//! model lacks. With other serde formats they degrade to natural forms.

/// Serialize any iterable as a NOVA `set` (duplicates are an error).
///
/// ```
/// # use serde::{Serialize, Deserialize};
/// # use std::collections::BTreeSet;
/// #[derive(Serialize, Deserialize)]
/// struct S { #[serde(with = "novafile_serde::with::set")] tags: BTreeSet<String> }
/// ```
pub mod set {
    use novafile_value::serde_support::magic;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    struct Seq<'a, T>(Vec<&'a T>);

    impl<T: Serialize> Serialize for Seq<'_, T> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_seq(self.0.iter())
        }
    }

    /// Serialize `c` as a set.
    ///
    /// # Errors
    ///
    /// Fails if the serializer does; the NOVA serializer rejects duplicate elements.
    pub fn serialize<'a, S, C, T>(c: &'a C, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        &'a C: IntoIterator<Item = &'a T>,
        T: Serialize + 'a,
    {
        s.serialize_newtype_struct(magic::SET, &Seq(c.into_iter().collect()))
    }

    /// Deserialize a set (or any sequence) into `C`.
    ///
    /// # Errors
    ///
    /// Fails if the input is not a sequence `C` accepts.
    pub fn deserialize<'de, D: Deserializer<'de>, C: Deserialize<'de>>(d: D) -> Result<C, D::Error> {
        C::deserialize(d)
    }
}

/// `std::time::Duration` as a NOVA `duration` (rather than `{secs, nanos}`).
pub mod duration {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};

    /// Serialize `d` as a NOVA duration.
    ///
    /// # Errors
    ///
    /// Fails if `d` exceeds the NOVA duration range or the serializer fails.
    pub fn serialize<S: Serializer>(d: &std::time::Duration, s: S) -> Result<S::Ok, S::Error> {
        novafile_value::Duration::from_nanos(d.as_nanos().cast_signed())
            .map_err(S::Error::custom)?
            .serialize(s)
    }

    /// Deserialize a NOVA duration.
    ///
    /// # Errors
    ///
    /// Fails if the input is not a duration or the duration is negative.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<std::time::Duration, D::Error> {
        let n = novafile_value::Duration::deserialize(d)?.as_nanos();
        if n < 0 {
            return Err(D::Error::custom("negative duration cannot be a std::time::Duration"));
        }

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "n is non-negative and a NOVA duration is at most i64::MAX seconds"
        )]
        Ok(std::time::Duration::new((n / 1_000_000_000) as u64, (n % 1_000_000_000) as u32))
    }
}

/// `std::time::SystemTime` as a NOVA `timestamp`.
pub mod system_time {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Serialize `t` as a NOVA timestamp.
    ///
    /// # Errors
    ///
    /// Fails if `t` lies outside the years 0001-9999 or the serializer fails.
    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let to_secs = |d: Duration| i64::try_from(d.as_secs()).map_err(|_| "timestamp out of range (years 0001-9999)".to_owned());
        let ts = match t.duration_since(UNIX_EPOCH) {
            Ok(d) => to_secs(d).and_then(|s| novafile_value::Timestamp::new(s, d.subsec_nanos())),
            Err(e) => {
                let d = e.duration();
                let (secs, nanos) = (to_secs(d).map_err(S::Error::custom)?, d.subsec_nanos());
                if nanos == 0 {
                    novafile_value::Timestamp::new(-secs, 0)
                } else {
                    novafile_value::Timestamp::new(-secs - 1, 1_000_000_000 - nanos)
                }
            }
        };

        ts.map_err(S::Error::custom)?.serialize(s)
    }

    /// Deserialize a NOVA timestamp.
    ///
    /// # Errors
    ///
    /// Fails if the input is not a timestamp or it is out of `SystemTime`'s range.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let t = novafile_value::Timestamp::deserialize(d)?;
        let (secs, nanos) = (t.unix_seconds(), t.subsec_nanos());
        let r = if secs >= 0 {
            UNIX_EPOCH.checked_add(Duration::new(secs.unsigned_abs(), nanos))
        } else {
            UNIX_EPOCH
                .checked_sub(Duration::new(secs.unsigned_abs(), 0))
                .and_then(|x| x.checked_add(Duration::new(0, nanos)))
        };

        r.ok_or_else(|| D::Error::custom("timestamp out of SystemTime range"))
    }
}
