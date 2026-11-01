//! systemd's listening descriptors, consumed once before starting child processes.
use std::{
    collections::BTreeMap,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

#[derive(Default)]
pub struct Listeners(BTreeMap<String, OwnedFd>);

fn names(
    pid: Option<&str>,
    count: Option<&str>,
    names: Option<&str>,
    expected: &[&str],
    current: u32,
) -> io::Result<Vec<String>> {
    let Some(pid) = pid else {
        return Ok(Vec::new());
    };
    if pid.parse::<u32>().map_err(io::Error::other)? != current {
        return Ok(Vec::new());
    }
    let count = count
        .unwrap_or("0")
        .parse::<i32>()
        .map_err(io::Error::other)?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if count < 0 || count as usize != expected.len() {
        return Err(io::Error::other("unexpected LISTEN_FDS count"));
    }
    let names: Vec<String> = match names {
        Some(names) => names.split(':').map(str::to_owned).collect(),
        None => expected.iter().map(|name| (*name).to_owned()).collect(),
    };
    if names.len() != count as usize
        || expected.iter().any(|expected| {
            names
                .iter()
                .filter(|name| name.as_str() == *expected)
                .count()
                != 1
        })
    {
        return Err(io::Error::other("unexpected or duplicate LISTEN_FDNAMES"));
    }
    Ok(names)
}

impl Listeners {
    pub fn inherit(expected: &[&str]) -> io::Result<Self> {
        let parsed = names(
            std::env::var("LISTEN_PID").ok().as_deref(),
            std::env::var("LISTEN_FDS").ok().as_deref(),
            std::env::var("LISTEN_FDNAMES").ok().as_deref(),
            expected,
            std::process::id(),
        );
        // Do not pass activation metadata to hooks or other children.
        for key in ["LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES"] {
            std::env::remove_var(key);
        }
        let mut listeners = BTreeMap::new();
        for (index, name) in parsed?.into_iter().enumerate() {
            let fd = 3 + index as i32;
            // SAFETY: fcntl operates on a live descriptor and these commands take integers.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fcntl validated this live descriptor; systemd transferred ownership.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let mut accepting: libc::c_int = 0;
            let mut len = std::mem::size_of_val(&accepting) as libc::socklen_t;
            // SAFETY: both output pointers reference writable storage of the stated size.
            if unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ACCEPTCONN,
                    (&mut accepting as *mut libc::c_int).cast(),
                    &mut len,
                )
            } < 0
                || accepting != 1
            {
                return Err(io::Error::other(
                    "inherited descriptor is not a listening socket",
                ));
            }
            listeners.insert(name, fd);
        }
        Ok(Self(listeners))
    }

    pub async fn tcp(
        &mut self,
        name: &str,
        addr: std::net::SocketAddr,
    ) -> io::Result<tokio::net::TcpListener> {
        let Some(fd) = self.0.remove(name) else {
            return tokio::net::TcpListener::bind(addr).await;
        };
        let listener = std::net::TcpListener::from(fd);
        if listener.local_addr()? != addr {
            return Err(io::Error::other(
                "inherited TCP address differs from configuration",
            ));
        }
        listener.set_nonblocking(true)?;
        tokio::net::TcpListener::from_std(listener)
    }

    pub fn unix(
        &mut self,
        name: &str,
        path: &std::path::Path,
    ) -> io::Result<Option<tokio::net::UnixListener>> {
        let Some(fd) = self.0.remove(name) else {
            return Ok(None);
        };
        let listener = std::os::unix::net::UnixListener::from(fd);
        if listener.local_addr()?.as_pathname() != Some(path) {
            return Err(io::Error::other(
                "inherited Unix address differs from configuration",
            ));
        }
        listener.set_nonblocking(true)?;
        tokio::net::UnixListener::from_std(listener).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_metadata_is_pid_scoped_named_or_positional() {
        let expected = ["http", "runner"];
        assert!(
            names(None, Some("bad"), None, &expected, 42)
                .unwrap()
                .is_empty()
        );
        assert!(
            names(Some("43"), Some("2"), None, &expected, 42)
                .unwrap()
                .is_empty()
        );
        assert!(
            names(Some("42"), None, None, &expected, 42)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            names(Some("42"), Some("2"), None, &expected, 42).unwrap(),
            expected
        );
        assert_eq!(
            names(Some("42"), Some("2"), Some("runner:http"), &expected, 42).unwrap(),
            ["runner", "http"]
        );
        assert!(names(Some("invalid"), Some("2"), None, &expected, 42).is_err());
        for (count, labels) in [
            ("-1", None),
            ("bad", None),
            ("1", None),
            ("2", Some("http:http")),
            ("2", Some("http:unknown")),
            ("2", Some("http")),
        ] {
            assert!(names(Some("42"), Some(count), labels, &expected, 42).is_err());
        }
    }
}
