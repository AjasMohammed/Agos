//! Lightweight systemd service-notification helpers.
//!
//! Sends `sd_notify(3)` messages to systemd when `NOTIFY_SOCKET` is present
//! in the environment.  Does nothing when the variable is absent (direct
//! invocation, Docker, tests, CI).
//!
//! Messages used:
//! - `READY=1`    — sent once after the kernel is fully initialised.
//! - `WATCHDOG=1` — sent on every successful health-check cycle so systemd
//!   can detect hangs (not just crashes) and restart the process.
//! - `STOPPING=1` — sent at the start of graceful shutdown.

use std::os::unix::net::UnixDatagram;

/// Signal to systemd that the service has finished starting and is ready to
/// accept connections.  Call once after the bus, health server, and all
/// subsystems are running.
pub fn notify_ready() {
    send("READY=1\n");
}

/// Ping the systemd watchdog.  Call after every successful health-check cycle.
/// If no ping arrives within `WatchdogSec`, systemd kills and restarts the
/// process.
pub fn notify_watchdog() {
    send("WATCHDOG=1\n");
}

/// Signal to systemd that the service is beginning a graceful shutdown.
/// Calling this gives systemd precise timing information and prevents it from
/// sending SIGKILL before the `TimeoutStopSec` window expires.
pub fn notify_stopping() {
    send("STOPPING=1\n");
}

/// Write `msg` to `$NOTIFY_SOCKET`.  Silently no-ops when the socket is not
/// configured or any I/O error occurs — notification failures must never crash
/// the kernel.
pub(crate) fn send(msg: &str) {
    let socket_path = match std::env::var("NOTIFY_SOCKET") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };

    if let Some(abstract_path) = socket_path.strip_prefix('@') {
        send_abstract(abstract_path, msg);
    } else if let Ok(sock) = UnixDatagram::unbound() {
        let _ = sock.send_to(msg.as_bytes(), &socket_path);
    }
}

/// Send `msg` to an abstract-namespace Unix datagram socket.
/// The `@` prefix has already been stripped from `path`.
///
/// Abstract-namespace sockets are not accessible via the filesystem; the
/// address is a NUL byte followed by the name.  `UnixDatagram::send_to`
/// cannot handle this because it treats the path as a NUL-terminated string,
/// so we fall through to a raw `sendto(2)` via libc.
fn send_abstract(path: &str, msg: &str) {
    use std::os::unix::io::AsRawFd;

    let sock = match UnixDatagram::unbound() {
        Ok(s) => s,
        Err(_) => return,
    };

    let path_bytes = path.as_bytes();
    // sun_path is 108 bytes; we need 1 (NUL prefix) + path length to fit.
    if path_bytes.len() >= 107 {
        return;
    }

    // SAFETY: sockaddr_un is a plain C struct; zero-initialisation is valid.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // Abstract socket address: first byte of sun_path is '\0', rest is the name.
    for (i, &b) in path_bytes.iter().enumerate() {
        addr.sun_path[i + 1] = b as libc::c_char;
    }

    // Use offset_of! (stable since Rust 1.77) for portable addrlen calculation.
    // addrlen = offset_of(sun_path) + 1 (NUL prefix byte) + name length.
    let addrlen = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + 1 + path_bytes.len())
        as libc::socklen_t;

    // SAFETY: addr is correctly initialised above; msg pointer and len are valid.
    unsafe {
        libc::sendto(
            sock.as_raw_fd(),
            msg.as_ptr() as *const libc::c_void,
            msg.len(),
            libc::MSG_NOSIGNAL,
            &addr as *const _ as *const libc::sockaddr,
            addrlen,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // NOTIFY_SOCKET is a process-wide env var; serialize tests that touch it
    // to prevent races when cargo runs tests in parallel threads.
    use serial_test::serial;

    #[test]
    #[serial]
    fn noop_without_notify_socket() {
        std::env::remove_var("NOTIFY_SOCKET");
        // Must not panic.
        notify_ready();
        notify_watchdog();
        notify_stopping();
    }

    #[test]
    #[serial]
    fn noop_on_empty_notify_socket() {
        std::env::set_var("NOTIFY_SOCKET", "");
        notify_ready();
        std::env::remove_var("NOTIFY_SOCKET");
    }

    #[test]
    #[serial]
    fn path_socket_delivers_ready_message() {
        use std::os::unix::net::UnixDatagram;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        // Remove the file so we can bind a datagram socket at the path.
        drop(tmp);

        let receiver = UnixDatagram::bind(&path).unwrap();
        std::env::set_var("NOTIFY_SOCKET", &path);

        notify_ready();

        let mut buf = [0u8; 64];
        receiver.set_nonblocking(true).unwrap();
        let n = receiver.recv(&mut buf).expect("should receive datagram");
        let received = std::str::from_utf8(&buf[..n]).unwrap();
        assert_eq!(received, "READY=1\n");

        std::env::remove_var("NOTIFY_SOCKET");
        let _ = std::fs::remove_file(&path);
    }
}
