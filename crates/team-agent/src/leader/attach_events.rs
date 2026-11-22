//! Wake an attached launcher on wrapper EOF, attach-client exit or SIGTERM.
//! No timer, tmux watcher process, or signal-handler filesystem work.
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt, net::UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

pub(super) struct WrapperPipe {
    pub path: PathBuf,
    reader: File,
    // Do not see EOF before the tmux shell actually opens the write end.
    bootstrap: Option<File>,
    // Darwin does not report FIFO last-writer close through poll(2).
    #[cfg(target_os = "macos")]
    pane_exit: Option<std::os::fd::OwnedFd>,
}

impl WrapperPipe {
    pub fn new(workspace: &Path) -> io::Result<Self> {
        let dir = crate::model::paths::runtime_dir(workspace);
        std::fs::create_dir_all(&dir)?;
        let nonce = crate::station::seat_store::random_instance().map_err(io::Error::other)?;
        let path = dir.join(format!("attach-{nonce}.fifo"));
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: NUL-terminated path; mkfifo refuses an existing entry.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let open = || -> io::Result<_> {
            let reader = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(&path)?;
            let writer = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(&path)?;
            Ok((reader, writer))
        };
        let (reader, writer) = open().inspect_err(|_| {
            let _ = std::fs::remove_file(&path);
        })?;
        Ok(Self {
            path,
            reader,
            bootstrap: Some(writer),
            #[cfg(target_os = "macos")]
            pane_exit: None,
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn watch_pane(&mut self, _pid: u32) -> io::Result<()> {
        // poll reports POLLHUP on our wrapper FIFO when the writer disappears.
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn watch_pane(&mut self, pid: u32) -> io::Result<()> {
        use std::os::fd::{FromRawFd, OwnedFd};
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: kqueue created a new descriptor; the event contains no user pointer.
        let queue = unsafe { OwnedFd::from_raw_fd(fd) };
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if unsafe { libc::kevent(fd, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
            let e = io::Error::last_os_error();
            // It may already have exited: the caller takes a health snapshot after arming.
            if e.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(e);
        }
        self.pane_exit = Some(queue);
        Ok(())
    }
}
impl Drop for WrapperPipe {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// Permanent pair: an in-flight signal handler can never write to a reused fd.
static WAKE: OnceLock<(UnixStream, UnixStream)> = OnceLock::new();
static TERM: AtomicBool = AtomicBool::new(false);
extern "C" fn signal(sig: libc::c_int) {
    if sig == libc::SIGTERM {
        TERM.store(true, Ordering::Relaxed);
    }
    if let Some((_, tx)) = WAKE.get() {
        // SAFETY: write is async-signal-safe. If full, a wake is already pending.
        unsafe {
            #[cfg(target_os = "macos")]
            let errno = libc::__error();
            #[cfg(target_os = "linux")]
            let errno = libc::__errno_location();
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            let saved = *errno;
            libc::write(tx.as_raw_fd(), b"!".as_ptr().cast(), 1);
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                *errno = saved;
            }
        }
    }
}

pub(super) struct Signals {
    old_term: libc::sighandler_t,
    old_chld: libc::sighandler_t,
}
impl Signals {
    pub fn install() -> io::Result<Self> {
        if WAKE.get().is_none() {
            let pair = UnixStream::pair()?;
            pair.0.set_nonblocking(true)?;
            pair.1.set_nonblocking(true)?;
            let _ = WAKE.set(pair);
        }
        TERM.store(false, Ordering::Relaxed);
        // These handlers are limited to the managed launcher wait; do not reap in them.
        unsafe {
            let handler = signal as *const () as libc::sighandler_t;
            let old_term = libc::signal(libc::SIGTERM, handler);
            if old_term == libc::SIG_ERR {
                return Err(io::Error::last_os_error());
            }
            let old_chld = libc::signal(libc::SIGCHLD, handler);
            if old_chld == libc::SIG_ERR {
                let err = io::Error::last_os_error();
                libc::signal(libc::SIGTERM, old_term);
                return Err(err);
            }
            Ok(Self { old_term, old_chld })
        }
    }
    pub fn terminated(&self) -> bool {
        TERM.load(Ordering::Relaxed)
    }

    /// True when the provider wrapper closed its descriptor. Otherwise the
    /// caller checks SIGTERM and reaps its own (and only its own) attach child.
    pub fn wait(&self, pipe: &mut WrapperPipe) -> io::Result<bool> {
        let mut rx = &WAKE.get().unwrap().0;
        let fd = |fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        #[cfg(target_os = "macos")]
        let procfd = pipe.pane_exit.as_ref().map_or(-1, AsRawFd::as_raw_fd);
        #[cfg(not(target_os = "macos"))]
        let procfd = -1;
        let mut fds = [fd(rx.as_raw_fd()), fd(pipe.reader.as_raw_fd()), fd(procfd)];
        // SAFETY: all descriptors remain live; -1 is an infinite timeout.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) } < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(e)
            };
        }
        if fds[2].revents != 0 {
            return Ok(true);
        }
        let mut bytes = [0; 128];
        if fds[0].revents != 0 {
            rx.read(&mut bytes)?;
        }
        if fds[1].revents != 0 {
            let n = pipe.reader.read(&mut bytes)?;
            if n == 0 || bytes[..n].contains(&b'E') {
                return Ok(true);
            }
            if pipe.bootstrap.take().is_some() {
                // The shell owns its fd now. No named artifact even if a client is killed.
                let _ = std::fs::remove_file(&pipe.path);
            }
        }
        Ok(false)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        unsafe {
            libc::signal(libc::SIGTERM, self.old_term);
            libc::signal(libc::SIGCHLD, self.old_chld);
        }
    }
}
