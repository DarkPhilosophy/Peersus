//! Reading the system clipboard for files to send. The only code that touches the clipboard; the
//! decision of what to do with what it holds is `intake::pasted`, which is tried without one.

use std::path::PathBuf;

/// The files on the system clipboard now: a list of files if a file manager put one there, or
/// else a list of `file://` addresses in text. Empty when there is none, or when the clipboard
/// cannot be read at all: pasting then simply does nothing, and never fails the window.
///
/// This may block while the owner of the clipboard answers, so it is called off the thread that
/// draws.
#[cfg(not(target_os = "android"))]
pub fn read() -> Vec<PathBuf> {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return Vec::new();
    };
    // Only the list of files a file manager put there; the text on the clipboard is never read,
    // because anyone can put text there, including the address of a file that was never chosen.
    crate::intake::pasted(clipboard.get().file_list().ok())
}

/// On Android a file is chosen with the system picker; there is no clipboard of files to read.
#[cfg(target_os = "android")]
pub fn read() -> Vec<PathBuf> {
    Vec::new()
}
