//! Unix socket between `bspx <command>` and the running daemon.
//! One request per connection: a single line in, the reply out.
//! Replies start with `ok` or `err`, followed by the message.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("BSPX_SOCKET") {
        return PathBuf::from(p);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    std::env::temp_dir().join(format!("bspx_{user}.sock"))
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Binds the socket. Fails if another daemon is already answering on it;
    /// a stale socket file left by a crash is replaced.
    pub fn bind(path: &Path) -> io::Result<Self> {
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "bspx is already running",
                ));
            }
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
        })
    }

    /// Answers every pending request with `handle`. Never blocks waiting for
    /// new connections.
    pub fn poll(&self, mut handle: impl FnMut(&str) -> Result<String, String>) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) => {
                    log::warn!("socket accept failed: {e}");
                    return;
                }
            };
            if let Err(e) = Self::serve(stream, &mut handle) {
                log::warn!("socket request failed: {e}");
            }
        }
    }

    fn serve(
        stream: UnixStream,
        handle: &mut impl FnMut(&str) -> Result<String, String>,
    ) -> io::Result<()> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        let mut line = String::new();
        BufReader::new(&stream).take(4096).read_line(&mut line)?;
        if line.trim().is_empty() {
            // A liveness probe (see `bind`) or a client that gave up.
            return Ok(());
        }
        let reply = match handle(line.trim()) {
            Ok(msg) => format!("ok\n{msg}"),
            Err(msg) => format!("err\n{msg}"),
        };
        (&stream).write_all(reply.as_bytes())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Sends one command and returns the daemon's verdict and message.
pub fn send(path: &Path, command: &str) -> io::Result<Result<String, String>> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    let (status, msg) = reply.split_once('\n').unwrap_or((reply.as_str(), ""));
    Ok(match status {
        "ok" => Ok(msg.to_string()),
        _ => Err(msg.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("bspx-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");
        let server = Server::bind(&path).unwrap();
        assert!(
            Server::bind(&path).is_err(),
            "second daemon must be refused"
        );

        let client_path = path.clone();
        let client = std::thread::spawn(move || {
            (
                send(&client_path, "focus left").unwrap(),
                send(&client_path, "bad").unwrap(),
            )
        });
        let mut seen = Vec::new();
        while seen.len() < 2 {
            server.poll(|cmd| {
                seen.push(cmd.to_string());
                if cmd == "bad" {
                    Err("nope".into())
                } else {
                    Ok("done".into())
                }
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        let (a, b) = client.join().unwrap();
        assert_eq!(a, Ok("done".into()));
        assert_eq!(b, Err("nope".into()));
        assert_eq!(seen, ["focus left", "bad"]);
        drop(server);
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
