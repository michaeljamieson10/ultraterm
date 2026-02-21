use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl PtyHandle {
    pub fn write_all(&self, data: &[u8]) -> Result<()> {
        self.writer
            .lock()
            .write_all(data)
            .context("failed to write to PTY")
    }

    pub fn resize(&self, cols: usize, rows: usize) -> Result<()> {
        self.master
            .resize(PtySize {
                rows: rows.min(u16::MAX as usize) as u16,
                cols: cols.min(u16::MAX as usize) as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize PTY")
    }

    pub fn try_wait(&mut self) -> Result<Option<portable_pty::ExitStatus>> {
        self.child.try_wait().context("failed to wait on shell")
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[derive(Clone, Debug)]
pub struct SpawnConfig {
    pub cols: usize,
    pub rows: usize,
    pub term: String,
    pub truecolor: bool,
    pub stress_mode: bool,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 40,
            term: "xterm-256color".to_string(),
            truecolor: true,
            stress_mode: false,
        }
    }
}

pub fn spawn_pty(config: &SpawnConfig) -> Result<(PtyHandle, Receiver<Vec<u8>>)> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: config.rows.min(u16::MAX as usize) as u16,
            cols: config.cols.min(u16::MAX as usize) as u16,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("unable to open PTY")?;

    let mut cmd = if config.stress_mode {
        let mut builder = CommandBuilder::new("/bin/sh");
        builder.arg("-lc");
        builder.arg("yes 'ultraterm stress output 0123456789 abcdefghijklmnopqrstuvwxyz'");
        builder
    } else {
        let shell = choose_shell();
        let mut builder = CommandBuilder::new(shell);
        builder.arg("-l");
        builder
    };

    cmd.env("TERM", &config.term);
    if config.truecolor {
        cmd.env("COLORTERM", "truecolor");
    }
    cmd.env("PROMPT_EOL_MARK", "");

    if let Ok(home) = env::var("HOME") {
        cmd.cwd(home);
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .context("failed to spawn shell command")?;

    let mut reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    let writer = pair
        .master
        .take_writer()
        .context("failed to get PTY writer")?;

    let writer = Arc::new(Mutex::new(writer));
    let (tx, rx) = unbounded::<Vec<u8>>();

    thread::Builder::new()
        .name("pty-reader".to_string())
        .spawn(move || {
            let mut buf = vec![0_u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let kind = err.kind();
                        if kind == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                }
            }
        })
        .context("failed to spawn PTY reader thread")?;

    let handle = PtyHandle {
        master: pair.master,
        writer,
        child,
    };

    Ok((handle, rx))
}

fn choose_shell() -> String {
    if Path::new("/bin/zsh").exists() {
        return "/bin/zsh".to_string();
    }

    if let Ok(shell) = env::var("SHELL") {
        if !shell.trim().is_empty() && fs::metadata(&shell).is_ok() {
            return shell;
        }
    }

    "/bin/sh".to_string()
}
