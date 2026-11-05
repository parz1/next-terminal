// 当前只采集一次 ls → exit；不是持续运行的会话管理器。
use crate::screen;

pub fn capture_ls() -> anyhow::Result<Vec<u8>> {
    use anyhow::{Context, bail, ensure};
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_secs(20);
    const MAX_OUTPUT: u64 = 1024 * 1024;

    // master 是应用端，slave 是 shell 端。尺寸以字符的行、列表示。
    let pair = native_pty_system().openpty(PtySize {
        rows: screen::ROWS,
        cols: screen::COLUMNS,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    let mut command = CommandBuilder::new_default_prog();
    // portable-pty 默认优先使用 SHELL 环境变量；这里改为查询账户默认 shell。
    // macOS 的查询封装在依赖中，不在项目中复制 passwd/unsafe 代码。
    command.env_remove("SHELL");
    command.cwd(std::env::current_dir()?);
    // 本例没有终端解析器，不宣称支持颜色、光标查询等高级能力。
    command.env("TERM", "dumb");
    println!("Default shell: {} (login shell)", command.get_shell());
    let mut child = pair.slave.spawn_command(command)?;
    println!(
        "Shell PID: {:?}; PTY size: {} columns x {} rows",
        child.process_id(),
        screen::COLUMNS,
        screen::ROWS
    );
    // 父进程不能继续持有 slave，否则 shell 退出后 reader 可能收不到 EOF。
    drop(pair.slave);

    let (tx, rx) = mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // 同时读取，避免等待子进程时 PTY 输出缓冲区塞满。
        // 多读一个字节，以便区分正常 EOF 和超过本例的输出上限。
        let result = reader.take(MAX_OUTPUT + 1).read_to_end(&mut bytes);
        let _ = tx.send(result.map(|_| bytes));
    });

    // spawn 后的错误统一走下面的清理分支，不能直接用 ? 提前遗留 shell。
    let result = (|| -> anyhow::Result<Vec<u8>> {
        let deadline = Instant::now() + TIMEOUT;
        println!("Sending: ls + Enter, then exit + Enter");
        // 不用 shell -c：命令由应用写入 PTY，交给交互式 shell 读取和解释。
        // 两行相当于粘贴两条命令；exit 是本次有限验证的收尾。
        writer.write_all(b"ls\rexit\r")?;
        writer.flush()?;

        let bytes = rx
            .recv_timeout(TIMEOUT)
            .context("等待 PTY 输出结束超时或读取线程断开")??;
        ensure!(
            bytes.len() <= MAX_OUTPUT as usize,
            "PTY 输出超过 1 MiB 上限"
        );
        // EOF 和子进程退出是两件事，单独确认并回收退出状态。
        loop {
            if let Some(status) = child.try_wait()? {
                println!("--- Shell exit: {status} (reaped) ---");
                ensure!(status.success(), "shell 以非零状态退出");
                return Ok(bytes);
            }
            if Instant::now() >= deadline {
                bail!("等待 shell 退出超时");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();

    if result.is_err() {
        // Unix 下 portable-pty 先发 SIGHUP，必要时再强制终止直接子进程。
        // 这不是完整进程树管理；本例只运行短命令 ls。
        child.kill().context("验证失败后终止 shell 失败")?;
        child.wait().context("回收 shell 失败")?;
    }
    drop(writer);
    drop(pair.master);
    // 成功时输出已收齐，线程会立即结束；超时路径不无期限等待后台读取。
    if result.is_ok() {
        reader_thread
            .join()
            .map_err(|_| anyhow::anyhow!("PTY 读取线程异常退出"))?;
    }
    result
}

// 单窗口的持续 PTY；阻塞读写和进程回收均在后台线程。
use crate::shell_control::{ControlEvent, ReadyRequest};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

pub enum Event {
    Started { shell: String, pid: Option<u32> },
    Output(Vec<u8>),
    ShellControl(ControlEvent),
    Exited(String),
    Failed(String),
}

enum Command {
    Write(Vec<u8>),
    Submit {
        bytes: Vec<u8>,
        size: screen::GridSize,
    },
    Resize(screen::GridSize),
    Check(ReadyRequest),
    Shutdown,
}

#[derive(Clone)]
pub struct Input(Arc<Mutex<InputQueue>>);

struct InputQueue {
    sender: Sender<Command>,
    generation: u64,
}

impl Input {
    pub fn write(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut queue = self.0.lock().unwrap();
        queue.sender.send(Command::Write(bytes.to_vec()))?;
        queue.generation += 1;
        Ok(())
    }

    pub fn generation(&self) -> u64 {
        self.0.lock().unwrap().generation
    }

    /// 收起命令框后的 PTY 尺寸必须先于命令生效；检查代次和入队在同一把锁内。
    pub fn submit(
        &self,
        expected: Option<u64>,
        size: screen::GridSize,
        bytes: &[u8],
    ) -> anyhow::Result<bool> {
        let mut queue = self.0.lock().unwrap();
        if expected.is_some_and(|generation| generation != queue.generation) {
            return Ok(false);
        }
        queue.sender.send(Command::Submit {
            bytes: bytes.to_vec(),
            size,
        })?;
        queue.generation += 1;
        Ok(true)
    }

    pub fn check_ready(&self, request: ReadyRequest) -> anyhow::Result<bool> {
        let queue = self.0.lock().unwrap();
        if queue.generation != request.generation {
            return Ok(false);
        }
        queue.sender.send(Command::Check(request))?;
        Ok(true)
    }

    pub fn resize(&self, size: screen::GridSize) -> anyhow::Result<()> {
        self.0.lock().unwrap().sender.send(Command::Resize(size))?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct Shutdown {
    input: Input,
    done: async_channel::Receiver<()>,
}

impl Shutdown {
    pub fn request(&self) {
        let _ = self.input.0.lock().unwrap().sender.send(Command::Shutdown);
    }

    pub async fn wait(&self) {
        // 后台线程结束时 sender 被释放，所有等待者都会被唤醒。
        let _ = self.done.recv().await;
    }
}

pub struct Session {
    pub input: Input,
    pub shutdown: Shutdown,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown.request();
    }
}

impl Session {
    pub fn start() -> anyhow::Result<(Self, async_channel::Receiver<Event>)> {
        Self::start_with_size(screen::GridSize::default())
    }

    pub fn start_with_size(
        size: screen::GridSize,
    ) -> anyhow::Result<(Self, async_channel::Receiver<Event>)> {
        let (commands, command_rx) = mpsc::channel();
        let (events, event_rx) = async_channel::bounded(64);
        let (done, done_rx) = async_channel::bounded::<()>(1);
        std::thread::Builder::new()
            .name("pty-session".into())
            .spawn(move || {
                if let Err(error) = run_session(command_rx, &events, size) {
                    let _ = events.send_blocking(Event::Failed(format!("{error:#}")));
                }
                drop(done);
            })?;
        let input = Input(Arc::new(Mutex::new(InputQueue {
            sender: commands,
            generation: 0,
        })));
        let shutdown = Shutdown {
            input: input.clone(),
            done: done_rx,
        };
        Ok((Self { input, shutdown }, event_rx))
    }
}

fn run_session(
    commands: mpsc::Receiver<Command>,
    events: &async_channel::Sender<Event>,
    size: screen::GridSize,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    use std::time::Duration;

    let pair = native_pty_system().openpty(PtySize {
        rows: size.rows,
        cols: size.columns,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let mut command = CommandBuilder::new_default_prog();
    command.env_remove("SHELL");
    command.cwd(std::env::current_dir()?);
    command.env("TERM", "xterm-256color");
    let shell = command.get_shell();
    // 生命周期覆盖 shell 启动与退出；错误路径也由 TempDir 自动清理。
    let mut startup = match crate::zsh_startup::configure(&mut command) {
        Ok(startup) => startup,
        Err(error) => {
            eprintln!("zsh 集成不可用，保持普通终端：{error:#}");
            None
        }
    };
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    eprintln!("PTY started: {shell}, pid={:?}", child.process_id());
    let _ = events.try_send(Event::Started {
        shell,
        pid: child.process_id(),
    });

    let output = events.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if output
                        .send_blocking(Event::Output(buffer[..count].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = output.send_blocking(Event::Failed(format!("PTY 读取失败：{error}")));
                    break;
                }
            }
        }
    });

    let io_result = (|| -> anyhow::Result<()> {
        loop {
            match commands.recv_timeout(Duration::from_millis(10)) {
                Ok(Command::Write(bytes)) => {
                    writer.write_all(&bytes)?;
                    writer.flush()?;
                }
                Ok(Command::Submit { bytes, size }) => {
                    pair.master.resize(PtySize {
                        rows: size.rows,
                        cols: size.columns,
                        pixel_width: 0,
                        pixel_height: 0,
                    })?;
                    writer.write_all(&bytes)?;
                    writer.flush()?;
                }
                Ok(Command::Resize(size)) => {
                    pair.master.resize(PtySize {
                        rows: size.rows,
                        cols: size.columns,
                        pixel_width: 0,
                        pixel_height: 0,
                    })?;
                }
                Ok(Command::Check(request)) => {
                    if !startup
                        .as_mut()
                        .is_some_and(|startup| startup.control.check(request))
                    {
                        let _ = events.send_blocking(Event::ShellControl(ControlEvent::Checked {
                            request: request.request,
                            generation: request.generation,
                            ready: false,
                        }));
                    }
                }
                Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(startup) = &mut startup {
                for event in startup.control.poll() {
                    if events.send_blocking(Event::ShellControl(event)).is_err() {
                        break;
                    }
                }
            }
            if child.try_wait()?.is_some() {
                break;
            }
        }
        Ok(())
    })();
    // 无论写入失败、用户关闭窗口还是 shell 自行退出，都走相同回收路径。
    if child.try_wait()?.is_none() {
        child.kill().context("终止 shell 失败")?;
    }
    let status = child.wait().context("回收 shell 失败")?;
    eprintln!("PTY exited: {status} (reaped)");
    drop(writer);
    drop(pair.master);
    let _ = events.send_blocking(Event::Exited(status.to_string()));
    // 不因任意继承了 slave 的后台进程而无限等待。
    if reader_thread.is_finished() {
        let _ = reader_thread.join();
    }
    io_result
}
