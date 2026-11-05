//! 用会话私有的 .zshenv 加载集成；不写入用户 dotfiles。
use crate::shell_control::transport::Server;
use portable_pty::CommandBuilder;
use std::path::Path;
use tempfile::TempDir;

pub(crate) struct Startup {
    _directory: TempDir,
    pub control: Server,
}

impl Startup {
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        self._directory.path()
    }
}

pub(crate) fn configure(command: &mut CommandBuilder) -> anyhow::Result<Option<Startup>> {
    configure_in(command, Path::new("/tmp"))
}

fn configure_in(command: &mut CommandBuilder, root: &Path) -> anyhow::Result<Option<Startup>> {
    if Path::new(&command.get_shell())
        .file_name()
        .is_none_or(|name| name != "zsh")
    {
        return Ok(None);
    }
    let directory = tempfile::Builder::new()
        .prefix("next-terminal-zsh-")
        .tempdir_in(root)?;
    let socket_path = directory.path().join("control.sock");
    let control = Server::bind(&socket_path)?;
    let integration = directory.path().join("integration.zsh");
    std::fs::write(&integration, include_str!("../scripts/zsh-integration.zsh"))?;
    let original = command.get_env("ZDOTDIR").map(|value| value.to_owned());
    // 路径是 shell 字面量，不能直接拼入未转义的 source 命令。
    let integration = integration
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("临时集成路径不是 UTF-8"))?;
    let source = format!(
        r#"# Generated for this session only.
if [[ $NEXT_TERMINAL_ORIGINAL_ZDOTDIR_SET == 1 ]]; then
    export ZDOTDIR="$NEXT_TERMINAL_ORIGINAL_ZDOTDIR"
else
    unset ZDOTDIR
fi
unset NEXT_TERMINAL_ORIGINAL_ZDOTDIR_SET NEXT_TERMINAL_ORIGINAL_ZDOTDIR
if [[ -r ${{ZDOTDIR-$HOME}}/.zshenv ]]; then
    builtin source "${{ZDOTDIR-$HOME}}/.zshenv"
fi
if [[ -o interactive && -o rcs ]]; then
    function _next_terminal_bootstrap_precmd() {{
        autoload -Uz add-zsh-hook
        add-zsh-hook -d precmd _next_terminal_bootstrap_precmd
        typeset -g _next_terminal_control_path='{control_path}'
        typeset -g _next_terminal_control_token='{control_token}'
        builtin source '{}'
        _next_terminal_precmd
        unfunction _next_terminal_bootstrap_precmd
    }}
    autoload -Uz add-zsh-hook
    add-zsh-hook precmd _next_terminal_bootstrap_precmd
fi
"#,
        integration.replace('\'', "'\\''"),
        control_path = socket_path.display(),
        control_token = control.token(),
    );
    std::fs::write(directory.path().join(".zshenv"), source)?;
    // 文件和 socket 全部准备好后才提交环境修改，失败时可以原样启动普通 shell。
    command.env(
        "NEXT_TERMINAL_ORIGINAL_ZDOTDIR_SET",
        if original.is_some() { "1" } else { "0" },
    );
    command.env(
        "NEXT_TERMINAL_ORIGINAL_ZDOTDIR",
        original.unwrap_or_default(),
    );
    command.env("ZDOTDIR", directory.path());
    Ok(Some(Startup {
        _directory: directory,
        control,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_failure_leaves_the_original_shell_environment_intact() {
        let root = tempfile::tempdir().unwrap();
        let mut command = CommandBuilder::new("/bin/zsh");
        command.env("SHELL", "/bin/zsh");
        command.env("ZDOTDIR", "/original");
        assert!(configure_in(&mut command, &root.path().join("missing")).is_err());
        assert_eq!(
            command.get_env("ZDOTDIR"),
            Some(std::ffi::OsStr::new("/original"))
        );
        assert!(
            command
                .get_env("NEXT_TERMINAL_ORIGINAL_ZDOTDIR_SET")
                .is_none()
        );
    }

    #[test]
    fn startup_keeps_user_file_order_and_zdotdir_changes() {
        let fixture = tempfile::tempdir().unwrap();
        let original = fixture.path().join("original ' config");
        let redirected = fixture.path().join("redirected ' config");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&redirected).unwrap();
        let quoted = redirected.to_str().unwrap().replace('\'', "'\\''");
        std::fs::write(
            original.join(".zshenv"),
            format!("STARTUP_TRACE=E\nexport ZDOTDIR='{quoted}'\n"),
        )
        .unwrap();
        for (name, letter) in [(".zprofile", "P"), (".zshrc", "R"), (".zlogin", "L")] {
            std::fs::write(redirected.join(name), format!("STARTUP_TRACE+={letter}\n")).unwrap();
        }
        std::fs::write(redirected.join(".zlogout"), "print -r -- USER_LOGOUT\n").unwrap();
        let mut builder = CommandBuilder::new("/bin/zsh");
        builder.env("SHELL", "/bin/zsh");
        builder.env("ZDOTDIR", &original);
        let bootstrap = configure(&mut builder).unwrap().unwrap();
        let bootstrap_path = bootstrap.path().to_owned();
        let output = std::process::Command::new("/bin/zsh")
            .args(["-lic", "print -r -- TRACE=$STARTUP_TRACE; print -r -- DIR=$ZDOTDIR; print -r -- FLAGS=$options[login],$options[interactive]; print -r -- PRIVATE=${+NEXT_TERMINAL_ORIGINAL_ZDOTDIR_SET}"])
            .env_clear().envs(builder.iter_full_env_as_str())
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("TRACE=EPRL\n"), "{text}");
        assert!(
            text.contains(&format!("DIR={}\n", redirected.display())),
            "{text}"
        );
        assert!(text.contains("FLAGS=on,on\n"), "{text}");
        assert!(text.contains("PRIVATE=0\n"), "{text}");
        assert!(text.contains("USER_LOGOUT\n"), "{text}");
        drop(bootstrap);
        assert!(!bootstrap_path.exists());
    }

    #[test]
    fn unset_zdotdir_and_disabled_rcs_are_preserved() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(
            fixture.path().join(".zshenv"),
            "ENV_LOADED=yes\nunsetopt rcs\n",
        )
        .unwrap();
        std::fs::write(fixture.path().join(".zshrc"), "print SHOULD_NOT_LOAD\n").unwrap();
        let mut builder = CommandBuilder::new("/bin/zsh");
        builder.env("SHELL", "/bin/zsh");
        builder.env("HOME", fixture.path());
        builder.env_remove("ZDOTDIR");
        let _bootstrap = configure(&mut builder).unwrap().unwrap();
        let output = std::process::Command::new("/bin/zsh")
            .args([
                "-lic",
                "print -r -- ENV=$ENV_LOADED,ZDOTDIR_SET=${+ZDOTDIR},RCS=$options[rcs]",
            ])
            .env_clear()
            .envs(builder.iter_full_env_as_str())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "ENV=yes,ZDOTDIR_SET=0,RCS=off\n"
        );
    }

    #[test]
    fn other_shells_are_left_alone() {
        let mut builder = CommandBuilder::new("/bin/bash");
        builder.env("SHELL", "/bin/bash");
        builder.env("ZDOTDIR", "/original");
        assert!(configure(&mut builder).unwrap().is_none());
        assert_eq!(
            builder.get_env("ZDOTDIR"),
            Some(std::ffi::OsStr::new("/original"))
        );
        assert!(
            builder
                .get_env("NEXT_TERMINAL_ORIGINAL_ZDOTDIR_SET")
                .is_none()
        );
    }
}
