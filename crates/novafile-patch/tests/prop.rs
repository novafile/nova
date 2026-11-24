//! Property tests: a diff applied to its source gives its target.

#[path = "../../novafile-value/tests/support/strategy.rs"]
mod strategy;

use novafile_patch::{apply_unchecked, diff};
use novafile_value::revision;
use proptest::prelude::*;
use strategy::value;

proptest! {
    /// `apply(a, diff(a, b))` is `b`, with the revisions of `a` and `b`.
    #[test]
    fn diff_then_apply_gives_target(a in value(), b in value()) {
        let out = apply_unchecked(&a, &diff(&a, &b)).map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        prop_assert_eq!(out.base, revision(&a));
        prop_assert_eq!(out.revision, revision(&b));
        prop_assert_eq!(out.value, b);
    }

    /// A diff of a value against itself is empty and applies as a no-op.
    #[test]
    fn self_diff_is_empty(a in value()) {
        let p = diff(&a, &a);
        prop_assert!(p.ops.is_empty(), "{:?}", p.ops);
        prop_assert_eq!(apply_unchecked(&a, &p).unwrap().value, a);
    }
}
