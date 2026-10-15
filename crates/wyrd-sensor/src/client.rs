//! Protocol between `wyrd-sensord` and its clients, and the client.
//!
//! One connection per session: the client sends `register` once; the
//! daemon answers `registered` (or `error`) and then streams `event` and
//! periodic `drops` lines until either side closes the connection, at
//! which point the cgroup is unregistered.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wyrd_schema::{EventBody, ProcessRef};

/// Default socket of the system service.
pub const DEFAULT_SOCKET: &str = "/run/wyrdline/sensord.sock";

/// Socket path from `WYRD_SENSORD_SOCKET`, else [`DEFAULT_SOCKET`].
pub fn socket_path() -> PathBuf {
    std::env::var_os("WYRD_SENSORD_SOCKET")
        .map_or_else(|| PathBuf::from(DEFAULT_SOCKET), PathBuf::from)
}

/// Largest request line the daemon accepts.
pub const MAX_REQUEST: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Stream the events of `cgroup` (a directory under /sys/fs/cgroup the
    /// caller owns); paths under `workspace` are marked as inside the Cell.
    Register { cgroup: PathBuf, workspace: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Registered {
        cgroup_id: u64,
        attached: usize,
        skipped: Vec<String>,
    },
    Event {
        ts_mono_ns: u64,
        process: ProcessRef,
        body: EventBody,
    },
    /// Total ring-buffer drops since the daemon started.
    Drops {
        total: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("sensor daemon at {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sensor daemon: {0}")]
    Io(#[from] std::io::Error),
    #[error("sensor daemon sent an invalid message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sensor daemon refused: {0}")]
    Refused(String),
    #[error("sensor daemon closed the connection")]
    Closed,
}

/// A registered session's event stream.
#[derive(Debug)]
pub struct SensorClient {
    reader: BufReader<UnixStream>,
    pub cgroup_id: u64,
    pub attached: usize,
    pub skipped: Vec<String>,
}

impl SensorClient {
    pub fn connect(socket: &Path, cgroup: &Path, workspace: &Path) -> Result<Self, ClientError> {
        let mut stream = UnixStream::connect(socket).map_err(|source| ClientError::Connect {
            path: socket.to_path_buf(),
            source,
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut line = serde_json::to_vec(&ClientMsg::Register {
            cgroup: cgroup.to_path_buf(),
            workspace: workspace.to_path_buf(),
        })?;
        line.push(b'\n');
        stream.write_all(&line)?;
        let mut reader = BufReader::new(stream);
        match read_msg(&mut reader)? {
            Some(ServerMsg::Registered {
                cgroup_id,
                attached,
                skipped,
            }) => {
                reader.get_ref().set_read_timeout(None)?;
                Ok(SensorClient {
                    reader,
                    cgroup_id,
                    attached,
                    skipped,
                })
            }
            Some(ServerMsg::Error { message }) => Err(ClientError::Refused(message)),
            Some(other) => Err(ClientError::Refused(format!("unexpected {other:?}"))),
            None => Err(ClientError::Closed),
        }
    }

    /// Next message; `None` when the daemon closed the connection.
    pub fn recv(&mut self) -> Result<Option<ServerMsg>, ClientError> {
        read_msg(&mut self.reader)
    }

    /// Bound how long `recv` may block (`None` = forever).
    pub fn set_timeout(&self, t: Option<Duration>) -> Result<(), ClientError> {
        Ok(self.reader.get_ref().set_read_timeout(t)?)
    }
}

fn read_msg(r: &mut BufReader<UnixStream>) -> Result<Option<ServerMsg>, ClientError> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}
