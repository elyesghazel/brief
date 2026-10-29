//! What differs between Linux and Windows: shell, opener, notifications, keyring, editor and a
//! few defaults. Everything else in brief is the same code on both.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
