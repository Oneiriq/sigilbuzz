//! Shared helpers used by more than one subcommand.
//!
//! The scaffold commit only carries the [`CliResult`] alias; concrete
//! parsers (gid lists, unicode lists, feature lists, …) land alongside
//! the subcommand that first needs them.

/// CLI-level error alias. Concrete errors are stringified at the
/// subcommand boundary so the dispatcher only has to print them.
pub type CliResult<T = ()> = Result<T, String>;
