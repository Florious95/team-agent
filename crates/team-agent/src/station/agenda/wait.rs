//! Wait for input/deadlines and storage-scope loss without periodic filesystem polling.
use super::Store;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

pub(super) struct Wait {
    fd: OwnedFd,
    #[cfg(target_os = "macos")]
    _directories: Vec<std::fs::File>,
}

fn owned(fd: i32) -> Result<OwnedFd, String> {
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: the successful syscall returned a fresh descriptor owned by this instance.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl Wait {
    pub(super) fn new(store: &Store, socket: &UnixDatagram) -> Result<Self, String> {
        let paths = [
            store.workspace.clone(),
            store.workspace.join(".team"),
            store.workspace.join(".team/agenda"),
            store.root.clone(),
        ];
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // SAFETY: kqueue takes no pointers and returns an owned descriptor.
            let fd = owned(unsafe { libc::kqueue() })?;
            let directories = paths
                .iter()
                .map(|path| {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                        .open(path)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            let event = |ident, filter, fflags| libc::kevent {
                ident,
                filter,
                flags: libc::EV_ADD,
                fflags,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let mut changes = vec![event(socket.as_raw_fd() as usize, libc::EVFILT_READ, 0)];
            changes.extend(directories.iter().map(|directory| {
                event(
                    directory.as_raw_fd() as usize,
                    libc::EVFILT_VNODE,
                    libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE,
                )
            }));
            // SAFETY: all descriptors are live and changes points to the specified array.
            let result = unsafe {
                libc::kevent(
                    fd.as_raw_fd(),
                    changes.as_ptr(),
                    changes.len() as i32,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if result < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(Self {
                fd,
                _directories: directories,
            })
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt;
            let _ = socket;
            // SAFETY: inotify_init1 takes flags only and returns a fresh descriptor.
            let fd = owned(unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) })?;
            for path in paths {
                let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                    .map_err(|e| e.to_string())?;
                // SAFETY: path is NUL-terminated and fd remains owned/live.
                let result = unsafe {
                    libc::inotify_add_watch(
                        fd.as_raw_fd(),
                        path.as_ptr(),
                        libc::IN_MOVE_SELF
                            | libc::IN_DELETE_SELF
                            | libc::IN_UNMOUNT
                            | libc::IN_ONLYDIR
                            | libc::IN_DONT_FOLLOW,
                    )
                };
                if result < 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
            Ok(Self { fd })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (paths, socket);
            Err("agenda lifetime watching is unsupported on this platform".into())
        }
    }

    /// False means an ancestor was removed, moved or revoked; the old scope must not execute.
    pub(super) fn ready(
        &self,
        socket: &UnixDatagram,
        timeout: Option<Duration>,
    ) -> Result<bool, String> {
        #[cfg(target_os = "macos")]
        {
            let _ = socket;
            let timeout = timeout.map(|duration| libc::timespec {
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos().into(),
            });
            // SAFETY: zero is valid for every kevent field; kevent writes at most five entries.
            let mut events: [libc::kevent; 5] = unsafe { std::mem::zeroed() };
            let count = unsafe {
                libc::kevent(
                    self.fd.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    timeout.as_ref().map_or(std::ptr::null(), |value| value),
                )
            };
            if count < 0 {
                return interrupted();
            }
            Ok(!events[..count as usize]
                .iter()
                .any(|event| event.filter == libc::EVFILT_VNODE))
        }
        #[cfg(target_os = "linux")]
        {
            let mut fds = [
                libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let timeout = timeout.map_or(-1, |duration| {
                duration.as_millis().min(i32::MAX as u128) as i32
            });
            // SAFETY: fds points to exactly two initialized, live descriptors.
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
                return interrupted();
            }
            Ok(fds[1].revents == 0)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (socket, timeout);
            Err("agenda lifetime watching is unsupported on this platform".into())
        }
    }
}

fn interrupted() -> Result<bool, String> {
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::Interrupted {
        Ok(true)
    } else {
        Err(error.to_string())
    }
}
