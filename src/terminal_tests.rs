use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::parser::AnsiParser;
use crate::pty::{spawn_pty, PtyHandle, SpawnConfig};
use crate::screen::Screen;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

struct TerminalHarness {
    pty: PtyHandle,
    rx: Receiver<Vec<u8>>,
    parser: AnsiParser,
    screen: Screen,
    transcript: Vec<u8>,
}

impl TerminalHarness {
    fn new() -> Self {
        let config = SpawnConfig {
            cols: 120,
            rows: 40,
            ..SpawnConfig::default()
        };

        let (pty, rx) = spawn_pty(&config).expect("failed to spawn PTY for test");
        Self {
            pty,
            rx,
            parser: AnsiParser::new(),
            screen: Screen::new(config.cols, config.rows, 1_000),
            transcript: Vec::new(),
        }
    }

    fn write(&self, bytes: &[u8]) {
        self.pty
            .write_all(bytes)
            .expect("failed to write test bytes into PTY");
    }

    fn wait_for_contains(&mut self, needle: &str, timeout: Duration) -> bool {
        let needle = needle.as_bytes();
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(Duration::from_millis(200));

            match self.rx.recv_timeout(wait) {
                Ok(chunk) => {
                    self.transcript.extend_from_slice(&chunk);
                    let responses = self.parser.process(&chunk, &mut self.screen);
                    for response in responses {
                        let _ = self.pty.write_all(&response);
                    }

                    if self
                        .transcript
                        .windows(needle.len())
                        .any(|window| window == needle)
                    {
                        return true;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        false
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

#[test]
fn typed_command_executes_and_renders_output() {
    let mut term = TerminalHarness::new();
    term.write(b"printf 'ULTRATERM_TYPED_OK\\n'\n");

    let saw_output = term.wait_for_contains("ULTRATERM_TYPED_OK", DEFAULT_TIMEOUT);
    assert!(
        saw_output,
        "did not observe typed command output in PTY transcript:\n{}",
        term.transcript_tail(4_096),
    );
    assert!(
        term.screen_contains("ULTRATERM_TYPED_OK"),
        "parser/screen did not render typed command output.\nScreen:\n{}",
        term.screen_dump(),
    );
}

#[test]
fn backspace_editing_produces_expected_command() {
    let mut term = TerminalHarness::new();
    term.write(b"echo ULTRATERM_BACKSPACX\x7fE_OK\n");

    assert!(
        term.wait_for_contains("ULTRATERM_BACKSPACE_OK", DEFAULT_TIMEOUT),
        "backspace-edited command output not found.\nTranscript tail:\n{}",
        term.transcript_tail(4_096),
    );
}

#[test]
fn ctrl_c_interrupts_running_command_and_returns_to_prompt() {
    let mut term = TerminalHarness::new();
    term.write(b"sleep 10\n");

    thread::sleep(Duration::from_millis(700));
    term.write(&[0x03]);
    term.write(b"printf 'ULTRATERM_CTRL_C_OK\\n'\n");

    assert!(
        term.wait_for_contains("ULTRATERM_CTRL_C_OK", Duration::from_secs(6)),
        "ctrl-c did not interrupt promptly (or terminal did not recover).\nTranscript tail:\n{}",
        term.transcript_tail(4_096),
    );
}
