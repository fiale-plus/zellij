use super::*;
use super::select_best_candidates;
use zellij_utils::input::command::RunCommand;

fn make_server() -> ServerOsInputOutput {
    get_server_os_input().expect("failed to create server os input")
}

// --- Cross-platform command helpers ---

#[allow(dead_code)]
#[cfg(not(windows))]
fn long_running_cmd() -> Command {
    let mut cmd = Command::new("sleep");
    cmd.arg("60");
    cmd
}

#[allow(dead_code)]
#[cfg(windows)]
fn long_running_cmd() -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("timeout");
    cmd.args(&["/T", "60"]);
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    cmd
}

#[allow(dead_code)]
#[cfg(not(windows))]
fn echo_cmd(msg: &str) -> Command {
    let mut cmd = Command::new("echo");
    cmd.arg(msg);
    cmd
}

#[allow(dead_code)]
#[cfg(windows)]
fn echo_cmd(msg: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("cmd");
    cmd.args(&["/C", "echo", msg]);
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    cmd
}

#[allow(dead_code)]
#[cfg(not(windows))]
fn stdin_reader_cmd() -> Command {
    let mut cmd = Command::new("cat");
    cmd.stdin(std::process::Stdio::piped());
    cmd
}

#[allow(dead_code)]
#[cfg(windows)]
fn stdin_reader_cmd() -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("findstr");
    cmd.arg("/R").arg(".*");
    cmd.stdin(std::process::Stdio::piped());
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    cmd
}

#[test]
fn get_cwd() {
    let server = make_server();

    let pid = std::process::id();
    assert!(
        server.get_cwd(pid).is_some(),
        "Get current working directory from PID {}",
        pid
    );
}

// --- Signal delivery tests ---

#[cfg(not(windows))]
#[test]
fn kill_sends_sighup_to_process() {
    let child = long_running_cmd()
        .spawn()
        .expect("failed to spawn long-running process");
    let pid = child.id();

    let server = make_server();

    server.kill(pid).expect("kill should succeed");

    // Give the signal time to be delivered
    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[cfg(not(windows))]
#[test]
fn force_kill_sends_sigkill_to_process() {
    let child = long_running_cmd()
        .spawn()
        .expect("failed to spawn long-running process");
    let pid = child.id();

    let server = make_server();

    server.force_kill(pid).expect("force_kill should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[cfg(not(windows))]
#[test]
fn send_sigint_to_process() {
    let child = stdin_reader_cmd()
        .spawn()
        .expect("failed to spawn stdin-reader process");
    let pid = child.id();

    let server = make_server();

    server.send_sigint(pid).expect("send_sigint should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[test]
fn spawn_and_read_output() {
    use crate::panes::PaneId;
    use zellij_utils::input::command::TerminalAction;

    let server = make_server();
    let test_message = "hello_zellij_test";

    #[cfg(not(windows))]
    let cmd = RunCommand {
        command: PathBuf::from("echo"),
        args: vec![test_message.to_string()],
        ..Default::default()
    };
    #[cfg(windows)]
    let cmd = RunCommand {
        command: PathBuf::from("cmd"),
        args: vec![
            "/K".to_string(),
            "echo".to_string(),
            test_message.to_string(),
        ],
        ..Default::default()
    };

    let action = TerminalAction::RunCommand(cmd);
    let quit_cb: Box<dyn Fn(PaneId, Option<i32>, RunCommand) + Send> =
        Box::new(|_pane_id, _exit_status, _run_command| {});

    let (_terminal_id, mut reader, _child_pid) = server
        .spawn_terminal(action, quit_cb, None)
        .expect("spawn_terminal should succeed");

    // Read output from the spawned terminal
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        loop {
            if std::time::Instant::now() > deadline {
                break;
            }
            match tokio::time::timeout(std::time::Duration::from_millis(500), reader.read(&mut buf))
                .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    output.extend_from_slice(&buf[..n]);
                    let s = String::from_utf8_lossy(&output);
                    if s.contains(test_message) {
                        break;
                    }
                },
                Ok(Err(_)) => break,
                Err(_) => {
                    // timeout — check if we already have enough
                    let s = String::from_utf8_lossy(&output);
                    if s.contains(test_message) {
                        break;
                    }
                },
            }
        }
    });

    let output_str = String::from_utf8_lossy(&output);
    assert!(
        output_str.contains(test_message),
        "expected output to contain '{}', got: '{}'",
        test_message,
        output_str
    );
}

// --- select_best_candidates tests ---

fn candidates_from(entries: Vec<(&str, Vec<(bool, Vec<&str>)>)>) -> HashMap<String, Vec<(bool, Vec<String>)>> {
    entries
        .into_iter()
        .map(|(ppid, children)| {
            let children = children
                .into_iter()
                .map(|(fg, args)| (fg, args.into_iter().map(String::from).collect()))
                .collect();
            (ppid.to_string(), children)
        })
        .collect()
}

#[test]
fn foreground_process_preferred_over_background_children() {
    let candidates = candidates_from(vec![
        ("1234", vec![
            (true, vec!["claude", "--resume", "my-session"]),
            (false, vec!["node", "/path/to/mcp-server"]),
        ]),
    ]);
    let cmds = select_best_candidates(candidates);
    assert_eq!(cmds.get("1234").unwrap(), &["claude", "--resume", "my-session"]);
}

#[test]
fn background_process_listed_first_does_not_win() {
    let candidates = candidates_from(vec![
        ("1234", vec![
            (false, vec!["node", "/path/to/mcp-server-1"]),
            (false, vec!["node", "/path/to/mcp-server-2"]),
            (true, vec!["claude", "--resume", "my-session"]),
        ]),
    ]);
    let cmds = select_best_candidates(candidates);
    assert_eq!(cmds.get("1234").unwrap(), &["claude", "--resume", "my-session"]);
}

#[test]
fn single_child_returned_regardless_of_foreground() {
    let candidates = candidates_from(vec![
        ("5678", vec![(false, vec!["nvim", "main.rs"])]),
    ]);
    let cmds = select_best_candidates(candidates);
    assert_eq!(cmds.get("5678").unwrap(), &["nvim", "main.rs"]);
}

#[test]
fn multiple_ppids_handled_independently() {
    let candidates = candidates_from(vec![
        ("100", vec![
            (false, vec!["node", "mcp-server"]),
            (true, vec!["claude", "--resume", "foo"]),
        ]),
        ("200", vec![
            (true, vec!["nvim", "bar.rs"]),
            (false, vec!["node", "lsp-server"]),
        ]),
    ]);
    let cmds = select_best_candidates(candidates);
    assert_eq!(cmds.get("100").unwrap(), &["claude", "--resume", "foo"]);
    assert_eq!(cmds.get("200").unwrap(), &["nvim", "bar.rs"]);
}

#[test]
fn no_foreground_falls_back_to_first_child() {
    let candidates = candidates_from(vec![
        ("300", vec![
            (false, vec!["node", "server-a"]),
            (false, vec!["node", "server-b"]),
        ]),
    ]);
    let cmds = select_best_candidates(candidates);
    assert_eq!(cmds.get("300").unwrap(), &["node", "server-a"]);
}
