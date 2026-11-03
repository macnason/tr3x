//! The one way tr3x writes to its terminal: the whole string under one stdout lock, flushed
//! before returning, write errors ignored (a closed pane has nothing left to tell).

use std::io::Write;

pub fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}
