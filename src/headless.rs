use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use log::info;

use crate::parser::AnsiParser;
use crate::pty::{spawn_pty, PtyHandle, SpawnConfig};
use crate::screen::Screen;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run_self_test() -> Result<()> {
    info!("running headless self-test");

    let scenarios: [(&str, fn() -> Result<()>); 3] = [
        ("typed command output", scenario_typed_command_output),
        ("backspace editing", scenario_backspace_editing),
        ("ctrl-c interrupt", scenario_ctrl_c_interrupt),
    ];

    let mut passed = 0usize;
    for (name, scenario) in scenarios {
        match scenario() {
            Ok(()) => {
                println!("PASS: {name}");
                passed += 1;
            }
            Err(err) => {
                eprintln!("FAIL: {name}\n{err:#}");
            }
        }
    }

    println!(
        "headless self-test summary: {}/{} passed",
        passed,
        scenarios.len()
    );

    if passed != scenarios.len() {
        bail!("headless self-test failed");
    }

    Ok(())
}

struct HeadlessHarness {
    pty: PtyHandle,
    rx: Receiver<Vec<u8>>,
    parser: AnsiParser,
    screen: Screen,
    transcript: Vec<u8>,
}

impl HeadlessHarness {
    fn new() -> Result<Self> {
        let config = SpawnConfig {
            cols: 120,
            rows: 40,
            ..SpawnConfig::default()
        };

        let (pty, rx) = spawn_pty(&config).context("failed to spawn PTY")?;
        Ok(Self {
            pty,
            rx,
            parser: AnsiParser::new(),
            screen: Screen::new(config.cols, config.rows, 1_000),
            transcript: Vec::new(),
        })
    }

    fn write(&self, bytes: &[u8]) -> Result<()> {
        self.pty
            .write_all(bytes)
            .context("failed to write bytes to PTY")
    }

    fn wait_for_contains(&mut self, needle: &str, timeout: Duration) -> Result<()> {
        let needle_bytes = needle.as_bytes();
        if needle_bytes.is_empty() {
            return Ok(());
        }

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(Duration::from_millis(250));

            match self.rx.recv_timeout(wait) {
                Ok(chunk) => {
                    self.transcript.extend_from_slice(&chunk);
                    let responses = self.parser.process(&chunk, &mut self.screen);
                    for response in responses {
                        self.pty
                            .write_all(&response)
                            .context("failed to send parser response to PTY")?;
                    }

                    if self
                        .transcript
                        .windows(needle_bytes.len())
                        .any(|window| window == needle_bytes)
                    {
                        return Ok(());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        bail!(
            "timed out waiting for marker {:?}\ntranscript tail:\n{}",
            needle,
            self.transcript_tail(4_096),
        )
    }

    fn screen_contains(&self, needle: &str) -> bool {
        self.screen_dump().contains(needle)
    }

    fn screen_dump(&self) -> String {
        let mut out = String::new();
        for row in 0..self.screen.rows() {
            let mut line = String::new();
            for cell in self.screen.line(row) {
                line.push(cell.ch);
            }
            out.push_str(line.trim_end_matches(' '));
            out.push('\n');
        }
        out
    }

    fn transcript_tail(&self, limit: usize) -> String {
        let start = self.transcript.len().saturating_sub(limit);
        String::from_utf8_lossy(&self.transcript[start..]).into_owned()
    }
}

fn scenario_typed_command_output() -> Result<()> {
    let mut harness = HeadlessHarness::new()?;
    harness.write(b"printf 'ULTRATERM_HEADLESS_TYPED_OK\\n'\n")?;
    harness.wait_for_contains("ULTRATERM_HEADLESS_TYPED_OK", DEFAULT_TIMEOUT)?;

    if !harness.screen_contains("ULTRATERM_HEADLESS_TYPED_OK") {
        bail!(
            "output was received but not rendered in screen model\n{}",
            harness.screen_dump()
        );
    }

    Ok(())
}

fn scenario_backspace_editing() -> Result<()> {
    let mut harness = HeadlessHarness::new()?;
    harness.write(b"echo ULTRATERM_HEADLESS_BACKSPACX\x7fE_OK\n")?;
    harness.wait_for_contains("ULTRATERM_HEADLESS_BACKSPACE_OK", DEFAULT_TIMEOUT)?;
    Ok(())
}

fn scenario_ctrl_c_interrupt() -> Result<()> {
    let mut harness = HeadlessHarness::new()?;
    harness.write(b"sleep 10\n")?;
    thread::sleep(Duration::from_millis(700));
    harness.write(&[0x03])?;
    harness.write(b"printf 'ULTRATERM_HEADLESS_CTRL_C_OK\\n'\n")?;
    harness.wait_for_contains("ULTRATERM_HEADLESS_CTRL_C_OK", Duration::from_secs(6))?;
    Ok(())
}
