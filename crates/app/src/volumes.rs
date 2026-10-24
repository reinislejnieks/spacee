use std::ffi::{CStr, c_char, c_void};
use std::mem::offset_of;
use std::path::{Path, PathBuf};

pub struct Volume {
    pub name: String,
    pub mount: PathBuf,
    pub kind: Kind,
    pub total: u64,
    pub available: u64,
}

impl Volume {
    pub fn used_fraction(&self) -> f32 {
        self.total.saturating_sub(self.available) as f32 / self.total.max(1) as f32
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// The volume mounted at `/`.
    Startup,
    DiskImage,
    /// A share mounted from another machine, such as `host` over SMB or NFS.
    Network {
        host: String,
    },
    /// Any other local volume, such as an external drive.
    Local,
}

impl Kind {
    /// The section of the volumes screen listing this kind, or `None` for the startup
    /// disk, which has its own panel.
    pub fn group(&self) -> Option<Group> {
        match self {
            Kind::Startup => None,
            Kind::Local => Some(Group::Drives),
            Kind::DiskImage => Some(Group::DiskImages),
            Kind::Network { .. } => Some(Group::Network),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Group {
    Drives,
    DiskImages,
    Network,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Drives, Group::DiskImages, Group::Network];

    pub fn title(self) -> &'static str {
        match self {
            Group::Drives => "DRIVES",
            Group::DiskImages => "DISK IMAGES",
            Group::Network => "NETWORK",
        }
    }

    pub fn note(self) -> Option<&'static str> {
        (self == Group::Network).then_some("Scanning over the network is slow.")
    }
}

/// How full a volume is, for its colour and label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Fine,
    FillingUp,
    AlmostFull,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Fine => "Plenty of space",
            Status::FillingUp => "Filling up",
            Status::AlmostFull => "Almost full",
        }
    }

    pub fn of(used_fraction: f32) -> Status {
        if used_fraction > 0.9 {
            Status::AlmostFull
        } else if used_fraction >= 0.75 {
            Status::FillingUp
        } else {
            Status::Fine
        }
    }
}

const NETWORK_FILESYSTEMS: [&str; 4] = ["smbfs", "nfs", "afpfs", "webdav"];

/// Volumes that Finder would show, local and network, skipping system-internal mounts
/// such as `/System/Volumes/Data` or the VM volume.
pub fn list() -> Vec<Volume> {
    scan::mounts()
        .iter()
        .filter(|m| {
            let flags = m.f_flags as i32;
            let network = NETWORK_FILESYSTEMS.contains(&&*string(&m.f_fstypename));
            (flags & libc::MNT_LOCAL != 0 || network)
                && flags & libc::MNT_DONTBROWSE == 0
                && m.f_blocks > 0
        })
        .map(|m| {
            let mount = unsafe { CStr::from_ptr(m.f_mntonname.as_ptr()) };
            let path = PathBuf::from(mount.to_string_lossy().into_owned());
            let name = volume_name(mount).unwrap_or_else(|| path.display().to_string());
            let from = string(&m.f_mntfromname);
            let local = m.f_flags as i32 & libc::MNT_LOCAL != 0;
            Volume {
                name,
                kind: kind(&path, local, &from, is_disk_image),
                mount: path,
                total: m.f_blocks * m.f_bsize as u64,
                available: m.f_bavail * m.f_bsize as u64,
            }
        })
        .collect()
}

