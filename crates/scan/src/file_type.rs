/// What a file holds, judged by its extension. A folder takes the type that fills most of
/// its bytes, except for bundles such as `.app` and `.photoslibrary`, which count as a
/// whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileType {
    Video,
    Audio,
    Image,
    Document,
    Archive,
    Code,
    App,
    Other,
}

impl FileType {
    pub const ALL: [FileType; 8] = [
        FileType::Video,
        FileType::Audio,
        FileType::Image,
        FileType::Document,
        FileType::Archive,
        FileType::Code,
        FileType::App,
        FileType::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FileType::Video => "Video",
            FileType::Audio => "Audio",
            FileType::Image => "Images",
            FileType::Document => "Documents",
            FileType::Archive => "Archives and disk images",
            FileType::Code => "Code and builds",
            FileType::App => "Apps",
            FileType::Other => "Other",
        }
    }

    pub fn of_file(name: &str) -> FileType {
        let Some(ext) = extension(name) else {
            return FileType::Other;
        };
        match ext.as_str() {
            "mp4" | "m4v" | "mov" | "avi" | "mkv" | "webm" | "wmv" | "flv" | "mpg" | "mpeg"
            | "3gp" | "mts" | "m2ts" | "braw" | "r3d" => FileType::Video,
            "mp3" | "m4a" | "m4b" | "aac" | "wav" | "aif" | "aiff" | "flac" | "ogg" | "opus"
            | "wma" | "caf" | "mid" | "midi" => FileType::Audio,
            "jpg" | "jpeg" | "png" | "gif" | "heic" | "heif" | "tif" | "tiff" | "bmp" | "webp"
            | "avif" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "raf" | "psd"
            | "svg" | "ico" | "icns" | "exr" | "hdr" => FileType::Image,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "pages" | "numbers"
            | "key" | "txt" | "rtf" | "md" | "csv" | "epub" | "odt" | "ods" | "odp" | "tex" => {
                FileType::Document
            }
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "dmg" | "iso"
            | "img" | "raw" | "pkg" | "xip" | "sparseimage" => FileType::Archive,
            "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "m" | "mm" | "swift" | "py" | "pyc"
            | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "java" | "class" | "jar" | "kt"
            | "go" | "rb" | "php" | "cs" | "sh" | "lua" | "json" | "yaml" | "yml" | "toml"
            | "xml" | "html" | "css" | "scss" | "sql" | "wasm" | "o" | "a" | "rlib" | "rmeta"
            | "dylib" | "so" | "pch" | "pcm" | "map" => FileType::Code,
            _ => FileType::Other,
        }
    }

    /// The type of a folder macOS shows as a single item, or `None` for a plain folder.
    pub fn of_bundle(name: &str) -> Option<FileType> {
        Some(match extension(name)?.as_str() {
            "app" => FileType::App,
            "photoslibrary" => FileType::Image,
            "fcpbundle" | "imovielibrary" | "tvlibrary" => FileType::Video,
            "logicx" | "band" | "musiclibrary" => FileType::Audio,
            "sparsebundle" => FileType::Archive,
            _ => return None,
        })
    }
}

fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Bytes per [`FileType`], summed up a subtree to find what fills it most.
#[derive(Clone, Copy, Default)]
pub(crate) struct Bytes([u64; 8]);

impl Bytes {
    pub(crate) fn of(file_type: FileType, bytes: u64) -> Bytes {
        let mut out = Bytes::default();
        out.0[file_type as usize] = bytes;
        out
    }

    pub(crate) fn add(&mut self, other: &Bytes) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a += b;
        }
    }

    /// The type with the most bytes, or [`FileType::Other`] when there are none.
    pub(crate) fn most(&self) -> FileType {
        let (i, &bytes) = self
            .0
            .iter()
            .enumerate()
            .max_by_key(|&(i, &b)| (b, std::cmp::Reverse(i)))
            .unwrap();
        if bytes == 0 {
            FileType::Other
        } else {
            FileType::ALL[i]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_decides_a_files_type_ignoring_case() {
        assert_eq!(FileType::of_file("Holiday.MOV"), FileType::Video);
        assert_eq!(FileType::of_file("archive.tar.gz"), FileType::Archive);
        assert_eq!(FileType::of_file("main.rs"), FileType::Code);
        // Virtual machine disks, not camera photos, which have their own extensions.
        assert_eq!(FileType::of_file("data.img.raw"), FileType::Archive);
        assert_eq!(FileType::of_file("Makefile"), FileType::Other);
        assert_eq!(FileType::of_file(".png"), FileType::Other);
    }

    #[test]
    fn only_known_bundles_count_as_a_whole() {
        assert_eq!(FileType::of_bundle("Safari.app"), Some(FileType::App));
        assert_eq!(
            FileType::of_bundle("Photos Library.photoslibrary"),
            Some(FileType::Image)
        );
        assert_eq!(FileType::of_bundle("Documents"), None);
        assert_eq!(FileType::of_bundle("node.js"), None);
    }

    #[test]
    fn most_picks_the_largest_type_and_other_when_empty() {
        let mut bytes = Bytes::of(FileType::Image, 10);
        bytes.add(&Bytes::of(FileType::Video, 30));
        bytes.add(&Bytes::of(FileType::Image, 25));
        assert_eq!(bytes.most(), FileType::Image);
        assert_eq!(Bytes::default().most(), FileType::Other);
    }
}
