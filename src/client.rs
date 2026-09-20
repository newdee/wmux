//! The wmux client: sends one command to the server and, if that attaches,
//! forwards console input and writes server output until detached.

use crate::console::{Console, InputEvent};
use crate::ipc::{ClientMsg, PROTOCOL_VERSION, ServerMsg, pipe_name, read_frame, write_frame};
use anyhow::{Context, Result, bail};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::mpsc;

const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_FILE_NOT_FOUND: i32 = 2;

async fn connect(pipe: &str, autostart: bool, socket: &str) -> Result<NamedPipeClient> {
    let mut started = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match ClientOptions::new().open(pipe) {
            Ok(c) => return Ok(c),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                if !autostart {
                    bail!("no server running on {pipe}");
                }
                if !started {
                    start_server(socket)?;
                    started = true;
                }
                if Instant::now() > deadline {
                    bail!("server did not start (see %LOCALAPPDATA%\\wmux\\server.log)");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e).context("open pipe"),
        }
    }
}

/// Start the server as a detached process that inherits *no* handles.
///
/// `std::process::Command` always passes `bInheritHandles = TRUE`, so a
/// server started from a client whose stdout is a pipe (e.g. `$x = wmux new
/// -d` in a script, or `Command::output()`) would hold that pipe open for
/// its whole life and the caller would never see EOF. Call CreateProcessW
/// directly instead.
fn start_server(socket: &str) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION,
        STARTUPINFOW,
    };
    let exe = std::env::current_exe().context("current_exe")?;
    let quote = |s: &str| -> String {
        if s.is_empty() || s.contains([' ', '\t', '"']) {
            format!("\"{}\"", s.replace('"', "\\\""))
        } else {
            s.to_string()
        }
    };
    let cmdline = format!("{} -L {} __server", quote(&exe.to_string_lossy()), quote(socket));
    let mut exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut cmd_w: Vec<u16> = std::ffi::OsStr::new(&cmdline).encode_wide().chain(std::iter::once(0)).collect();
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        CreateProcessW(
            exe_w.as_mut_ptr(),
            cmd_w.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0, // bInheritHandles = FALSE
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(), // inherit our environment block
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("spawn server");
    }
    unsafe {
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// Commands that must not start a server just to answer.
fn needs_server(argv: &[String]) -> bool {
    matches!(
        argv.first().map(String::as_str),
        Some(
            "new-session"
                | "new"
                | "attach-session"
                | "attach"
                | "a"
                | "at"
                | "source-file"
                | "source"
                | "bind-key"
                | "bind"
                | "set-option"
                | "set"
                | "restore-session"
                | "restore"
                | "resume"
                | "list-saved"
                | "saved"
                | "delete-saved"
                | "forget"
        )
    )
}

pub async fn run(socket: String, argv: Vec<String>) -> Result<i32> {
    let pipe = pipe_name(&socket);
    let mut console = Console::open().ok();
    let (cols, rows) = console.as_ref().map(|c| c.size()).unwrap_or((80, 24));
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let pane_env = std::env::var("WMUX_PANE").ok().and_then(|p| p.parse().ok());

    let conn = connect(&pipe, needs_server(&argv), &socket).await?;
    let (mut rd, mut wr) = tokio::io::split(conn);
    write_frame(
        &mut wr,
        &ClientMsg::Command {
            version: PROTOCOL_VERSION,
            argv,
            cwd,
            cols,
            rows,
            interactive: console.is_some(),
            pane_env,
        },
    )
    .await?;

    // Non-interactive phase.
    loop {
        let Some(msg) = read_frame::<_, ServerMsg>(&mut rd).await? else {
            bail!("server closed the connection");
        };
        match msg {
            ServerMsg::Text(t) => println!("{t}"),
            ServerMsg::Error(e) => eprintln!("{e}"),
            ServerMsg::Done { code } => return Ok(code),
            ServerMsg::Attached { session } => {
                let Some(mut c) = console.take() else { bail!("attached without a console") };
                c.enter_raw()?;
                c.set_title(&format!("wmux: {session}"));
                let c = Arc::new(c);
                // `rd` moves into the reader task and never comes back: this
                // arm always returns.
                let reason = attached(Arc::clone(&c), &session, rd, &mut wr).await;
                // The input thread still holds a reference; restore explicitly.
                c.restore();
                match reason {
                    Ok(r) => {
                        println!("[{r}]");
                        return Ok(0);
                    }
                    Err(e) => {
                        eprintln!("[{e:#}]");
                        return Ok(1);
                    }
                }
            }
            ServerMsg::Output(_) | ServerMsg::Detached { .. } | ServerMsg::SetMouse(_) => {}
        }
    }
}

/// Read frames on their own task and hand them over one at a time.
///
/// `read_frame` is two reads, the header and then the body, so awaiting it
/// directly in a `select!` loses the header whenever another branch wins the
/// race: the next read then takes four bytes out of the middle of a frame and
/// calls them a length. (That is where "frame too large: 538976288" came
/// from: 0x20202020, four spaces of a full-screen redraw.) A channel receive
/// is cancel-safe, so the reading happens where nothing can cancel it.
fn spawn_frame_reader<R>(mut rd: R) -> mpsc::Receiver<Result<Option<ServerMsg>>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    // Bounded: a client that cannot keep up should slow the pipe down rather
    // than grow without limit.
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        loop {
            let msg = read_frame::<_, ServerMsg>(&mut rd).await;
            let done = !matches!(msg, Ok(Some(_)));
            if tx.send(msg).await.is_err() || done {
                return;
            }
        }
    });
    rx
}

