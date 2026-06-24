//! Config service (T1): load/serialize `config.toml`, defaults, validation, and
//! first-run initialization (SPECS §6, §7, §8).

pub mod init;
pub mod load;
pub mod schema;
