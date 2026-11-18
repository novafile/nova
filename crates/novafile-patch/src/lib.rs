//! Partial reads and validated partial writes (spec §7): the interaction
//! model where agents fetch only the fields they need and send back small,
//! conditional, atomically applied patches.

mod diff;
mod nav;
mod patch;
mod query;
mod source;

pub use diff::diff;
pub use nav::get_mut;
pub use patch::{Applied, Op, OpKind, Patch, PatchError, apply, parse_patch};
pub use query::{Response, get, parse_request, select};
pub use source::{SourceEdit, apply_to_source};

/// Apply with no extra validation.
///
/// # Errors
///
/// Returns a [`PatchError`] under the same conditions as [`apply`], except
/// [`PatchError::Invalid`].
pub fn apply_unchecked(doc: &novafile_value::Value, patch: &Patch) -> Result<Applied, PatchError> {
    apply(doc, patch, &|_| Ok(()))
}
