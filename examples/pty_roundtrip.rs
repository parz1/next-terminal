// 保留第 2、3 步的命令行入口，与窗口复用同一条 PTY 和解析链路。
#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    let bytes = next_terminal::pty::capture_ls()?;
    println!("--- PTY output (control characters escaped) ---");
    for line in String::from_utf8_lossy(&bytes).split_inclusive('\n') {
        println!("{}", line.strip_suffix('\n').unwrap_or(line).escape_debug());
    }
    let terminal = next_terminal::screen::parse(&bytes);
    next_terminal::screen::print_snapshot(&terminal);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("这个 PTY 验证步骤目前只在 macOS 启用")
}
