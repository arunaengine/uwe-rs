//! Redraws live progress below captured stdout and stderr lines.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::File;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::progress::Board;

/// How long the end of a run waits for the last lines of the pipes.
const DRAIN: Duration = Duration::from_secs(2);

/// Whether standard output is a terminal that can draw in place.
pub fn possible() -> bool {
    std::io::stdout().is_terminal() && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

/// The drawing, while it lasts.
pub struct Live {
    shared: Arc<Shared>,
    /// The terminal of standard output and of standard error, to give them back at the end.
    out: OwnedFd,
    err: Option<OwnedFd>,
    /// One message per reader of a pipe when it has read all.
    drained: Receiver<()>,
    readers: usize,
    ticker: Option<JoinHandle<()>>,
    restored: bool,
}

/// What the readers, the ticker and the run share.
struct Shared {
    board: Arc<Mutex<Board>>,
    screen: Mutex<Screen>,
    stop: AtomicBool,
    color: bool,
}

/// The terminal and how many lines of steps are on it below the other output.
struct Screen {
    terminal: File,
    drawn: usize,
}

impl Live {
    /// Starts drawing, or `None` when standard output is no terminal or a pipe cannot be made.
    pub fn start(board: Arc<Mutex<Board>>) -> Option<Live> {
        if !possible() {
            return None;
        }
        let out = std::io::stdout().as_fd().try_clone_to_owned().ok()?;
        let terminal = File::from(out.try_clone().ok()?);
        let shared = Arc::new(Shared {
            board,
            screen: Mutex::new(Screen { terminal, drawn: 0 }),
            stop: AtomicBool::new(false),
            color: std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
        });
        let (sender, drained) = channel();
        let (read_out, write_out) = rustix::pipe::pipe().ok()?;
        rustix::stdio::dup2_stdout(&write_out).ok()?;
        let mut live = Live {
            shared: shared.clone(),
            out,
            err: None,
            drained,
            readers: 0,
            ticker: None,
            restored: false,
        };
        live.read(read_out, sender.clone());
        if std::io::stderr().is_terminal()
            && let Ok(err) = std::io::stderr().as_fd().try_clone_to_owned()
            && let Ok((read_err, write_err)) = rustix::pipe::pipe()
            && rustix::stdio::dup2_stderr(&write_err).is_ok()
        {
            live.err = Some(err);
            live.read(read_err, sender);
        }
        live.ticker = std::thread::Builder::new()
            .name("progress".into())
            .spawn(move || {
                while !shared.stop.load(Ordering::Relaxed) {
                    std::thread::park_timeout(Duration::from_secs(1));
                    shared.redraw();
                }
            })
            .ok();
        Some(live)
    }

    /// Prints the lines of a pipe above the steps, until its last writer closes it.
    fn read(&mut self, pipe: OwnedFd, drained: std::sync::mpsc::Sender<()>) {
        let shared = self.shared.clone();
        let started = std::thread::Builder::new()
            .name("output".into())
            .spawn(move || {
                let mut reader = BufReader::new(File::from(pipe));
                let mut line = Vec::new();
                while reader
                    .read_until(b'\n', &mut line)
                    .is_ok_and(|read| read > 0)
                {
                    let text = String::from_utf8_lossy(&line);
                    shared.print(text.trim_end_matches(['\n', '\r']));
                    line.clear();
                }
                let _ = drained.send(());
            });
        if started.is_ok() {
            self.readers += 1;
        }
    }

    /// Prints a line above the steps.
    pub fn print(&self, text: &str) {
        self.shared.print(text);
    }

    /// Ends the drawing: standard output and error go to the terminal again, the last lines of
    /// the pipes are printed, and the steps are drawn a last time and stay.
    pub fn finish(mut self) {
        self.restore();
        for _ in 0..self.readers {
            if self.drained.recv_timeout(DRAIN).is_err() {
                break;
            }
        }
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            ticker.thread().unpark();
            let _ = ticker.join();
        }
        self.shared.redraw();
        let mut screen = (self.shared.screen.lock()).unwrap_or_else(PoisonError::into_inner);
        screen.drawn = 0;
    }

    /// Gives standard output and error back to the terminal, which closes the pipes.
    fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let _ = rustix::stdio::dup2_stdout(&self.out);
        if let Some(err) = &self.err {
            let _ = rustix::stdio::dup2_stderr(err);
        }
    }
}

impl Drop for Live {
    /// A run that ends by a panic still gives the terminal back.
    fn drop(&mut self) {
        self.restore();
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Shared {
    fn print(&self, text: &str) {
        let mut screen = self.screen.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = screen.erase();
        out.push_str(text);
        out.push_str("\x1b[0m\n");
        self.draw(&mut screen, out);
    }

    fn redraw(&self) {
        let mut screen = self.screen.lock().unwrap_or_else(PoisonError::into_inner);
        let out = screen.erase();
        self.draw(&mut screen, out);
    }

    /// Writes `out` and then the lines of the steps, each cut to the width of the terminal so
    /// that none wraps and the next drawing finds them all.
    fn draw(&self, screen: &mut Screen, mut out: String) {
        // A terminal that does not tell its size, like that of `script`, counts as 80 wide.
        let width = (rustix::termios::tcgetwinsize(&screen.terminal).ok())
            .map(|size| usize::from(size.ws_col))
            .filter(|columns| *columns > 0)
            .unwrap_or(80)
            .max(20)
            - 1;
        let (lines, plain) = {
            let board = self.board.lock().unwrap_or_else(PoisonError::into_inner);
            (board.lines(self.color), board.lines(false))
        };
        for (line, plain) in lines.iter().zip(&plain) {
            match plain.chars().count() > width {
                true => out.extend(plain.chars().take(width)),
                false => out.push_str(line),
            }
            out.push_str("\x1b[K\n");
        }
        screen.drawn = lines.len();
        let _ = screen.terminal.write_all(out.as_bytes());
        let _ = screen.terminal.flush();
    }
}

impl Screen {
    /// The codes that move to the first line of the steps and clear them and what follows.
    fn erase(&self) -> String {
        match self.drawn {
            0 => String::new(),
            drawn => format!("\r\x1b[{drawn}A\x1b[J"),
        }
    }
}
