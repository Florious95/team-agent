//! Remove only a positively dead, owned Unix socket; never stop its server.
use std::ffi::CString;
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SocketCleanup {
    Absent,
    Removed,
    Preserved,
}

pub(super) fn remove_dead_socket(path: &Path) -> io::Result<SocketCleanup> {
    remove_with_probe(path, listener_refused)
}

fn identity(metadata: &Metadata) -> (u64, u64, u32, u32, u64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.mode(),
        metadata.uid(),
        metadata.nlink(),
    )
}

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

fn remove_with_probe(
    path: &Path,
    probe: impl FnOnce(&Path) -> io::Result<bool>,
) -> io::Result<SocketCleanup> {
    let socket = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(SocketCleanup::Absent),
        Err(error) => return Err(error),
    };
    // SAFETY: geteuid has no pointer arguments or ownership effects.
    let uid = unsafe { libc::geteuid() };
    if !socket.file_type().is_socket() || socket.uid() != uid || socket.nlink() != 1 {
        return Err(invalid("shutdown socket is not an owned single-link socket"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("socket parent missing"))?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || parent.canonicalize()? != parent
    {
        return Err(invalid(
            "shutdown socket parent is not a canonical private owned directory",
        ));
    }
    let directory: File = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(parent)?;
    if identity(&directory.metadata()?) != identity(&metadata) {
        return Err(invalid("shutdown socket parent changed"));
    }
    if !probe(path)? {
        return Ok(SocketCleanup::Preserved);
    }
    if identity(&std::fs::symlink_metadata(parent)?) != identity(&metadata) {
        return Err(invalid("shutdown socket parent changed after death probe"));
    }
    let name = CString::new(
        path.file_name()
            .ok_or_else(|| invalid("socket name missing"))?
            .as_bytes(),
    )
    .map_err(|_| invalid("socket name contains NUL"))?;
    // SAFETY: stat is an all-integer C record; directory/name are valid for the call.
    let mut fresh: libc::stat = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut fresh,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(SocketCleanup::Absent)
        } else {
            Err(error)
        };
    }
    if fresh.st_dev as u64 != socket.dev()
        || fresh.st_ino as u64 != socket.ino()
        || fresh.st_mode as u32 != socket.mode()
        || fresh.st_uid != socket.uid()
        || fresh.st_nlink as u64 != socket.nlink()
    {
        return Err(invalid("shutdown socket identity changed after death probe"));
    }
    // SAFETY: unlink is relative to the pinned owned directory and freshly fenced socket.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SocketCleanup::Removed)
}

fn listener_refused(path: &Path) -> io::Result<bool> {
    // SAFETY: socket has no borrowed pointer arguments; successful fd ownership moves below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is newly created and exclusively owned here.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: fcntl acts on our owned fd; no pointer arguments.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sockaddr_un is a C integer/byte record; zero initializes all fields/padding.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shutdown socket path exceeds Unix address bounds",
        ));
    }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        address.sun_len = std::mem::size_of_val(&address) as u8;
    }
    // SAFETY: address points to the fully initialized sockaddr_un of the supplied size.
    let result = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if result == 0 {
        return Ok(false);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ECONNREFUSED) => Ok(true),
        Some(libc::EINPROGRESS | libc::EAGAIN | libc::ENOENT) => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::os::unix::net::UnixListener;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = Path::new("/tmp").join(format!(
                "ta-shutdown-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn socket(&self) -> std::path::PathBuf {
            self.0.join("owned.sock")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn shutdown_socket_removes_dead_inode_and_repeated_cleanup_is_absent() {
        let fixture = Fixture::new();
        let path = fixture.socket();
        drop(UnixListener::bind(&path).unwrap());
        assert_eq!(remove_dead_socket(&path).unwrap(), SocketCleanup::Removed);
        assert!(!path.exists());
        assert_eq!(remove_dead_socket(&path).unwrap(), SocketCleanup::Absent);
    }

    #[test]
    fn shutdown_socket_preserves_live_listener_and_unsafe_objects() {
        let fixture = Fixture::new();
        let path = fixture.socket();
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(remove_dead_socket(&path).unwrap(), SocketCleanup::Preserved);
        assert!(path.exists());
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"not a socket").unwrap();
        assert!(remove_dead_socket(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(remove_dead_socket(&path).is_err());
        std::fs::remove_dir(&path).unwrap();
        let target = fixture.0.join("target.sock");
        drop(UnixListener::bind(&target).unwrap());
        symlink(&target, &path).unwrap();
        assert!(remove_dead_socket(&path).is_err());
        assert!(target.exists());
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&target, &path).unwrap();
        assert!(remove_dead_socket(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(remove_dead_socket(&target).is_err());
        assert!(target.exists());
    }

    #[test]
    fn shutdown_socket_refuses_failed_probe_and_inode_or_parent_replacement() {
        let fixture = Fixture::new();
        let path = fixture.socket();
        drop(UnixListener::bind(&path).unwrap());
        assert!(remove_with_probe(&path, |_| Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "probe failed"
        )))
        .is_err());
        assert!(path.exists());
        let original = fixture.0.join("original.sock");
        assert!(remove_with_probe(&path, |_| {
            std::fs::rename(&path, &original)?;
            drop(UnixListener::bind(&path)?);
            Ok(true)
        })
        .is_err());
        assert!(path.exists());
        assert_ne!(
            std::fs::symlink_metadata(&path).unwrap().ino(),
            std::fs::symlink_metadata(&original).unwrap().ino()
        );
        std::fs::remove_file(&path).unwrap();
        drop(UnixListener::bind(&path).unwrap());
        assert!(remove_with_probe(&path, |_| {
            std::fs::remove_file(&path)?;
            std::fs::write(&path, b"replacement")?;
            Ok(true)
        })
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        std::fs::remove_file(&path).unwrap();
        drop(UnixListener::bind(&path).unwrap());
        let moved = fixture.0.with_extension("old");
        assert!(remove_with_probe(&path, |_| {
            std::fs::rename(&fixture.0, &moved)?;
            std::fs::create_dir(&fixture.0)?;
            std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700))?;
            Ok(true)
        })
        .is_err());
        assert!(moved.join("owned.sock").exists());
        std::fs::remove_dir_all(moved).unwrap();
    }
}
