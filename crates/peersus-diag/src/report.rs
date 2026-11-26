//! The files of one crash: the minidump, a few lines of text beside it, and how many are kept.
//!
//! A minidump holds the stacks of every thread and the memory they point into, which can include anything the program
//! held (a message being shown, a file name): the folder is the owner's alone (0700), each file too (0600), and nothing is
//! ever sent anywhere. The person reads, shares or deletes them from the Diagnostics page.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// The most crash dumps kept, of every program together; the oldest go first.
pub const KEEP: usize = 10;
/// The largest minidump written. A window with many threads stays far below it; past it the writer drops stack memory.
pub const SIZE_LIMIT: u64 = 64 * 1024 * 1024;
/// Every crash dump is named `crash-<program>-<milliseconds since 1970>.dmp`, its text the same with `.txt`.
pub const PREFIX: &str = "crash-";
pub const DUMP_EXTENSION: &str = "dmp";

/// Makes `folder` (and its parents) and leaves it readable by the owner only.
pub fn private_folder(folder: &Path) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700))
}

/// A NEW file only the owner can read; an existing path is never followed or overwritten.
pub fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// The path of the next dump of `program` in `folder`.
pub fn dump_path(folder: &Path, program: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    folder.join(format!("{PREFIX}{program}-{millis}.{DUMP_EXTENSION}"))
}

/// What the text beside a dump says about the crash.
pub struct Crash<'a> {
    pub program: &'a str,
    pub signal: &'a str,
    pub code: i32,
    pub thread: &'a str,
    pub written: Result<(), String>,
}

/// The few lines beside a dump: what crashed and how to read the dump. No paths, names or contents of the program.
pub fn summary(crash: &Crash<'_>) -> String {
    let outcome = match &crash.written {
        Ok(()) => "The minidump beside this file was written.".to_owned(),
        Err(error) => format!("The minidump could not be written completely: {error}"),
    };
    format!(
        "Peersus crash report\nprogram: {}\nversion: {}\nsignal: {} (code {})\nthread: {}\n\n{outcome}\n\
         Read it with `minidump-stackwalk <file>` (rust-minidump) or a debugger. It holds the stacks of every thread and the\n\
         memory they point into: share it only with someone you trust to look at it.\n",
        crash.program,
        env!("CARGO_PKG_VERSION"),
        crash.signal,
        crash.code,
        crash.thread,
    )
}

/// The stream type of the exception stream in a minidump's directory.
const EXCEPTION_STREAM: u32 = 6;

/// A dump written from outside the process says "dump requested": no signal context crossed over. Writes the signal and
/// its code into the dump's exception record, so a reader names the crash for what it was.
pub fn mark_signal(
    dump: &mut (impl Read + Write + Seek),
    signal: u32,
    code: i32,
) -> io::Result<bool> {
    let mut header = [0u8; 16];
    dump.seek(SeekFrom::Start(0))?;
    dump.read_exact(&mut header)?;
    if &header[..4] != b"MDMP" {
        return Ok(false);
    }
    let word = |bytes: &[u8], at: usize| {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    };
    let streams = word(&header, 8);
    let directory = word(&header, 12);
    // A directory entry is (stream type, size, offset); far more streams than any writer makes is a damaged file.
    if streams > 64 {
        return Ok(false);
    }
    for index in 0..streams {
        let mut entry = [0u8; 12];
        dump.seek(SeekFrom::Start(
            u64::from(directory) + u64::from(index) * 12,
        ))?;
        dump.read_exact(&mut entry)?;
        if word(&entry, 0) != EXCEPTION_STREAM {
            continue;
        }
        let stream = u64::from(word(&entry, 8));
        // MDRawExceptionStream: thread id (4), alignment (4), then the record: exception code (4), flags (4).
        dump.seek(SeekFrom::Start(stream + 8))?;
        dump.write_all(&signal.to_le_bytes())?;
        dump.write_all(&code.to_le_bytes())?;
        dump.flush()?;
        return Ok(true);
    }
    Ok(false)
}

