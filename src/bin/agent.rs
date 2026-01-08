use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use interactive_shell::{CommandRequest, CommandResponse};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

fn main() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:1337")?;
    println!("Agent listening on 127.0.0.1:1337...");

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
    reader.read_line(&mut line)?; // Expect JSON on one line

    if line.trim().is_empty() {
        return Ok(());
    }

    let req: CommandRequest = serde_json::from_str(&line)?;
    println!("Received task: {:?}", req);

    // 2. Execute Command
    let response = execute_command(req)?;

    // 3. Send Response
    let resp_json = serde_json::to_string(&response)?;
    stream.write_all(resp_json.as_bytes())?;
    stream.write_all(b"\n")?;

    println!("Task finished, response sent.");
    Ok(())
}

fn execute_command(req: CommandRequest) -> Result<CommandResponse> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 200,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Wrap command in shell to properly execute command line strings
    #[cfg(target_os = "windows")]
    let mut cmd = CommandBuilder::new("cmd");
    #[cfg(target_os = "windows")]
    cmd.args(&["/C", &req.command]);

    #[cfg(not(target_os = "windows"))]
    let mut cmd = CommandBuilder::new("bash");
    #[cfg(not(target_os = "windows"))]
    cmd.args(&["-c", &req.command]);

    // Avoid ANSI codes if possible (though strip-ansi-escapes is better)
    cmd.env("TERM", "dumb");

    let mut child = pair.slave.spawn_command(cmd)?;

    // Close slave in parent
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;

    // We don't need to write to stdin for this use case, so we can drop writer or ignore it
    // drop(pair.master.take_writer()?);

    let (tx, rx) = mpsc::channel();

    // Output reading thread
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut output_acc = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    output_acc.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(output_acc);
        // Pty reader closes when process exits (usually)
    });

    let start = Instant::now();
    let timeout = Duration::from_millis(req.timeout_ms);
    let mut timed_out = false;
    let mut exit_code = None;

    // Polling for exit or timeout
    loop {
        if start.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
            break;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = Some(status.exit_code() as i32);
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => {
                // Process likely gone or error
                break;
            }
        }
    }

    // Wait for reader thread to finish collecting output
    // (if child exited, reader should hit EOF soon)
    let raw_output = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();

    // Clean output
    let clean_bytes = strip_ansi_escapes::strip(&raw_output);
    let output_str = String::from_utf8_lossy(&clean_bytes).to_string();

    Ok(CommandResponse {
        timed_out,
        exit_code,
        output: output_str,
    })
}
