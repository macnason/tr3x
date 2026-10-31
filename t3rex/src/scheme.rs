//! Light or dark, as the terminal reports it. Rex answers mode 2031 with the scheme of the
//! device that owns the pane (`CSI ? 997 ; 1 n` dark, `2 n` light), and sends it again when
//! that changes, so pictures and logos follow the screen they're seen on, not the Mac that
//! runs the pane. Without a report, the Mac's own appearance stands in.
//!
//! crossterm takes `CSI ? 997 ; n` for the start of a sequence it hasn't finished reading and
//! waits on it forever, which would swallow every key after it. So input passes through a
//! pseudo-terminal first: a thread reads the real terminal, keeps the reports, and hands
//! everything else on. crossterm reads the other end, which is a terminal too, while sizes
//! still come from the controlling terminal.

use std::os::fd::RawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::sync::watch;

const UNKNOWN: u8 = 0;
const DARK: u8 = 1;
const LIGHT: u8 = 2;

static REPORTED: AtomicU8 = AtomicU8::new(UNKNOWN);
static CHANGES: Mutex<Option<watch::Sender<Option<bool>>>> = Mutex::new(None);
/// The real terminal and its settings from before, while input passes through the filter.
static FILTER: Mutex<Option<(RawFd, libc::termios)>> = Mutex::new(None);

/// Starts listening for the terminal's scheme. Call before crossterm reads any input. Returns
/// what changes: `Some(true)` for dark, once a report comes.
pub fn listen() -> watch::Receiver<Option<bool>> {
    let (tx, rx) = watch::channel(None);
    *CHANGES.lock().unwrap() = Some(tx);
    // SAFETY: plain calls on file descriptors this process owns.
    if unsafe { libc::isatty(0) } == 1 && unsafe { start() }.is_ok() {
        out("\x1b[?2031h\x1b[?996n");
    }
    rx
}

/// Whether the screen is dark: the terminal's report, else the Mac's appearance.
pub fn dark() -> bool {
    match REPORTED.load(Ordering::Relaxed) {
        DARK => true,
        LIGHT => false,
        _ => crate::media::mac_is_dark(),
    }
}

/// `dark`, waiting a moment for the terminal's first report, for a pane that has just started.
pub async fn dark_soon() -> bool {
    for _ in 0..10 {
        if REPORTED.load(Ordering::Relaxed) != UNKNOWN || FILTER.lock().unwrap().is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    dark()
}

/// Puts input back as it was, before the process exits or runs another program in its place.
pub fn stop() {
    let Some((real, saved)) = FILTER.lock().unwrap().take() else {
        return;
    };
    out("\x1b[?2031l");
    // SAFETY: `real` is the terminal this process was started on, kept open since `start`.
    unsafe {
        libc::dup2(real, 0);
        libc::tcsetattr(0, libc::TCSANOW, &saved);
        libc::close(real);
    }
}

fn out(text: &str) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

unsafe fn start() -> std::io::Result<()> {
    let fail = || Err(std::io::Error::last_os_error());
    unsafe {
        let mut saved: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut saved) != 0 {
            return fail();
        }
        let (mut master, mut slave) = (0, 0);
        let mut raw = saved;
        libc::cfmakeraw(&mut raw);
        if libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            &mut raw,
            std::ptr::null_mut(),
        ) != 0
        {
            return fail();
        }
        let real = libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3);
        if real < 0 {
            libc::close(master);
            libc::close(slave);
            return fail();
        }
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
        // The real terminal goes raw too, so keys reach the filter as they're typed. crossterm
        // sets the end it reads as it always does.
        libc::tcsetattr(real, libc::TCSANOW, &raw);
        libc::dup2(slave, 0);
        libc::close(slave);
        *FILTER.lock().unwrap() = Some((real, saved));
        std::thread::spawn(move || pass(real, master));
    }
    Ok(())
}

