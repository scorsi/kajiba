//! A single marker byte carries SCM_RIGHTS; bounded length-prefixed text follows it.
use std::{
    io,
    mem::{size_of, size_of_val},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, Interest},
    net::UnixStream,
};

pub async fn send_fds(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    stream
        .async_io(Interest::WRITABLE, || {
            let mut marker = [0_u8];
            let mut iov = libc::iovec {
                iov_base: marker.as_mut_ptr().cast(),
                iov_len: 1,
            };
            let mut control = [0_usize; 8];
            // SAFETY: msghdr and its aligned ancillary storage are initialized; every pointer
            // remains valid for sendmsg, which copies descriptors without taking ownership.
            unsafe {
                let space = libc::CMSG_SPACE(size_of_val(fds) as _) as usize;
                if space > size_of_val(&control) {
                    return Err(io::Error::other("too many descriptors"));
                }
                let mut msg: libc::msghdr = std::mem::zeroed();
                msg.msg_iov = &mut iov;
                msg.msg_iovlen = 1;
                msg.msg_control = control.as_mut_ptr().cast();
                msg.msg_controllen = space as _;
                let cmsg = libc::CMSG_FIRSTHDR(&msg);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(size_of_val(fds) as _) as _;
                std::ptr::copy_nonoverlapping(
                    fds.as_ptr(),
                    libc::CMSG_DATA(cmsg).cast(),
                    fds.len(),
                );
                #[cfg(target_os = "linux")]
                let flags = libc::MSG_NOSIGNAL;
                #[cfg(not(target_os = "linux"))]
                let flags = 0;
                match libc::sendmsg(stream.as_raw_fd(), &msg, flags) {
                    1 => Ok(()),
                    -1 => Err(io::Error::last_os_error()),
                    _ => Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "descriptor hand-off failed",
                    )),
                }
            }
        })
        .await
}

pub async fn receive_fds(stream: &UnixStream) -> io::Result<[OwnedFd; 3]> {
    stream
        .async_io(Interest::READABLE, || {
            let mut marker = [0_u8];
            let mut iov = libc::iovec {
                iov_base: marker.as_mut_ptr().cast(),
                iov_len: 1,
            };
            let mut control = [0_usize; 8];
            let mut fds = Vec::new();
            // SAFETY: recvmsg writes within the supplied aligned storage. Each SCM_RIGHTS
            // descriptor is newly owned by us and immediately wrapped so all errors close it.
            unsafe {
                let mut msg: libc::msghdr = std::mem::zeroed();
                msg.msg_iov = &mut iov;
                msg.msg_iovlen = 1;
                msg.msg_control = control.as_mut_ptr().cast();
                msg.msg_controllen = size_of_val(&control) as _;
                #[cfg(target_os = "linux")]
                let flags = libc::MSG_CMSG_CLOEXEC;
                #[cfg(not(target_os = "linux"))]
                let flags = 0;
                let count = libc::recvmsg(stream.as_raw_fd(), &mut msg, flags);
                if count < 0 {
                    return Err(io::Error::last_os_error());
                }
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "missing descriptors",
                    ));
                }
                let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
                while !cmsg.is_null() {
                    if (*cmsg).cmsg_level == libc::SOL_SOCKET
                        && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                    {
                        let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                            / size_of::<RawFd>();
                        for i in 0..count {
                            fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
                                libc::CMSG_DATA(cmsg).cast::<RawFd>().add(i),
                            )));
                        }
                    }
                    cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
                }
                if msg.msg_flags & libc::MSG_CTRUNC != 0 || marker != [0] {
                    return Err(io::Error::other("invalid descriptor hand-off"));
                }
                for fd in &fds {
                    if libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            fds.try_into()
                .map_err(|_| io::Error::other("exactly three descriptors required"))
        })
        .await
}

pub async fn write_text(stream: &mut UnixStream, text: &str) -> io::Result<()> {
    let len =
        u16::try_from(text.len()).map_err(|_| io::Error::other("hand-off message too long"))?;
    stream.write_u16(len).await?;
    stream.write_all(text.as_bytes()).await
}

pub async fn read_text(stream: &mut UnixStream, limit: usize) -> io::Result<String> {
    let len = usize::from(stream.read_u16().await?);
    if len > limit {
        return Err(io::Error::other("hand-off message too long"));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    String::from_utf8(bytes).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        io::{Read, Write},
        os::unix::net::UnixStream as BlockingStream,
    };

    #[tokio::test]
    async fn descriptors_are_owned_cloexec_and_keep_working_after_sender_closes() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let mut peers = Vec::new();
        let mut passed = Vec::new();
        for _ in 0..3 {
            let (a, b) = BlockingStream::pair().unwrap();
            peers.push(a);
            passed.push(b);
        }
        send_fds(
            &sender,
            &passed.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        write_text(&mut sender, "request").await.unwrap();
        let received = receive_fds(&receiver).await.unwrap();
        assert_eq!(read_text(&mut receiver, 167).await.unwrap(), "request");
        drop(passed);
        drop(sender);
        drop(receiver);
        for (i, fd) in received.into_iter().enumerate() {
            // SAFETY: F_GETFD reads flags of a live descriptor.
            assert_ne!(
                unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
            let mut file = File::from(fd);
            peers[i].write_all(b"input").unwrap();
            let mut bytes = [0; 5];
            file.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"input");
            file.write_all(b"output").unwrap();
            let mut bytes = [0; 6];
            peers[i].read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"output");
            drop(file);
            assert_eq!(peers[i].read(&mut bytes).unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn malformed_handoffs_close_received_descriptors_and_refuse_frames() {
        for count in [1, 2, 4, 12] {
            let (sender, receiver) = UnixStream::pair().unwrap();
            let (peer, passed) = BlockingStream::pair().unwrap();
            send_fds(&sender, &vec![passed.as_raw_fd(); count])
                .await
                .unwrap();
            assert!(receive_fds(&receiver).await.is_err());
            drop(passed);
            peer.set_nonblocking(true).unwrap();
            let mut peer = UnixStream::from_std(peer).unwrap();
            // Parallel tests may fork with a copy of the descriptors until their child execs.
            let closed =
                tokio::time::timeout(std::time::Duration::from_secs(5), peer.read(&mut [0]))
                    .await
                    .unwrap_or_else(|_| panic!("leaked descriptor for count {count}"))
                    .unwrap();
            assert_eq!(closed, 0, "leaked descriptor for count {count}");
        }
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_u16(168).await.unwrap();
        assert!(read_text(&mut receiver, 167).await.is_err());
        drop(sender);
        assert!(receive_fds(&receiver).await.is_err());
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender.write_all(&[0]).await.unwrap();
        assert!(receive_fds(&receiver).await.is_err());
    }
}