async fn attached<R, W>(console: Arc<Console>, session: &str, rd: R, wr: &mut W) -> Result<String>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    {
        let console = Arc::clone(&console);
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("console-input".into())
            .spawn(move || {
                let mut size = console.size();
                loop {
                    let events = match console.read_events() {
                        Ok(e) => e,
                        Err(_) => break,
                    };
                    for ev in events {
                        let m = match ev {
                            InputEvent::Key(k) => ClientMsg::Key(k),
                            InputEvent::Mouse(m) => ClientMsg::Mouse(m),
                            InputEvent::Resize => {
                                let s = console.size();
                                if s == size {
                                    continue;
                                }
                                size = s;
                                ClientMsg::Resize { cols: s.0, rows: s.1 }
                            }
                        };
                        if tx.send(m).is_err() {
                            return;
                        }
                    }
                }
            })
            .context("spawn input thread")?;
    }
    // The server sized the session from our Command; re-check in case the
    // window changed while connecting.
    let s = console.size();
    let _ = tx.send(ClientMsg::Resize { cols: s.0, rows: s.1 });

    let reason: Option<String>;
    // Poll the window size too: not every host reports WINDOW_BUFFER_SIZE_EVENT.
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    let mut last_size = s;
    let mut frames = spawn_frame_reader(rd);
    loop {
        tokio::select! {
            m = frames.recv() => {
                match m {
                    None | Some(Ok(None)) => { reason = Some("server exited".into()); break; }
                    Some(Err(e)) => return Err(e),
                    Some(Ok(Some(ServerMsg::Output(b)))) => console.write_bytes(&b),
                    Some(Ok(Some(ServerMsg::Detached { reason: r }))) => { reason = Some(format!("{r} (from session {session})")); break; }
                    Some(Ok(Some(ServerMsg::Error(e)))) => { reason = Some(format!("error: {e}")); break; }
                    Some(Ok(Some(ServerMsg::SetMouse(on)))) => console.set_mouse(on),
                    Some(Ok(Some(ServerMsg::Text(_) | ServerMsg::Done { .. } | ServerMsg::Attached { .. }))) => {}
                }
            }
            Some(m) = rx.recv() => {
                write_frame(wr, &m).await?;
            }
            _ = poll.tick() => {
                let s = console.size();
                if s != last_size {
                    last_size = s;
                    write_frame(wr, &ClientMsg::Resize { cols: s.0, rows: s.1 }).await?;
                }
            }
        }
    }
    // The input thread is blocked in ReadConsoleInput; it dies with the process.
    Ok(reason.unwrap_or_else(|| "detached".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// Frames must survive a `select!` loop that keeps waking up for other
    /// reasons. Reading them inline used to drop a half-read frame and read
    /// the next length out of the middle of a payload.
    #[tokio::test(flavor = "current_thread")]
    async fn frames_survive_a_select_loop() {
        let (mut server, client) = tokio::io::duplex(64 * 1024);
        // Send each frame in two writes with a gap between them, which is what
        // a big screen redraw looks like on a pipe.
        let sent: Vec<String> = (0..20).map(|i| format!("frame {i} {}", "x".repeat(2000))).collect();
        let expect = sent.clone();
        tokio::spawn(async move {
            for text in sent {
                let mut buf = Vec::new();
                let payload = bincode::serialize(&ServerMsg::Text(text)).unwrap();
                buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                // header first, then the body a moment later
                server.write_all(&buf).await.unwrap();
                server.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
                server.write_all(&payload).await.unwrap();
                server.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });

        let mut frames = spawn_frame_reader(client);
        // The branch that used to cancel the read, firing constantly.
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut got: Vec<String> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while got.len() < expect.len() && tokio::time::Instant::now() < deadline {
            tokio::select! {
                m = frames.recv() => match m {
                    Some(Ok(Some(ServerMsg::Text(t)))) => got.push(t),
                    Some(Ok(Some(other))) => panic!("unexpected message {other:?}"),
                    Some(Ok(None)) | None => break,
                    Some(Err(e)) => panic!("{e:#}"),
                },
                _ = ticker.tick() => {}
            }
        }
        assert_eq!(got, expect, "every frame arrives, in order and intact");
    }
}
