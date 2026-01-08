use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::Result;
use dashmap::DashMap;
use interactive_shell::{AgentRequest, AgentResponse};
use once_cell::sync::Lazy;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use uuid::Uuid;

// Global Session Map: SessionID -> Session
// Using DashMap for concurrent access
static SESSIONS: Lazy<DashMap<String, Session>> = Lazy::new(|| DashMap::new());

struct Session {
    writer: Mutex<Box<dyn Write + Send>>, // Must be Sync to be in DashMap, so wrap in Mutex
    // In this simple design, we buffer output in a shared queue
    // ExecCommand will drain this queue.
    output_queue: Arc<Mutex<Vec<u8>>>,
    // To kill the session
    kill_tx: mpsc::Sender<()>,
}

fn main() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:1337")?;
    println!("Agent listening on 127.0.0.1:1337 with SESSION support...");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut stream) {
                        eprintln!("Error handling client: {:?}", e);
                    }
                });
            }
            Err(e) => eprintln!("Connection failed: {:?}", e),
        }
    }
    Ok(())
}

fn handle_client(stream: &mut std::net::TcpStream) -> Result<()> {
    // 1. Read Request
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    if line.trim().is_empty() {
        return Ok(());
    }

    // Try to parse as new AgentRequest
    let req: AgentRequest = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            // Send error response
            let resp = AgentResponse {
                success: false,
                session_id: None,
                output: "".to_string(),
                exit_code: None,
                error: Some(format!("Invalid Request: {}", e)),
            };
            stream.write_all(serde_json::to_string(&resp)?.as_bytes())?;
            stream.write_all(b"\n")?;
            return Ok(());
        }
    };

    println!("Received: {:?}", req);

    let response = process_request(req);

    // 3. Send Response
    let resp_json = serde_json::to_string(&response)?;
    stream.write_all(resp_json.as_bytes())?;
    stream.write_all(b"\n")?;

    Ok(())
}

fn process_request(req: AgentRequest) -> AgentResponse {
    match req {
        AgentRequest::StartSession { user } => start_session(user),
        AgentRequest::ExecCommand {
            session_id,
            command,
            timeout_ms,
        } => exec_command(session_id, command, timeout_ms),
        AgentRequest::CloseSession { session_id } => close_session(session_id),
    }
}

fn start_session(user: Option<String>) -> AgentResponse {
    let session_id = Uuid::new_v4().to_string();

    match spawn_shell(user.as_deref()) {
        Ok((writer, output_queue, kill_tx)) => {
            let session = Session {
                writer: Mutex::new(writer),
                output_queue,
                kill_tx,
            };
            SESSIONS.insert(session_id.clone(), session);

            AgentResponse {
                success: true,
                session_id: Some(session_id),
                output: "Session started".to_string(),
                exit_code: None,
                error: None,
            }
        }
        Err(e) => AgentResponse {
            success: false,
            session_id: None,
            output: "".to_string(),
            exit_code: None,
            error: Some(format!("Failed to spawn shell: {}", e)),
        },
    }
}

