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
    /// With `egress`, the cgroup connect programs enforce it too.
    Register {
        cgroup: PathBuf,
        workspace: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        egress: Option<crate::loader::EgressRules>,
    },
    /// Replace the session's egress rules (after registration, on the same
    /// connection), e.g. when allowed host names resolve differently.
    Egress { rules: crate::loader::EgressRules },
    /// Ask for a [`ServerMsg::Flushed`] with the same token once every event
    /// the kernel had queued for this session when the request arrived has
    /// been sent on this connection.
    Flush { token: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Registered {
        cgroup_id: u64,
        attached: usize,
        skipped: Vec<String>,
        /// Why requested egress rules are not enforced, if they are not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        egress_error: Option<String>,
    },
    Event {
        ts_mono_ns: u64,
        process: ProcessRef,
        body: EventBody,
    },
    /// Total ring-buffer drops since the daemon started, and the events this
    /// connection lost because its queue was full (the client was not reading
    /// fast enough). Both are cumulative.
    Drops {
        total: u64,
        #[serde(default)]
        queue_dropped: u64,
    },
    /// Answer to [`ClientMsg::Flush`]: every earlier event was sent before it.
    Flushed {
        token: u64,
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
    pub egress_error: Option<String>,
}

impl SensorClient {
    pub fn connect(socket: &Path, cgroup: &Path, workspace: &Path) -> Result<Self, ClientError> {
        Self::connect_with(socket, cgroup, workspace, None)
    }

    /// As [`SensorClient::connect`], asking the daemon to enforce `egress`.
    pub fn connect_with(
        socket: &Path,
        cgroup: &Path,
        workspace: &Path,
        egress: Option<crate::loader::EgressRules>,
    ) -> Result<Self, ClientError> {
        let mut stream = UnixStream::connect(socket).map_err(|source| ClientError::Connect {
            path: socket.to_path_buf(),
            source,
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut line = serde_json::to_vec(&ClientMsg::Register {
            cgroup: cgroup.to_path_buf(),
            workspace: workspace.to_path_buf(),
            egress,
        })?;
        line.push(b'\n');
        stream.write_all(&line)?;
        let mut reader = BufReader::new(stream);
        match read_msg(&mut reader)? {
            Some(ServerMsg::Registered {
                cgroup_id,
                attached,
                skipped,
                egress_error,
            }) => {
                reader.get_ref().set_read_timeout(None)?;
                Ok(SensorClient {
                    reader,
                    cgroup_id,
                    attached,
                    skipped,
                    egress_error,
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

    /// Replace the session's egress rules.
    pub fn send_egress(&self, rules: &crate::loader::EgressRules) -> Result<(), ClientError> {
        let mut line = serde_json::to_vec(&ClientMsg::Egress {
            rules: rules.clone(),
        })?;
        line.push(b'\n');
        let mut s: &UnixStream = self.reader.get_ref();
        s.write_all(&line)?;
        Ok(())
    }

    /// Ask the daemon for a delivery barrier (see [`ClientMsg::Flush`]).
    pub fn send_flush(&self, token: u64) -> Result<(), ClientError> {
        let mut line = serde_json::to_vec(&ClientMsg::Flush { token })?;
        line.push(b'\n');
        let mut s: &UnixStream = self.reader.get_ref();
        s.write_all(&line)?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_from_an_older_daemon_read_as_no_queue_loss() {
        let old: ServerMsg = serde_json::from_str(r#"{"type":"drops","total":4}"#).unwrap();
        assert_eq!(
            old,
            ServerMsg::Drops {
                total: 4,
                queue_dropped: 0,
            }
        );
    }

    #[test]
    fn drops_round_trip_with_the_queue_count() {
        let msg = ServerMsg::Drops {
            total: 1,
            queue_dropped: 9,
        };
        let text = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<ServerMsg>(&text).unwrap(), msg);
    }
}
