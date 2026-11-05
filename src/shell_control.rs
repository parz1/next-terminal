//! 独立于 PTY 文本的会话控制通道。不是同一用户进程之间的安全沙箱。
use crate::shell_observer::ShellState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlEvent {
    Connected,
    State(ShellState),
    Checked {
        request: u64,
        generation: u64,
        ready: bool,
    },
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadyRequest {
    pub request: u64,
    pub generation: u64,
}

#[cfg(target_os = "macos")]
pub(crate) mod transport {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::{Duration, Instant};

    pub struct Server {
        listener: UnixListener,
        token: String,
        stream: Option<UnixStream>,
        authenticated: bool,
        stopped: bool,
        buffer: Vec<u8>,
        pending: Option<(ReadyRequest, Instant)>,
    }

    impl Server {
        pub fn bind(path: &Path) -> anyhow::Result<Self> {
            let listener = UnixListener::bind(path)?;
            listener.set_nonblocking(true)?;
            let mut bytes = [0u8; 32];
            std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
            let token = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            Ok(Self {
                listener,
                token,
                stream: None,
                authenticated: false,
                stopped: false,
                buffer: Vec::new(),
                pending: None,
            })
        }

        pub fn token(&self) -> &str {
            &self.token
        }

        pub fn check(&mut self, request: ReadyRequest) -> bool {
            if !self.authenticated {
                return false;
            }
            let line = format!("CHECK {} {}\n", request.request, request.generation);
            if self
                .stream
                .as_mut()
                .is_none_or(|stream| stream.write_all(line.as_bytes()).is_err())
            {
                self.stopped = true;
                return false;
            }
            self.pending = Some((request, Instant::now()));
            true
        }

        pub fn poll(&mut self) -> Vec<ControlEvent> {
            let mut events = Vec::new();
            if !self.stopped && self.stream.is_none() {
                if let Ok((stream, _)) = self.listener.accept() {
                    if stream.set_nonblocking(true).is_ok() {
                        self.stream = Some(stream);
                    }
                }
            }
            if !self.stopped {
                let mut bytes = [0u8; 2048];
                if let Some(stream) = self.stream.as_mut() {
                    // 限制每次读取量，持续控制输出不能饿死 PTY 命令队列。
                    for _ in 0..4 {
                        match stream.read(&mut bytes) {
                            Ok(0) => {
                                self.stopped = true;
                                break;
                            }
                            Ok(count) => self.buffer.extend_from_slice(&bytes[..count]),
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                continue;
                            }
                            Err(_) => {
                                self.stopped = true;
                                break;
                            }
                        }
                    }
                }
                while let Some(end) = self.buffer.iter().position(|&byte| byte == b'\n') {
                    let line: Vec<_> = self.buffer.drain(..=end).collect();
                    if line.len() > 256 {
                        self.stopped = true;
                        break;
                    }
                    let Ok(line) = std::str::from_utf8(&line) else {
                        self.stopped = true;
                        break;
                    };
                    if !self.authenticated {
                        if line.trim_end() == format!("HELLO {}", self.token) {
                            self.authenticated = true;
                            events.push(ControlEvent::Connected);
                        } else {
                            self.stopped = true;
                            break;
                        }
                    } else if let Some(event) = parse_line(line) {
                        if let ControlEvent::Checked {
                            request,
                            generation,
                            ..
                        } = event
                        {
                            if self.pending.is_some_and(|(pending, _)| {
                                pending
                                    == ReadyRequest {
                                        request,
                                        generation,
                                    }
                            }) {
                                self.pending = None;
                            }
                        }
                        events.push(event);
                    } else {
                        self.stopped = true;
                        break;
                    }
                }
                if self.buffer.len() > 256 {
                    self.stopped = true;
                }
            }
            if self
                .pending
                .is_some_and(|(_, since)| since.elapsed() >= Duration::from_millis(700))
            {
                let (pending, _) = self.pending.take().unwrap();
                events.push(ControlEvent::Checked {
                    request: pending.request,
                    generation: pending.generation,
                    ready: false,
                });
            }
            if self.stopped {
                if self.stream.take().is_some() {
                    events.push(ControlEvent::Disconnected);
                }
                self.pending = None;
                self.authenticated = false;
            }
            events
        }
    }

    fn parse_line(line: &str) -> Option<ControlEvent> {
        let parts: Vec<_> = line.split_whitespace().collect();
        let state = match parts.as_slice() {
            ["STATE", "primary-empty"] => ShellState::Editing {
                continuation: false,
                empty: true,
            },
            ["STATE", "primary-text"] => ShellState::Editing {
                continuation: false,
                empty: false,
            },
            ["STATE", "continuation-empty"] => ShellState::Editing {
                continuation: true,
                empty: true,
            },
            ["STATE", "continuation-text"] => ShellState::Editing {
                continuation: true,
                empty: false,
            },
            ["STATE", "other"] => ShellState::OtherEditor,
            ["STATE", "inactive"] => ShellState::EditorInactive,
            ["STATE", "running"] => ShellState::Running,
            ["STATE", "prompt"] => ShellState::PromptDrawing,
            ["READY", request, generation, ready @ ("0" | "1")] => {
                return Some(ControlEvent::Checked {
                    request: request.parse().ok()?,
                    generation: generation.parse().ok()?,
                    ready: *ready == "1",
                });
            }
            _ => return None,
        };
        Some(ControlEvent::State(state))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn handshake_is_required_and_split_lines_are_buffered() {
            let dir = tempfile::tempdir_in("/tmp").unwrap();
            let path = dir.path().join("s");
            let mut server = Server::bind(&path).unwrap();
            let mut client = UnixStream::connect(&path).unwrap();
            client
                .write_all(format!("HELLO {}\nSTATE primary-", server.token()).as_bytes())
                .unwrap();
            assert_eq!(server.poll(), [ControlEvent::Connected]);
            client.write_all(b"empty\n").unwrap();
            assert_eq!(
                server.poll(),
                [ControlEvent::State(ShellState::Editing {
                    continuation: false,
                    empty: true
                })]
            );
            drop(client);
            assert_eq!(server.poll(), [ControlEvent::Disconnected]);
        }
        #[test]
        fn incorrect_handshake_cannot_publish_state() {
            let dir = tempfile::tempdir_in("/tmp").unwrap();
            let path = dir.path().join("s");
            let mut server = Server::bind(&path).unwrap();
            let mut client = UnixStream::connect(&path).unwrap();
            client
                .write_all(b"HELLO wrong\nSTATE primary-empty\n")
                .unwrap();
            assert_eq!(server.poll(), [ControlEvent::Disconnected]);
            assert!(!server.check(ReadyRequest {
                request: 1,
                generation: 0
            }));
        }
    }
}