/// Removes the oldest crash dumps (and their text) past [`KEEP`]. Other files in the folder are not this helper's.
pub fn keep_newest(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    let mut dumps: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(PREFIX) && name.ends_with(&format!(".{DUMP_EXTENSION}"))
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    if dumps.len() <= KEEP {
        return;
    }
    dumps.sort();
    for (_, path) in &dumps[..dumps.len() - KEEP] {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("txt"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn dump_with_exception_stream() -> Vec<u8> {
        let mut bytes = vec![0u8; 96];
        bytes[..4].copy_from_slice(b"MDMP");
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&32u32.to_le_bytes());
        // Entry 0: some other stream; entry 1: the exception stream at offset 56.
        bytes[32..36].copy_from_slice(&3u32.to_le_bytes());
        bytes[44..48].copy_from_slice(&EXCEPTION_STREAM.to_le_bytes());
        bytes[52..56].copy_from_slice(&56u32.to_le_bytes());
        bytes[56..60].copy_from_slice(&1234u32.to_le_bytes());
        bytes[64..68].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        bytes
    }

    #[test]
    fn the_signal_and_its_code_replace_the_requested_dump_mark_and_nothing_else_changes() {
        let original = dump_with_exception_stream();
        let mut dump = Cursor::new(original.clone());
        assert!(mark_signal(&mut dump, 11, 1).unwrap());
        let marked = dump.into_inner();
        assert_eq!(&marked[64..68], &11u32.to_le_bytes());
        assert_eq!(&marked[68..72], &1i32.to_le_bytes());
        // The thread id and everything outside the code and flags stay as written.
        assert_eq!(&marked[..64], &original[..64]);
        assert_eq!(&marked[72..], &original[72..]);
    }

    #[test]
    fn a_file_that_is_not_a_minidump_or_has_no_exception_stream_is_left_alone() {
        let mut other = Cursor::new(vec![7u8; 64]);
        assert!(!mark_signal(&mut other, 11, 1).unwrap());
        assert_eq!(other.into_inner(), vec![7u8; 64]);
        let mut without = dump_with_exception_stream();
        without[44..48].copy_from_slice(&4u32.to_le_bytes());
        let copy = without.clone();
        let mut dump = Cursor::new(without);
        assert!(!mark_signal(&mut dump, 11, 1).unwrap());
        assert_eq!(dump.into_inner(), copy);
    }

    #[test]
    fn only_the_newest_dumps_and_their_text_are_kept_and_other_files_are_not_touched() {
        let folder = tempfile::tempdir().unwrap();
        for n in 0..KEEP + 3 {
            let dump = folder
                .path()
                .join(format!("{PREFIX}peersus-{n:03}.{DUMP_EXTENSION}"));
            fs::write(&dump, b"d").unwrap();
            fs::write(dump.with_extension("txt"), b"t").unwrap();
            let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + n as u64);
            File::options()
                .write(true)
                .open(&dump)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        fs::write(folder.path().join("peersus-ui.log"), b"log").unwrap();
        keep_newest(folder.path());
        let mut left: Vec<String> = fs::read_dir(folder.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP * 2 + 1);
        assert!(left.contains(&"peersus-ui.log".to_owned()));
        assert!(
            !left
                .iter()
                .any(|name| name.contains("-000.") || name.contains("-002."))
        );
        assert!(left.contains(&format!("{PREFIX}peersus-003.{DUMP_EXTENSION}")));
    }

    #[test]
    fn a_new_report_file_is_private_and_never_replaces_one_that_exists() {
        let folder = tempfile::tempdir().unwrap();
        let inner = folder.path().join("diagnostics");
        private_folder(&inner).unwrap();
        assert_eq!(
            fs::metadata(&inner).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let path = inner.join("crash-x-1.dmp");
        private_file(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(private_file(&path).is_err());
    }
}