/// Copies the real terminal's input to the pseudo-terminal, keeping the scheme reports.
fn pass(real: RawFd, master: RawFd) {
    let mut strip = Strip::default();
    let mut buf = [0u8; 4096];
    loop {
        // A report's first bytes wait for the rest, but only briefly: a lone Esc is a key.
        let mut poll = libc::pollfd {
            fd: real,
            events: libc::POLLIN,
            revents: 0,
        };
        let wait = if strip.held.is_empty() { -1 } else { 30 };
        // SAFETY: one pollfd that lives across the call.
        let ready = unsafe { libc::poll(&mut poll, 1, wait) };
        if ready == 0 {
            write_all(master, &strip.flush());
            continue;
        }
        // SAFETY: reads into a buffer of the length given.
        let n = unsafe { libc::read(real, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        let (keep, scheme) = strip.feed(&buf[..n as usize]);
        if let Some(scheme) = scheme {
            REPORTED.store(scheme, Ordering::Relaxed);
            if let Some(tx) = CHANGES.lock().unwrap().as_ref() {
                tx.send_if_modified(|now| {
                    let dark = Some(scheme == DARK);
                    let changed = *now != dark;
                    *now = dark;
                    changed
                });
            }
        }
        write_all(master, &keep);
    }
}

fn write_all(fd: RawFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: writes from a slice of the length given.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n <= 0 {
            return;
        }
        bytes = &bytes[n as usize..];
    }
}

const REPORT: &[u8] = b"\x1b[?997;";

/// Takes `CSI ? 997 ; Ps n` out of a stream of input, across reads.
#[derive(Default)]
struct Strip {
    /// The start of what may be a report, held until it is one or isn't.
    held: Vec<u8>,
}

impl Strip {
    /// The bytes to pass on, and the last scheme reported among them.
    fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Option<u8>) {
        let mut out = Vec::with_capacity(input.len());
        let mut scheme = None;
        for &b in input {
            self.held.push(b);
            let n = self.held.len();
            let fits = if n <= REPORT.len() {
                self.held[..] == REPORT[..n]
            } else {
                b.is_ascii_digit() && n <= REPORT.len() + 3 || b == b'n' && n > REPORT.len() + 1
            };
            if !fits {
                // Not a report: pass it on, though the last byte may start one.
                let last = self.held.pop().unwrap_or_default();
                out.append(&mut self.held);
                if last == 0x1b {
                    self.held.push(last);
                } else {
                    out.push(last);
                }
            } else if b == b'n' && n > REPORT.len() {
                scheme = match &self.held[REPORT.len()..n - 1] {
                    b"1" => Some(DARK),
                    b"2" => Some(LIGHT),
                    _ => scheme,
                };
                self.held.clear();
            }
        }
        (out, scheme)
    }

    /// What's held, when nothing more has come to finish it.
    fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_come_out_and_keys_stay() {
        let mut s = Strip::default();
        assert_eq!(s.feed(b"ab\x1b[?997;2ncd"), (b"abcd".to_vec(), Some(LIGHT)));
        // Split across reads.
        assert_eq!(s.feed(b"x\x1b[?99"), (b"x".to_vec(), None));
        assert_eq!(s.feed(b"7;1ny"), (b"y".to_vec(), Some(DARK)));
        // Other sequences pass whole: an arrow, an SGR click, a paste, a lone Esc.
        for keys in [
            &b"\x1b[A"[..],
            b"\x1b[<0;10;5M",
            b"\x1b[200~hi\x1b[201~",
            b"\x1b\x1b[B",
        ] {
            let (mut out, scheme) = s.feed(keys);
            out.extend(s.flush());
            assert_eq!((out, scheme), (keys.to_vec(), None));
        }
        let (out, _) = s.feed(b"\x1b");
        assert!(out.is_empty());
        assert_eq!(s.flush(), b"\x1b");
        // Close to a report but not one.
        let (mut out, scheme) = s.feed(b"\x1b[?997;12345n");
        out.extend(s.flush());
        assert_eq!((out, scheme), (b"\x1b[?997;12345n".to_vec(), None));
    }
}
