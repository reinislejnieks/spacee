use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use objc2_foundation::{NSFileManager, NSURL};

/// Moves `path` to the Trash, so it can still be recovered.
pub fn trash(path: &Path) -> io::Result<()> {
    let url = NSURL::from_file_path(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file path"))?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))
}

pub fn reveal(path: &Path) {
    spawn(Command::new("open").arg("-R").arg(path));
}

pub fn quick_look(path: &Path) {
    spawn(Command::new("qlmanage").arg("-p").arg(path));
}

/// Runs `command` without blocking, reaping it on a background thread when it exits.
fn spawn(command: &mut Command) {
    match command.stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("{command:?}: {e}"),
    }
}
