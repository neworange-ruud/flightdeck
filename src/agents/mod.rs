//! Agent service (T4): registry from config, PATH validation, launch-command
//! building, and output→status classification (SPECS §8, §16, §17, §24).

pub mod adapter;
pub mod registry;
pub mod status;
