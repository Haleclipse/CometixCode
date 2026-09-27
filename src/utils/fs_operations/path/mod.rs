//! Internal native lexical path implementation, imported from sugar_path 3.0.0.
//! Maps to CC utils/fsOperations.ts's node:path dependency; see ORIGIN.md.
//! Windows lexical corrections and remaining compatibility seams: ORIGIN.md.

mod borrowed;
mod cwd;
mod lexical;
mod normalize;
mod owned;
mod relative;
mod relative_string;
mod slash;
#[cfg(target_family = "windows")]
mod windows;
#[cfg(any(windows, test))]
pub(crate) mod windows_cwd;
#[cfg(any(windows, test))]
pub(crate) mod windows_lexical;
#[cfg(any(windows, test))]
pub(crate) mod windows_normalize;
#[cfg(any(windows, test))]
pub(crate) mod windows_relative;
#[cfg(any(target_family = "windows", test))]
pub(crate) mod windows_syntax;

pub(crate) use borrowed::SugarPath;
pub(crate) use owned::SugarPathBuf;

// The upstream sugar_path suite is preserved verbatim, including its
// self-spawning tests' raw std process/environment calls: each child reads the
// environment its parent spawned it with, which is exactly its startup capture.
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests;
