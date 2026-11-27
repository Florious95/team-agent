//! Stop-only image evidence. A vanished pathname is neither exit nor authority.
use super::{image_stamp, HostError, HostErrorKind, ImageStamp, ProcessStamp, ProcessState};
use libproc::libproc::{
    bsd_info::BSDInfo,
    proc_pid::{pidinfo, pidpath, PIDInfo, PidInfoFlavor},
};
use nix::errno::Errno;

// <sys/proc_info.h>: proc_regionwithpathinfo = proc_regioninfo + vnode_info_path.
// libc supplies the vnode ABI. Query address zero exactly once, never search the
// address space for an arbitrary mapping of the expected file.
#[repr(C)]
struct FirstRegion {
    protection: u32,
    protection_fields: [u32; 3],
    offset: u64,
    counters: [u32; 14],
    address: u64,
    size: u64,
    vnode: nix::libc::vnode_info_path,
}
impl PIDInfo for FirstRegion {
    fn flavor() -> PidInfoFlavor {
        PidInfoFlavor::RegionPathInfo
    }
}

fn text_image(pid: i32) -> Option<(ImageStamp, bool)> {
    let region = pidinfo::<FirstRegion>(pid, 0).ok()?;
    let st = region.vnode.vip_vi.vi_stat;
    if region.protection != (nix::libc::PROT_READ | nix::libc::PROT_EXEC) as u32
        || region.address == 0
        || region.size == 0
        || st.vst_ino == 0
    {
        return None;
    }
    let stamp = ImageStamp {
        device: st.vst_dev.into(),
        inode: st.vst_ino,
        length: st.vst_size.try_into().ok()?,
        modified_ns: i128::from(st.vst_mtime) * 1_000_000_000 + i128::from(st.vst_mtimensec),
    };
    Some((stamp, st.vst_nlink == 0))
}

fn birth(pid: i32, expected: &ProcessStamp) -> ProcessState {
    match pidinfo::<BSDInfo>(pid, 0) {
        Ok(info) if info.pbi_status == 5 => ProcessState::Exited,
        Ok(info)
            if info.pbi_pid == expected.identity.pid
                && info.pbi_ppid == expected.parent
                && expected.identity.birth_identity
                    == format!("macos:{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec) =>
        {
            ProcessState::Alive
        }
        Ok(_) => ProcessState::Replaced,
        Err(_) => match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Err(Errno::ESRCH) => ProcessState::Exited,
            _ => {
                ProcessState::Unknown(HostError::new("stop process birth", HostErrorKind::Unknown))
            }
        },
    }
}

pub(super) fn sample(expected: &ProcessStamp) -> ProcessState {
    let Some(pid) = i32::try_from(expected.identity.pid)
        .ok()
        .filter(|pid| *pid > 1)
    else {
        return ProcessState::Unknown(HostError::new("stop process pid", HostErrorKind::Invalid));
    };
    let first = birth(pid, expected);
    if first != ProcessState::Alive {
        return first;
    }
    let unknown =
        || ProcessState::Unknown(HostError::new("stop process image", HostErrorKind::Unknown));
    let Some((mapped, unlinked)) = text_image(pid) else {
        return unknown();
    };
    if mapped != expected.image {
        return ProcessState::Replaced;
    }
    let owned = match pidpath(pid) {
        Ok(path) => match std::fs::metadata(path) {
            Ok(meta) => image_stamp(&meta) == expected.image || unlinked,
            Err(_) => return unknown(),
        },
        // libproc 0.14's safe wrapper records a numeric errno before its
        // localized description. An encoding error is not an ENOENT result.
        Err(error) if error.starts_with("return code = 0, errno = 2, ") => unlinked,
        Err(_) => return unknown(),
    };
    if !owned {
        return ProcessState::Replaced;
    }
    birth(pid, expected)
}
