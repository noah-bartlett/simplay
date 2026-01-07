use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc::Sender, Mutex};
use std::thread;
use std::time::Duration;

pub enum MpvEvent {
    EndFile { reason: Option<String> },
}

struct MpvIpc {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
    next_id: u64,
}

pub struct MpvController {
    ipc_path: PathBuf,
    ipc: Mutex<MpvIpc>,
    _child: Child,
}

impl MpvController {
    pub fn spawn(ipc_path: &Path) -> Result<Self> {
        if ipc_path.exists() {
            fs::remove_file(ipc_path).ok();
        }

        let mpv_bin = env::var("SIMPLAY_MPV").unwrap_or_else(|_| "mpv".to_string());
        let mut cmd = Command::new(&mpv_bin);
        cmd.arg("--no-video")
            .arg("--idle=yes")
            .arg("--keep-open=yes")
            .arg("--audio-display=no")
            .arg("--no-terminal")
            .arg("--input-terminal=no")
            .arg("--msg-level=all=error")
            .arg(format!("--input-ipc-server={}", ipc_path.display()));
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                anyhow!(
                    "mpv binary '{}' not found. Install mpv or set SIMPLAY_MPV to its path.",
                    mpv_bin
                )
            } else {
                anyhow!(err)
            }
        })?;

        let mut attempts = 0;
        while !ipc_path.exists() {
            if attempts > 40 {
                return Err(anyhow!("Timed out waiting for mpv IPC socket"));
            }
            attempts += 1;
            thread::sleep(Duration::from_millis(50));
        }

        let stream = UnixStream::connect(ipc_path).context("Failed to connect mpv IPC")?;
        let reader = BufReader::new(stream.try_clone()?);
        let writer = BufWriter::new(stream);
        let ipc = MpvIpc {
            reader,
            writer,
            next_id: 1,
        };

        Ok(Self {
            ipc_path: ipc_path.to_path_buf(),
            ipc: Mutex::new(ipc),
            _child: child,
        })
    }

    pub fn start_event_loop(&self, tx: Sender<MpvEvent>) -> Result<()> {
        let stream = UnixStream::connect(&self.ipc_path).context("Failed to connect mpv event IPC")?;
        thread::spawn(move || {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(_) => break,
                };
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value.get("event").and_then(|v| v.as_str()) == Some("end-file") {
                        let reason = value
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let _ = tx.send(MpvEvent::EndFile { reason });
                    }
                }
            }
        });
        Ok(())
    }

    pub fn load(&self, url: &str) -> Result<()> {
        self.command(json!(["loadfile", url, "replace"]))?;
        Ok(())
    }

    pub fn pause(&self, paused: bool) -> Result<()> {
        self.command(json!(["set_property", "pause", paused]))?;
        Ok(())
    }

    pub fn seek_absolute(&self, position: f64) -> Result<()> {
        self.command(json!(["seek", position, "absolute"]))?;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.command(json!(["stop"]))?;
        Ok(())
    }

    pub fn set_volume(&self, volume: f64) -> Result<()> {
        self.command(json!(["set_property", "volume", volume]))?;
        Ok(())
    }

    pub fn get_volume(&self) -> Result<f64> {
        let resp = self.command(json!(["get_property", "volume"]))?;
        let volume = resp
            .get("data")
            .and_then(|v| v.as_f64())
            .unwrap_or(100.0);
        Ok(volume)
    }

    pub fn get_time_pos(&self) -> Result<Option<f64>> {
        let resp = self.command(json!(["get_property", "time-pos"]))?;
        if resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("success")
            != "success"
        {
            return Ok(None);
        }
        Ok(resp.get("data").and_then(|v| v.as_f64()))
    }

    fn command(&self, command: Value) -> Result<Value> {
        let mut ipc = self.ipc.lock().expect("mpv ipc lock");
        let request_id = ipc.next_id;
        ipc.next_id += 1;

        let payload = json!({
            "command": command,
            "request_id": request_id,
        });
        serde_json::to_writer(&mut ipc.writer, &payload)?;
        ipc.writer.write_all(b"\n")?;
        ipc.writer.flush()?;

        loop {
            let mut line = String::new();
            let bytes = ipc.reader.read_line(&mut line)?;
            if bytes == 0 {
                return Err(anyhow!("mpv IPC closed"));
            }
            let value: Value = serde_json::from_str(&line)?;
            if value
                .get("request_id")
                .and_then(|v| v.as_u64())
                == Some(request_id)
            {
                return Ok(value);
            }
        }
    }
}