fn string(chars: &[c_char]) -> String {
    unsafe { CStr::from_ptr(chars.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// `from` is what the volume was mounted from: a device such as `/dev/disk7s1` for a
/// local volume, or a share such as `//user@host/share` or `host:/path` for a network one.
fn kind(mount: &Path, local: bool, from: &str, is_disk_image: impl Fn(&str) -> bool) -> Kind {
    if mount == Path::new("/") {
        Kind::Startup
    } else if !local {
        Kind::Network { host: host(from) }
    } else if from.strip_prefix("/dev/").is_some_and(is_disk_image) {
        Kind::DiskImage
    } else {
        Kind::Local
    }
}

fn host(from: &str) -> String {
    let host = match from.strip_prefix("//") {
        Some(share) => share.split('/').next().unwrap_or(share),
        None => from.split(':').next().unwrap_or(from),
    };
    host.rsplit('@').next().unwrap_or(host).to_string()
}

/// Whether the device named `bsd_name`, such as `disk7s1`, is an attached disk image,
/// which Disk Arbitration reports with the device model "Disk Image".
fn is_disk_image(bsd_name: &str) -> bool {
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, enc: u32) -> u8;
        fn CFRelease(cf: *const c_void);
    }
    #[link(name = "DiskArbitration", kind = "framework")]
    unsafe extern "C" {
        static kDADiskDescriptionDeviceModelKey: *const c_void;
        fn DASessionCreate(allocator: *const c_void) -> *const c_void;
        fn DADiskCreateFromBSDName(
            allocator: *const c_void,
            session: *const c_void,
            name: *const c_char,
        ) -> *const c_void;
        fn DADiskCopyDescription(disk: *const c_void) -> *const c_void;
    }
    const UTF8: u32 = 0x0800_0100;

    let Ok(name) = std::ffi::CString::new(bsd_name) else {
        return false;
    };
    unsafe {
        let session = DASessionCreate(std::ptr::null());
        if session.is_null() {
            return false;
        }
        let disk = DADiskCreateFromBSDName(std::ptr::null(), session, name.as_ptr());
        let mut model = [0 as c_char; 64];
        let mut found = false;
        if !disk.is_null() {
            let description = DADiskCopyDescription(disk);
            if !description.is_null() {
                let value = CFDictionaryGetValue(description, kDADiskDescriptionDeviceModelKey);
                found = !value.is_null()
                    && CFGetTypeID(value) == CFStringGetTypeID()
                    && CFStringGetCString(value, model.as_mut_ptr(), 64, UTF8) != 0;
                CFRelease(description);
            }
            CFRelease(disk);
        }
        CFRelease(session);
        found && CStr::from_ptr(model.as_ptr()).to_bytes() == b"Disk Image"
    }
}

/// The name Finder shows for the volume mounted at `mount`, such as "Macintosh HD".
fn volume_name(mount: &CStr) -> Option<String> {
    #[repr(C)]
    struct Reply {
        length: u32,
        name: libc::attrreference_t,
        data: [u8; 1024],
    }
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_NAME,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply: Reply = unsafe { std::mem::zeroed() };
    let status = unsafe {
        libc::getattrlist(
            mount.as_ptr(),
            (&mut request as *mut libc::attrlist).cast::<c_void>(),
            (&mut reply as *mut Reply).cast::<c_void>(),
            size_of::<Reply>(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    // The name's offset is relative to its attrreference_t, and its length includes the NUL.
    let start = offset_of!(Reply, name) + reply.name.attr_dataoffset as usize;
    let bytes = unsafe { (&reply as *const Reply).cast::<u8>().add(start) };
    let len = (reply.name.attr_length as usize).saturating_sub(1);
    let name = unsafe { std::slice::from_raw_parts(bytes, len) };
    Some(String::from_utf8_lossy(name).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_thresholds() {
        assert_eq!(Status::of(0.5), Status::Fine);
        assert_eq!(Status::of(0.75), Status::FillingUp);
        assert_eq!(Status::of(0.9), Status::FillingUp);
        assert_eq!(Status::of(0.96), Status::AlmostFull);
    }

    #[test]
    fn kind_from_mount_and_source() {
        let image = |name: &str| name == "disk7s1";
        let kind = |mount: &str, local, from: &str| kind(Path::new(mount), local, from, image);
        assert_eq!(kind("/", true, "/dev/disk3s1s1"), Kind::Startup);
        assert_eq!(kind("/Volumes/Zed", true, "/dev/disk7s1"), Kind::DiskImage);
        assert_eq!(kind("/Volumes/Backup", true, "/dev/disk4s2"), Kind::Local);
        assert_eq!(
            kind("/Volumes/share", false, "//user@nas.local/share"),
            Kind::Network {
                host: "nas.local".into()
            }
        );
        assert_eq!(
            kind("/Volumes/export", false, "server:/export"),
            Kind::Network {
                host: "server".into()
            }
        );
    }

    #[test]
    fn attached_disk_image_is_detected_and_the_internal_disk_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("test.dmg");
        let hdiutil = |args: &[&str]| {
            let out = std::process::Command::new("hdiutil")
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "hdiutil {args:?} failed");
            String::from_utf8(out.stdout).unwrap()
        };
        let image = image.to_str().unwrap();
        hdiutil(&[
            "create",
            "-quiet",
            "-size",
            "1m",
            "-fs",
            "HFS+",
            "-volname",
            "SpaceeTest",
            image,
        ]);
        let attached = hdiutil(&["attach", "-nobrowse", "-noverify", image]);
        // The first line names the whole image's device, such as "/dev/disk9".
        let device = attached.split_whitespace().next().unwrap().to_string();
        let bsd_name = device.strip_prefix("/dev/").unwrap();
        let detected = is_disk_image(bsd_name);
        hdiutil(&["detach", "-quiet", &device]);
        assert!(detected);
        assert!(!is_disk_image("disk0"));
        assert!(!is_disk_image("disk999"));
    }
}
