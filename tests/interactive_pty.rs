#![cfg(target_os = "macos")]

use next_terminal::pty::{Event, Session};
use std::time::{Duration, Instant};

fn wait_for(events: &async_channel::Receiver<Event>, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(Event::Output(bytes)) => {
                output.extend(bytes);
                if String::from_utf8_lossy(&output).contains(marker) {
                    return;
                }
            }
            Ok(Event::Failed(error)) => panic!("{error}"),
            Ok(Event::Exited(status)) => panic!("shell exited early: {status}"),
            Ok(_) => {}
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("{error}"),
        }
    }
    panic!(
        "timeout waiting for {marker}: {}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn real_shell_accepts_multiple_commands_and_exits() {
    let (session, events) = Session::start().unwrap();
    // 分开写参数，防止输入回显被误判为执行结果；同时确认 stdin/stdout 真正连接 TTY。
    session
        .input
        .write(b"test -t 0 && test -t 1 && printf '%s%s\\n' 'PTY_' 'FIRST_8324'\r")
        .unwrap();
    wait_for(&events, "PTY_FIRST_8324\r\n");
    session
        .input
        .write(b"printf '%s%s\\n' 'PTY_' 'SECOND_8324'\r")
        .unwrap();
    wait_for(&events, "PTY_SECOND_8324\r\n");
    session.input.write(b"exit\r").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(Event::Exited(status)) => {
                assert_eq!(status, "Success");
                return;
            }
            Ok(Event::Failed(error)) => panic!("{error}"),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("shell was not reaped after exit");
}

#[test]
fn resizing_updates_tty_size_and_notifies_the_shell() {
    use next_terminal::screen::GridSize;
    let (session, events) = Session::start_with_size(GridSize {
        columns: 40,
        rows: 8,
    })
    .unwrap();
    session.input.write(b"stty size\r").unwrap();
    wait_for(&events, "8 40\r\n");
    session.input.write(b"trap 'printf \"%s%s\\n\" \"WINCH_\" \"8324\"' WINCH; printf '%s%s\\n' 'SIZE_' 'READY_8324'\r").unwrap();
    wait_for(&events, "SIZE_READY_8324\r\n");

    session
        .input
        .resize(GridSize {
            columns: 63,
            rows: 17,
        })
        .unwrap();
    wait_for(&events, "WINCH_8324\r\n");
    session.input.write(b"stty size\r").unwrap();
    wait_for(&events, "17 63\r\n");

    session
        .input
        .resize(GridSize {
            columns: 28,
            rows: 9,
        })
        .unwrap();
    wait_for(&events, "WINCH_8324\r\n");
    session.input.write(b"stty size\r").unwrap();
    wait_for(&events, "9 28\r\n");
    session.input.write(b"exit\r").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(Event::Exited(status)) => {
                assert_eq!(status, "Success");
                return;
            }
            Ok(Event::Failed(error)) => panic!("{error}"),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("shell was not reaped after resize test");
}

#[test]
fn zsh_notifications_report_execution_without_authorizing_input() {
    use next_terminal::input_control::InputControl;
    use next_terminal::shell_observer::{ShellMark, ShellObserver};

    let (session, events) = Session::start().unwrap();
    let mut observer = ShellObserver::default();
    let mut control = InputControl::default();
    control.started();
    let mut marks = Vec::new();
    let mut output = Vec::new();
    let mut collect_until = |expected: &[ShellMark], text: &str| {
        let deadline = Instant::now() + Duration::from_secs(20);
        marks.clear();
        output.clear();
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(Event::Started { shell, .. }) => assert_eq!(shell, "/bin/zsh"),
                Ok(Event::ShellControl(_)) => {}
                Ok(Event::Output(bytes)) => {
                    observer.advance(&bytes, |mark| {
                        if matches!(
                            mark,
                            ShellMark::PromptStarted
                                | ShellMark::CommandStarted
                                | ShellMark::CommandFinished(_)
                        ) {
                            marks.push(mark);
                        }
                    });
                    output.extend(bytes);
                    if marks.ends_with(expected) && String::from_utf8_lossy(&output).contains(text)
                    {
                        assert_eq!(marks, expected);
                        assert!(!control.can_submit());
                        return;
                    }
                }
                Ok(Event::Failed(error)) => panic!("{error}"),
                Ok(Event::Exited(status)) => panic!("shell exited early: {status}"),
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        }
        panic!(
            "missing {expected:?}, got {marks:?}: {}",
            String::from_utf8_lossy(&output)
        );
    };

    let script = format!("{}/scripts/zsh-integration.zsh", env!("CARGO_MANIFEST_DIR"));
    // 不发送 source：第一个提示符必须由自动加载产生通知。
    collect_until(&[ShellMark::PromptStarted], "");
    // 重复安装仍只得到一次 C / D / A；两个参数防止把回显误判为结果。
    session
        .input
        .write(
            format!(
                "source '{}'; printf '%s%s\\n' 'HOOK_' 'OK_8324'\r",
                script.replace('\'', "'\\''")
            )
            .as_bytes(),
        )
        .unwrap();
    collect_until(
        &[
            ShellMark::CommandStarted,
            ShellMark::CommandFinished(Some(0)),
            ShellMark::PromptStarted,
        ],
        "HOOK_OK_8324\r\n",
    );
    session.input.write(b"false\r").unwrap();
    collect_until(
        &[
            ShellMark::CommandStarted,
            ShellMark::CommandFinished(Some(1)),
            ShellMark::PromptStarted,
        ],
        "",
    );
    // 无回显 read 期间只收到命令开始，输入临时字符串后才收到结束和主提示符。
    session
        .input
        .write(b"printf '%s%s\\n' 'READ_' 'WAIT_8324'; read -rs reply; [[ $reply == temporary ]]\r")
        .unwrap();
    collect_until(&[ShellMark::CommandStarted], "READ_WAIT_8324\r\n");
    session.input.write(b"temporary\r").unwrap();
    collect_until(
        &[
            ShellMark::CommandFinished(Some(0)),
            ShellMark::PromptStarted,
        ],
        "",
    );
    // preexec 在前台子进程启动之前发生；必须等子进程自己的标记再发送 SIGINT。
    session
        .input
        .write(b"sh -c 'printf \"%s%s\\n\" \"SLEEP_\" \"READY_8324\"; exec sleep 30'\r")
        .unwrap();
    collect_until(&[ShellMark::CommandStarted], "SLEEP_READY_8324\r\n");
    session.input.write(b"\x03").unwrap();
    collect_until(
        &[
            ShellMark::CommandFinished(Some(130)),
            ShellMark::PromptStarted,
        ],
        "",
    );
    session.shutdown.request();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(Event::Exited(_)) = events.try_recv() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("notification test shell did not shut down");
}

