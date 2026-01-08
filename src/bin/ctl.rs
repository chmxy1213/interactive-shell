use std::{
    io::{self, BufRead, BufReader, Write},
    net::TcpStream,
};

use anyhow::Result;
use clap::Parser;
use interactive_shell::{AgentRequest, AgentResponse};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Agent address
    #[arg(short, long, default_value = "127.0.0.1:1337")]
    addr: String,

    /// Initial user to switch to (e.g. root, secvision)
    #[arg(short, long)]
    user: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("Connecting to Agent at {}...", args.addr);

    // 1. Start Session
    let session_id = start_session(&args.addr, args.user.clone())?;
    println!("Session started. ID: {}", session_id);
    println!("Type 'exit' to close session. Type commands to execute.");

    // 2. REPL Loop
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut input = String::new();

    loop {
        print!("> ");
        io::stdout().flush()?;

        input.clear();
        if handle.read_line(&mut input)? == 0 {
            break; // EOF
        }

        let cmd = input.trim();
        if cmd == "exit" {
            break;
        }
        if cmd.is_empty() {
            continue;
        }

        if let Err(e) = exec_command(&args.addr, &session_id, cmd) {
            eprintln!("Error executing command: {}", e);
            // Maybe session is dead?
        }
    }

    // 3. Close Session
    close_session(&args.addr, &session_id)?;

    Ok(())
}

fn send_request(addr: &str, req: AgentRequest) -> Result<AgentResponse> {
    let mut stream = TcpStream::connect(addr)?;
    let json = serde_json::to_string(&req)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let resp: AgentResponse = serde_json::from_str(&line)?;
    Ok(resp)
}

fn start_session(addr: &str, user: Option<String>) -> Result<String> {
    let req = AgentRequest::StartSession { user };
    let resp = send_request(addr, req)?;
    if !resp.success {
        return Err(anyhow::anyhow!("Start failed: {:?}", resp.error));
    }
    resp.session_id
        .ok_or_else(|| anyhow::anyhow!("No session ID returned"))
}

fn exec_command(addr: &str, session_id: &str, cmd: &str) -> Result<()> {
    let req = AgentRequest::ExecCommand {
        session_id: session_id.to_string(),
        command: cmd.to_string(),
        timeout_ms: 3000, // Default 3s waiting for output per chunk
    };
    let resp = send_request(addr, req)?;

    if !resp.success {
        eprintln!("Remote Error: {:?}", resp.error);
    }

    // Print output directly
    if !resp.output.is_empty() {
        print!("{}", resp.output);
    }

    Ok(())
}

fn close_session(addr: &str, session_id: &str) -> Result<()> {
    let req = AgentRequest::CloseSession {
        session_id: session_id.to_string(),
    };
    let _ = send_request(addr, req)?;
    println!("Session closed.");
    Ok(())
}
