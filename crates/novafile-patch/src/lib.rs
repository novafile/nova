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
pub use patch::{apply, parse_patch, Applied, Op, OpKind, Patch, PatchError};
pub use query::{get, select, Response};
pub use source::{apply_to_source, SourceEdit};

/// Apply with no extra validation.
pub fn apply_unchecked(doc: &novafile_value::Value, patch: &Patch) -> Result<Applied, PatchError> {
    apply(doc, patch, &|_| Ok(()))
}
