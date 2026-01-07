use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub cmd: String,
    pub arg: Option<String>,
}

impl Request {
    pub fn new(cmd: &str, arg: Option<String>) -> Self {
        Self {
            cmd: cmd.to_string(),
            arg,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    pub status: Option<Status>,
}

impl Response {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            status: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            status: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub song: Option<SongInfo>,
    pub paused: bool,
    pub queue_len: usize,
    pub index: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SongInfo {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

pub fn send_request(socket_path: &Path, req: &Request) -> anyhow::Result<Response> {
    let stream = UnixStream::connect(socket_path)?;
    let mut writer = BufWriter::new(stream.try_clone()?);
    let mut reader = BufReader::new(stream);

    serde_json::to_writer(&mut writer, req)?;
    writer.write_all(b"\n")?;
    writer.flush()?;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let resp: Response = serde_json::from_str(&line)?;
    Ok(resp)
}
