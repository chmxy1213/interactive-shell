use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
};

use anyhow::Result;
use clap::Parser;
use interactive_shell::{CommandRequest, CommandResponse};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Command to execute on the agent
    #[arg(short, long)]
    command: String,

    /// Timeout in milliseconds
    #[arg(short, long, default_value_t = 5000)]
    timeout: u64,

    /// Agent address
    #[arg(short, long, default_value = "127.0.0.1:1337")]
    addr: String,

    /// Run as specific user (Linux/macOS only)
    #[arg(short, long)]
    user: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("Connecting to {}...", args.addr);
    let mut stream = TcpStream::connect(&args.addr)?;

    let req = CommandRequest {
        command: args.command.clone(),
        timeout_ms: args.timeout,
        run_as_user: args.user,
    };

    let json_req = serde_json::to_string(&req)?;
    stream.write_all(json_req.as_bytes())?;
    stream.write_all(b"\n")?;

    println!("Sent command: {}", args.command);

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    if line.trim().is_empty() {
        println!("Error: Empty response from agent");
        return Ok(());
    }

    let resp: CommandResponse = serde_json::from_str(&line)?;

    println!("--- Execution Result ---");
    println!("Timed out: {}", resp.timed_out);
    println!("Exit code: {:?}", resp.exit_code);
    println!("Output received ({} bytes):", resp.output.len());
    println!("------------------------");
    println!("{}", resp.output);

    Ok(())
}
