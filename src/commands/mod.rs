//! One file per command: each exposes a `run`, and `main` does nothing but
//! parse the arguments and call one of them.

pub mod adapt;
pub mod add;
pub mod commit;
pub mod convert;
pub mod init;
pub mod log;
pub mod pull;
pub mod push;
pub mod remote;
pub mod restore;
pub mod status;
pub mod unstage;
