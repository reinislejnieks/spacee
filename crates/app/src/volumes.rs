use std::ffi::{CStr, c_void};
use std::mem::offset_of;
use std::path::PathBuf;

pub struct Volume {
    pub name: String,
    pub mount: PathBuf,
    pub total: u64,
    pub available: u64,
}

/// Local volumes that Finder would show, skipping system-internal mounts such as
/// `/System/Volumes/Data` or the VM volume.
pub fn list() -> Vec<Volume> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 {
        return vec![];
    }
    let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };
    mounts
        .iter()
        .filter(|m| {
            let flags = m.f_flags as i32;
            flags & libc::MNT_LOCAL != 0 && flags & libc::MNT_DONTBROWSE == 0 && m.f_blocks > 0
        })
        .map(|m| {
            let mount = unsafe { CStr::from_ptr(m.f_mntonname.as_ptr()) };
            let path = PathBuf::from(mount.to_string_lossy().into_owned());
            let name = volume_name(mount).unwrap_or_else(|| path.display().to_string());
            Volume {
                name,
                mount: path,
                total: m.f_blocks * m.f_bsize as u64,
                available: m.f_bavail * m.f_bsize as u64,
            }
        })
        .collect()
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
