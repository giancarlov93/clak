//! The client side of the control channel, synchronous: used by the command line, shim and
//! panel. A named pipe on Windows, a Unix socket on the Mac; line-delimited JSON.

use std::io::{self, Read, Write};
use std::time::Duration;

/// A connection to the control channel.
pub struct Stream {
    #[cfg(windows)]
    inner: std::fs::File,
    #[cfg(unix)]
    inner: std::os::unix::net::UnixStream,
}

impl Stream {
    /// A second handle on the same connection: one for reading, one for writing.
    pub fn try_clone(&self) -> io::Result<Stream> {
        Ok(Stream { inner: self.inner.try_clone()? })
    }

    /// Stops waiting: a pending read ends with an error. On the Mac it shuts the socket down
    /// in both directions; on Windows it cancels the handle's operations.
    pub fn shutdown(&self) {
        #[cfg(unix)]
        {
            let _ = self.inner.shutdown(std::net::Shutdown::Both);
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: the handle lives as long as `self`.
            unsafe {
                windows_sys::Win32::System::IO::CancelIoEx(self.inner.as_raw_handle() as _, std::ptr::null());
            }
        }
    }

    /// A limit on the next read (Mac only: on Windows the pipe has none).
    pub fn set_read_timeout(&self, timeout: Option<Duration>) {
        #[cfg(unix)]
        {
            let _ = self.inner.set_read_timeout(timeout);
        }
        #[cfg(windows)]
        let _ = timeout;
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Connects to the control channel, waiting at most `wait` if it is busy.
/// An error means the hub is not there (or not answering).
pub fn connect(control: &str, wait: Duration) -> io::Result<Stream> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
        use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
        let started = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(control) {
                Ok(file) => return Ok(Stream { inner: file }),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    let left = wait.saturating_sub(started.elapsed());
                    if left.is_zero() {
                        return Err(e);
                    }
                    let wide: Vec<u16> = std::ffi::OsStr::new(control).encode_wide().chain(Some(0)).collect();
                    // SAFETY: nul-terminated text.
                    unsafe { WaitNamedPipeW(wide.as_ptr(), left.as_millis().min(u32::MAX as u128) as u32) };
                }
                Err(e) => return Err(e),
            }
        }
    }
    #[cfg(unix)]
    {
        let _ = wait;
        let inner = std::os::unix::net::UnixStream::connect(control)?;
        Ok(Stream { inner })
    }
}
