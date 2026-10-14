//! One file per command: each exposes a `run`, and `main` does nothing but
//! parse the arguments and call one of them.

pub mod adapt;
pub mod add;
pub mod commit;
pub mod convert;
pub mod ilog;
pub mod init;
pub mod iremote;
pub mod irepos;
pub mod iscan;
pub mod istatus;
pub mod log;
pub mod pull;
pub mod push;
pub mod remote;
pub mod repos;
pub mod restore;
pub mod search;
pub mod status;
pub mod unstage;
