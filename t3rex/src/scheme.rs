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
//!
//! The filter also keeps the terminal's clipboard replies (OSC 5522), for `paste`: crossterm
//! would read their bytes as keys.

use std::os::fd::RawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tokio::sync::{mpsc, watch};

const UNKNOWN: u8 = 0;
const DARK: u8 = 1;
const LIGHT: u8 = 2;

static REPORTED: AtomicU8 = AtomicU8::new(UNKNOWN);
static CHANGES: Mutex<Option<watch::Sender<Option<bool>>>> = Mutex::new(None);
/// Where the clipboard replies go, each without its `OSC 5522 ;` and terminator.
static CLIPBOARD: Mutex<Option<mpsc::UnboundedSender<String>>> = Mutex::new(None);
/// The real terminal and its settings from before, while input passes through the filter.
static FILTER: Mutex<Option<(RawFd, libc::termios)>> = Mutex::new(None);
/// Whether this process has the kitty keyboard protocol on.
static KEYS: AtomicBool = AtomicBool::new(false);

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

/// The terminal's clipboard replies, once input passes through the filter. `None` without it,
/// where they would reach crossterm as keys, so nothing should ask for them.
pub fn clipboard() -> Option<mpsc::UnboundedReceiver<String>> {
    FILTER.lock().unwrap().as_ref()?;
    let (tx, rx) = mpsc::unbounded_channel();
    *CLIPBOARD.lock().unwrap() = Some(tx);
    Some(rx)
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

/// Turns on the kitty keyboard protocol's first level (`CSI > 1 u`) until the guard goes.
/// Without it Rex sends Shift+Enter as `CSI 27 ; 2 ; 13 ~`, which crossterm drops, and Cmd+Delete
/// as a plain Delete; with it they're `CSI 13 ; 2 u` and `CSI 127 ; 9 u`, and Esc is a key of
/// its own. Every byte of it passes the filter untouched. Each screen keeps its own flags, so
/// turn it on after entering the alternate screen and drop the guard before leaving it. The
/// guard also turns it off when the pane's work is dropped for a signal or unwinds from a panic.
pub fn keys() -> Keys {
    keys_on();
    Keys
}

pub struct Keys;

impl Drop for Keys {
    fn drop(&mut self) {
        keys_off();
    }
}

/// `keys` without a guard, for turning it back on after `keys_off`.
pub fn keys_on() {
    if !KEYS.swap(true, Ordering::Relaxed) {
        out("\x1b[>1u");
    }
}

pub fn keys_off() {
    if KEYS.swap(false, Ordering::Relaxed) {
        out("\x1b[<u");
    }
}

/// Puts input back as it was, before the process exits or runs another program in its place.
pub fn stop() {
    keys_off();
    let Some((real, saved)) = FILTER.lock().unwrap().take() else {
        return;
    };
    out("\x1b[?5522l\x1b[?2031l");
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
        // A clipboard reply can take a while to cross a network, so it gets longer.
        let wait = match (strip.held.is_empty(), strip.osc) {
            (true, _) => -1,
            (false, true) => 5000,
            (false, false) => 30,
        };
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
        let (keep, scheme, packets) = strip.feed(&buf[..n as usize]);
        if let Some(tx) = CLIPBOARD.lock().unwrap().as_ref() {
            for packet in packets {
                let _ = tx.send(packet);
            }
        }
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
const OSC: &[u8] = b"\x1b]5522;";

/// Takes `CSI ? 997 ; Ps n` and `OSC 5522 ; … ST` out of a stream of input, across reads.
#[derive(Default)]
struct Strip {
    /// The start of what may be a report, held until it is one or isn't.
    held: Vec<u8>,
    /// Inside a clipboard reply: everything is held until its terminator.
    osc: bool,
}

impl Strip {
    /// The bytes to pass on, the last scheme reported among them, and the clipboard replies.
    fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Option<u8>, Vec<String>) {
        let mut out = Vec::with_capacity(input.len());
        let mut scheme = None;
        let mut packets = Vec::new();
        for &b in input {
            if self.osc {
                // BEL or ESC \ ends it.
                if b == 0x07 || b == b'\\' && self.held.last() == Some(&0x1b) {
                    let end = self.held.len() - usize::from(b != 0x07);
                    packets.push(String::from_utf8_lossy(&self.held[OSC.len()..end]).into_owned());
                    self.held.clear();
                    self.osc = false;
                } else {
                    self.held.push(b);
                }
                continue;
            }
            self.held.push(b);
            let n = self.held.len();
            if n <= OSC.len() && self.held[..] == OSC[..n] {
                self.osc = n == OSC.len();
                continue;
            }
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
        (out, scheme, packets)
    }

    /// What's held, when nothing more has come to finish it. A clipboard reply cut short goes.
    fn flush(&mut self) -> Vec<u8> {
        if std::mem::take(&mut self.osc) {
            self.held.clear();
        }
        std::mem::take(&mut self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_come_out_and_keys_stay() {
        let mut s = Strip::default();
        let none = Vec::<String>::new();
        assert_eq!(
            s.feed(b"ab\x1b[?997;2ncd"),
            (b"abcd".to_vec(), Some(LIGHT), none.clone())
        );
        // Split across reads.
        assert_eq!(s.feed(b"x\x1b[?99"), (b"x".to_vec(), None, none.clone()));
        assert_eq!(s.feed(b"7;1ny"), (b"y".to_vec(), Some(DARK), none.clone()));
        // Other sequences pass whole: an arrow, an SGR click, a paste, a lone Esc.
        for keys in [
            &b"\x1b[A"[..],
            b"\x1b[<0;10;5M",
            b"\x1b[200~hi\x1b[201~",
            b"\x1b\x1b[B",
        ] {
            let (mut out, scheme, _) = s.feed(keys);
            out.extend(s.flush());
            assert_eq!((out, scheme), (keys.to_vec(), None));
        }
        let (out, _, _) = s.feed(b"\x1b");
        assert!(out.is_empty());
        assert_eq!(s.flush(), b"\x1b");
        // Close to a report but not one.
        let (mut out, scheme, _) = s.feed(b"\x1b[?997;12345n");
        out.extend(s.flush());
        assert_eq!((out, scheme), (b"\x1b[?997;12345n".to_vec(), None));
    }

    #[test]
    fn clipboard_replies_come_out() {
        let mut s = Strip::default();
        let (out, _, packets) = s.feed(b"a\x1b]5522;type=read:status=OK\x1b\\b\x1b]5522;type=re");
        assert_eq!(
            (out, packets),
            (b"ab".to_vec(), vec!["type=read:status=OK".to_string()])
        );
        // Split across reads, ended with BEL.
        let (out, _, packets) = s.feed(b"ad:status=DONE\x07c");
        assert_eq!(
            (out, packets),
            (b"c".to_vec(), vec!["type=read:status=DONE".to_string()])
        );
        // Alt+] is a key, not a reply.
        let (mut out, _, packets) = s.feed(b"\x1b]x");
        out.extend(s.flush());
        assert_eq!((out, packets.len()), (b"\x1b]x".to_vec(), 0));
        // A reply cut short goes, rather than reaching crossterm.
        let (out, _, _) = s.feed(b"\x1b]5522;type=read:status=DA");
        assert!(out.is_empty());
        assert!(s.flush().is_empty());
        assert!(!s.osc);
    }
}