#[test]
fn zsh_distinguishes_primary_editing_continuations_and_foreground_input() {
    use next_terminal::shell_observer::{ShellObserver, ShellState};

    fn wait_state(
        events: &async_channel::Receiver<Event>,
        observer: &mut ShellObserver,
        expected: ShellState,
    ) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(Event::Output(bytes)) => {
                    observer.advance(&bytes, |_| {});
                    output.extend(bytes);
                    if observer.state() == expected {
                        return output;
                    }
                }
                Ok(Event::Failed(error)) => panic!("{error}"),
                Ok(Event::Exited(status)) => panic!("shell exited early: {status}"),
                Ok(_) => {}
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("{error}"),
            }
        }
        panic!(
            "expected {expected:?}, observed {:?}: {}",
            observer.state(),
            String::from_utf8_lossy(&output)
        );
    }

    let (session, events) = Session::start().unwrap();
    let mut observer = ShellObserver::default();
    let primary_empty = ShellState::Editing {
        continuation: false,
        empty: true,
    };
    let primary_text = ShellState::Editing {
        continuation: false,
        empty: false,
    };
    let continuation_empty = ShellState::Editing {
        continuation: true,
        empty: true,
    };
    let continuation_text = ShellState::Editing {
        continuation: true,
        empty: false,
    };
    wait_state(&events, &mut observer, primary_empty);
    session.input.write(b"x").unwrap();
    wait_state(&events, &mut observer, primary_text);
    session.input.write(b"\x7f").unwrap();
    wait_state(&events, &mut observer, primary_empty);
    // 空格也是未提交的输入，不能把 trim 后为空当成真正空缓冲区。
    session.input.write(b" ").unwrap();
    wait_state(&events, &mut observer, primary_text);
    session.input.write(b"\x15").unwrap();
    wait_state(&events, &mut observer, primary_empty);

    session.input.write(b"printf '%s\\n' 'FIRST\r").unwrap();
    wait_state(&events, &mut observer, continuation_empty);
    session.input.write(b"SECOND").unwrap();
    wait_state(&events, &mut observer, continuation_text);
    session.input.write(b"'\r").unwrap();
    let output = wait_state(&events, &mut observer, primary_empty);
    assert!(String::from_utf8_lossy(&output).contains("FIRST\r\nSECOND\r\n"));

    session.input.write(b"echo '\r").unwrap();
    wait_state(&events, &mut observer, continuation_empty);
    session.input.write(b"\x03").unwrap();
    wait_state(&events, &mut observer, primary_empty);
    session.input.write(b"\x1b[A").unwrap();
    wait_state(&events, &mut observer, primary_text);
    session.input.write(b"\x15").unwrap();
    wait_state(&events, &mut observer, primary_empty);

    session
        .input
        .write(b"\x1b[200~PASTE_PROBE\x1b[201~")
        .unwrap();
    wait_state(&events, &mut observer, primary_text);
    session.input.write(b"\x15").unwrap();
    wait_state(&events, &mut observer, primary_empty);

    session
        .input
        .write(b"read -rs reply; [[ $reply == temporary ]]\r")
        .unwrap();
    wait_state(&events, &mut observer, ShellState::Running);
    // 保持一段无输入等待，确认密码式 read 不会被报告为主提示符。
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        if let Ok(Event::Output(bytes)) = events.try_recv() {
            observer.advance(&bytes, |_| {});
            assert_eq!(observer.state(), ShellState::Running);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    session.input.write(b"temporary\r").unwrap();
    let output = wait_state(&events, &mut observer, primary_empty);
    assert!(!String::from_utf8_lossy(&output).contains("temporary"));
    session.input.write(b"vared reply\r").unwrap();
    wait_state(&events, &mut observer, ShellState::OtherEditor);
    session.input.write(b"\x03").unwrap();
    wait_state(&events, &mut observer, primary_empty);
    session.input.write(b"exit 0\r").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(Event::Exited(status)) => {
                assert_eq!(status, "Success");
                return;
            }
            Ok(Event::Failed(error)) => panic!("{error}"),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("editing-state test shell did not exit");
}

