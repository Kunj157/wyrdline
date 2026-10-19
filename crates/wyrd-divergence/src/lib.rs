//! Intent–effect divergence: what an agent claimed versus what the ledger
//! shows (plan §17.4–17.5, ADR-006).
//!
//! Claims come from [`claims::extract_all`] (regex) and optionally an LLM
//! extractor; either way a claim whose quote is not verbatim in its turn is
//! dropped before any rule sees it. [`Config`] holds the matchers the rules
//! use: test and build runners, secret paths, build/cache directories,
//! scope, and expected hosts.

pub mod claims;
pub mod config;
pub mod glob;

pub use config::{Config, ConfigError};