fn exec_command(session_id: String, command: String, timeout_ms: u64) -> AgentResponse {
    // Get session
    // Note: DashMap's get returns a Ref which locks the entry.
    // We need to write to the writer and read from buffer.

    // Scoped block to handle locking if needed, but DashMap allows concurrent access pretty well.
    // However, if we want to write to the session, we need mutable access to writer?
    // Box<dyn Write> implies &mut self usually for write_all.
    // We wrapped Session in DashMap. We might need interior mutability for writer if we strictly follow Rust rules,
    // or DashMap::get_mut.

    // BUT: process_request is called per connection.
    // This is a blocking operation (wait for output).
    // If we hold DashMap RefMut for 5 seconds, no other command can run on THIS session? That's fine.

    if let Some(session) = SESSIONS.get_mut(&session_id) {
        // 1. Clear previous output from buffer (optional, maybe we want to keep history? No, drain it)
        {
            let mut q = session.output_queue.lock().unwrap();
            q.clear(); // Discard old junk
        }

        // 2. Write Command
        let mut cmd_with_newline = command.clone();
        if !cmd_with_newline.ends_with('\n') {
            cmd_with_newline.push('\n');
        }

        // Scope for the writer lock
        {
            let mut writer = session.writer.lock().unwrap();
            if let Err(e) = writer.write_all(cmd_with_newline.as_bytes()) {
                return AgentResponse {
                    success: false,
                    session_id: Some(session_id),
                    output: "".to_string(),
                    exit_code: None,
                    error: Some(format!("Failed to write to pty: {}", e)),
                };
            }
            let _ = writer.flush();
        }

        // 3. Wait/Poll for output
        // We implemented a simple polling mechanism:
        // Wait for some data to appear, then wait until "silence" for X ms?
        // This is heuristic-based because we don't know when command ends.

        // Simple logic:
        // Read buffer for `timeout_ms`. If we have data, good.
        // But `timeout_ms` is usually "max time to run".
        // With an interactive shell, we usually want to read UNTIL we stop seeing new data for a bit.

        let start = std::time::Instant::now();
        let timeout = Duration::from_millis(timeout_ms);

        // Initial sleep to give shell time to react (very naive)
        thread::sleep(Duration::from_millis(100));

        // In a clearer protocol, we'd use a delimiter/prompt matching.
        // For this demo, we just drain whatever comes in `timeout_ms`.

        // Actually, let's just sleep for 0.5s or so (or less if timeout is small) and return what we have?
        // Or wait loop.

        let mut captured_bytes = Vec::new();

        // We will loop until timeout, collecting data.
        // But if command finishes early (e.g. echo hi), waiting 5s is annoying.
        // We need a "silence detection".

        let silence_threshold = Duration::from_millis(300); // If no data for 300ms, assume done
        let mut last_data_time = std::time::Instant::now();
        let mut has_data = false;

        loop {
            if start.elapsed() > timeout {
                break; // Hard timeout
            }

            let mut q = session.output_queue.lock().unwrap();
            if !q.is_empty() {
                captured_bytes.extend(q.drain(..));
                last_data_time = std::time::Instant::now();
                has_data = true;
            } else {
                // Buffer empty
                if has_data && last_data_time.elapsed() > silence_threshold {
                    // We had some data, and now silence. Assume done.
                    break;
                }
            }
            drop(q); // Release lock

            thread::sleep(Duration::from_millis(50));
        }

        let clean_bytes = strip_ansi_escapes::strip(&captured_bytes);
        let mut output = String::from_utf8_lossy(&clean_bytes).to_string();

        // Attempt to remove the echoed command from the output to keep it clean.
        let trimmed_cmd = command.trim();
        // Check if output starts with the command (ignoring initial whitespace/newlines in output)
        if let Some(idx) = output.find(trimmed_cmd) {
            // Only strip if it's near the start (e.g. within first 100 chars)
            if idx < 100 {
                let end_of_cmd = idx + trimmed_cmd.len();
                // Skip the command and any immediate following newlines
                let remaining = &output[end_of_cmd..];
                let clean_output = remaining.trim_start_matches(|c| c == '\r' || c == '\n');
                output = clean_output.to_string();
            }
        }

        AgentResponse {
            success: true,
            session_id: Some(session_id.clone()),
            output,
            exit_code: None, // We don't know the status code of the command inside the shell easily
            error: None,
        }
    } else {
        AgentResponse {
            success: false,
            session_id: Some(session_id),
            output: "".to_string(),
            exit_code: None,
            error: Some("Session not found".to_string()),
        }
    }
}

fn close_session(session_id: String) -> AgentResponse {
    if let Some((_, session)) = SESSIONS.remove(&session_id) {
        let _ = session.kill_tx.send(());
        AgentResponse {
            success: true,
            session_id: Some(session_id),
            output: "Session closed".to_string(),
            exit_code: None,
            error: None,
        }
    } else {
        AgentResponse {
            success: false,
            session_id: Some(session_id),
            output: "".to_string(),
            exit_code: None,
            error: Some("Session not found".to_string()),
        }
    }
}

fn spawn_shell(
    user: Option<&str>,
) -> Result<(Box<dyn Write + Send>, Arc<Mutex<Vec<u8>>>, mpsc::Sender<()>)> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 200,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Prepare Command
    #[cfg(target_os = "windows")]
    let cmd = {
        let mut cmd = CommandBuilder::new("cmd");
        if let Some(u) = user {
            eprintln!(
                "Warning: User switching to '{}' not supported on Windows Agent (yet). Running as current user.",
                u
            );
        }
        cmd
    };

    #[cfg(not(target_os = "windows"))]
    let cmd = {
        if let Some(u) = user {
            // Check if we are stuck on password prompt?
            // We can't easily check BEFORE spawn.
            // We could use `sudo -n` for non-interactive check, but we are using `su`.
            // Let's assume root.
            let mut cmd = CommandBuilder::new("su");
            cmd.args(&["-", u]); // Just login as user

            // NOTE: Running `su` requires a TTY. We are in a PTY, so it works.
            cmd
        } else {
            CommandBuilder::new("bash")
        }
    };

    // cmd.env("TERM", "dumb"); // Can't set env on moved value without builder pattern chain or logic above.
    // Handled by re-declaring if needed, but let's assume default is fine or re-structure.
    let mut cmd = cmd;
    cmd.env("TERM", "dumb");
    // Set a very simple prompt to make parsing easier (and cleaner)
    // PS1 is the primary prompt string variable in bash/sh.
    // Setting it to empty string or a known token helps avoid garbage.
    cmd.env("PS1", ""); 

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    let output_queue = Arc::new(Mutex::new(Vec::new()));
    let (kill_tx, kill_rx) = mpsc::channel();

    let q_clone = output_queue.clone();

    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            if kill_rx.try_recv().is_ok() {
                let _ = child.kill();
                break;
            }

            match child.try_wait() {
                Ok(Some(_)) => break, // Exited
                _ => {}
            }

            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Password prompt detection or other logic could go here
                    let mut q = q_clone.lock().unwrap();
                    q.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        let _ = child.wait();
    });

    Ok((writer, output_queue, kill_tx))
}
