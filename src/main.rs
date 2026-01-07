use std::{
    io::{self, Read, Write},
    sync::mpsc,
    thread,
};

use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

fn main() -> Result<()> {
    // 1. 获取当前终端大小，以便 PTY 匹配
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    // 2. 创建 PTY 系统 (使用 native 实现)
    let pty_system = NativePtySystem::default();

    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // 3. 启动 Shell
    // 在 Windows 上我们一般用 cmd 或 powershell，在 Unix 上用 SHELL 环境变量或 bash
    #[cfg(target_os = "windows")]
    let cmd = CommandBuilder::new("cmd");
    #[cfg(not(target_os = "windows"))]
    let cmd = {
        let shell = std::env::var("SHELL").unwrap_or("bash".into());
        CommandBuilder::new(shell)
    };

    // 在 slave 端启动进程
    let mut child = pair.slave.spawn_command(cmd)?;

    // 父进程中不需要 slave 了，释放掉
    drop(pair.slave);

    // 获取 master 端的读写器
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // 4. 开启 Raw Mode (关键：让宿主终端直接把按键传给我们，不进行行缓冲)
    enable_raw_mode()?;

    println!("Interactive shell started. Type 'exit' to quit.\r");

    // 使用 channel 来通知主线程退出
    let (tx, rx) = mpsc::channel();

    // 5. 输出线程: PTY Master -> Host Stdout
    let tx_out = tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut stdout = io::stdout();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    // 直接透传数据到 stdout
                    if stdout.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = stdout.flush();
                }
                Err(_) => break,
            }
        }
        let _ = tx_out.send(());
    });

    // 6. 输入线程: Host Stdin -> PTY Master
    // 注意：在 Raw Mode 下，input 也是逐字节读取的
    let tx_in = tx.clone();
    thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx_in.send(());
    });

    // 等待任意一方结束 (通常是 shell 退出导致 reader EOF)
    let _ = rx.recv();

    // 7. 恢复终端模式
    disable_raw_mode()?;

    // 等待子进程彻底退出
    let _ = child.wait();

    Ok(())
}
