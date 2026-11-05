pub mod input_control;
pub mod screen;
pub mod shell_control;
pub mod shell_observer;

// 平台边界：当前只支持 macOS 的 PTY 启动、读写与回收。
#[cfg(target_os = "macos")]
pub mod pty;

#[cfg(target_os = "macos")]
mod zsh_startup;
