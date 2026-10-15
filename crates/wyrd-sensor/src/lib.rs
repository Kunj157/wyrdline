//! Userspace half of the eBPF sensor: load and attach the programs,
//! register session cgroups, read the ring buffer, decode raw events into
//! `wyrd-schema` effect events.

pub mod decode;
pub mod loader;
pub mod tracefs;

pub use decode::{Decoded, Decoder};
pub use loader::{LoadReport, Sensor, SensorConfig};

/// The compiled eBPF object.
pub static EBPF_OBJECT: &[u8] = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/wyrd-ebpf"));

#[derive(Debug, thiserror::Error)]
pub enum SensorError {
    #[error("tracefs: {0}")]
    Tracefs(String),
    #[error("eBPF load: {0}")]
    Load(String),
    #[error("eBPF map: {0}")]
    Map(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SensorError>;
