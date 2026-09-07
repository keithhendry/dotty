//! Building blocks shared by the [`crate::cmds`] command implementations.
//!
//! Each module owns one concern rather than one command, so a single command
//! typically composes several of them: [`path`] works out where a file belongs,
//! [`fs`] moves it and leaves a symlink behind, and [`git`] records the result.
//!
//! Every fallible function in these modules returns `Result<_, String>`, where
//! the error is a complete, human readable sentence ready to be logged. There
//! is no custom error type; new code should follow the same convention.

pub mod fs;
pub mod git;
pub mod path;
pub mod string;