#[test]
fn independent_control_checks_do_not_enter_program_input_and_guard_submission() {
    use next_terminal::shell_control::{ControlEvent, ReadyRequest};
    use next_terminal::shell_observer::ShellState;

    fn wait_control(events: &async_channel::Receiver<Event>, expected: ControlEvent) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(Event::ShellControl(event)) => {
                    if event == expected {
                        return;
                    }
                    seen.push(event);
                }
                Ok(Event::Failed(error)) => panic!("{error}"),
                Ok(Event::Exited(status)) => panic!("shell exited early: {status}"),
                Ok(_) => {}
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("{error}"),
            }
        }
        panic!("expected control {expected:?}, saw {seen:?}");
    }
    fn check(session: &Session, events: &async_channel::Receiver<Event>, id: u64, ready: bool) {
        let generation = session.input.generation();
        assert!(
            session
                .input
                .check_ready(ReadyRequest {
                    request: id,
                    generation
                })
                .unwrap()
        );
        wait_control(
            events,
            ControlEvent::Checked {
                request: id,
                generation,
                ready,
            },
        );
    }
    let empty = ControlEvent::State(ShellState::Editing {
        continuation: false,
        empty: true,
    });
    let (session, events) = Session::start().unwrap();
    wait_control(&events, ControlEvent::Connected);
    wait_control(&events, empty);
    check(&session, &events, 1, true);
    let generation = session.input.generation();
    assert!(
        session
            .input
            .submit(
                Some(generation),
                next_terminal::screen::GridSize::default(),
                b"printf '%s%s\\n' 'CONTROL_' 'ROUNDTRIP_8324'\r"
            )
            .unwrap()
    );
    assert!(
        !session
            .input
            .submit(
                Some(generation),
                next_terminal::screen::GridSize::default(),
                b"SHOULD_NOT_EXECUTE\r"
            )
            .unwrap()
    );
    wait_for(&events, "CONTROL_ROUNDTRIP_8324\r\n");
    check(&session, &events, 2, true);

    let generation = session.input.generation();
    let size = next_terminal::screen::GridSize {
        columns: 63,
        rows: 17,
    };
    assert!(
        session
            .input
            .submit(Some(generation), size, b"stty size\r")
            .unwrap()
    );
    assert!(
        !session
            .input
            .submit(Some(generation), size, b"SHOULD_NOT_EXECUTE\r")
            .unwrap()
    );
    wait_for(&events, "17 63\r\n");
    check(&session, &events, 20, true);

    session.input.write(b"x").unwrap();
    wait_control(
        &events,
        ControlEvent::State(ShellState::Editing {
            continuation: false,
            empty: false,
        }),
    );
    check(&session, &events, 3, false);
    session.input.write(b"\x15echo '\r").unwrap();
    wait_control(
        &events,
        ControlEvent::State(ShellState::Editing {
            continuation: true,
            empty: true,
        }),
    );
    check(&session, &events, 4, false);
    session.input.write(b"\x03").unwrap();
    wait_control(&events, empty);
    check(&session, &events, 5, true);

    // 前台程序伪造 OSC 空主提示符也不会发出真正的控制消息；CHECK 在独立 socket 超时。
    session.input.write(b"printf '\\033]777;next-terminal;zle;1;start;1\\007'; read -rs reply; [[ $reply == temporary ]] && printf '%s%s\\n' 'PASSWORD_' 'CLEAN_8324'\r").unwrap();
    wait_control(&events, ControlEvent::State(ShellState::Running));
    check(&session, &events, 6, false);
    session.input.write(b"temporary\r").unwrap();
    wait_for(&events, "PASSWORD_CLEAN_8324\r\n");
    check(&session, &events, 7, true);

    session.input.write(b"vim -Nu NONE -i NONE -n\r").unwrap();
    wait_control(&events, ControlEvent::State(ShellState::Running));
    check(&session, &events, 8, false);
    session.input.write(b":q!\r").unwrap();
    wait_control(&events, empty);
    check(&session, &events, 9, true);
    session.input.write(b"exit 0\r").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(Event::Exited(status)) => {
                assert_eq!(status, "Success");
                return;
            }
            Ok(Event::Failed(error)) => panic!("{error}"),
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("control test shell was not reaped");
}
