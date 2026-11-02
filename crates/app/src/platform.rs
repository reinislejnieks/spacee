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

/// macOS gives no API to ask, so this tries to read a file that only Full Disk Access unlocks.
pub fn has_full_disk_access() -> bool {
    std::fs::File::open("/Library/Application Support/com.apple.TCC/TCC.db").is_ok()
}

pub fn open_full_disk_access_settings() {
    spawn(
        Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"),
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn trash_moves_the_item_to_the_trash_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("spacee-trash-test-{nanos}.txt");
        let path = dir.path().join(&name);
        std::fs::write(&path, "keep me").unwrap();

        trash(&path).unwrap();

        assert!(!path.exists(), "the item left its folder");
        let in_trash = std::env::home_dir().unwrap().join(".Trash").join(&name);
        match std::fs::read_to_string(&in_trash) {
            Ok(contents) => {
                assert_eq!(contents, "keep me", "the Trash holds the same file");
                std::fs::remove_file(&in_trash).unwrap();
            }
            // Without Full Disk Access, macOS hides the Trash from the test process.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("can't read {}: {e}", in_trash.display())
            }
            Err(e) => panic!("{} not found: {e}", in_trash.display()),
        }
    }

    #[test]
    fn trash_of_a_missing_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(trash(&dir.path().join("missing")).is_err());
    }
}
