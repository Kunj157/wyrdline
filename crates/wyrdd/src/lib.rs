//! `wyrdd`: the per-user Wyrdline daemon. Serves the HTTP API (plan §14.1)
//! over the session data directory, on a unix socket (same UID only) and on
//! loopback TCP (bearer token).

pub mod auth;
pub mod listen;
pub mod problem;
