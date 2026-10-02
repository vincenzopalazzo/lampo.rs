//! CLN hsmd wire transport to `remote_hsmd_socket`.
//!
//! The proxy is a `lightningd` `hsmd` drop-in: it speaks the hsmd protocol on
//! file descriptor 3 inherited from its parent. We spawn it with one end of a
//! socketpair on that fd and keep the other end as the *root* connection
//! (node-level messages). Channel-level messages travel on a dedicated fd per
//! channel, obtained with `ClientHsmFd` and passed back over `SCM_RIGHTS`.
//!
//! Framing on every fd is `u32 BE length || u16 BE type || body`. One request
//! is in flight per fd at a time, guarded by a mutex; LDK's signer traits are
//! synchronous, so blocking here is the intended shape.
use std::fmt;
use std::io::{self, Read, Write};
use std::mem;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use vls_protocol::model::PubKey;
use vls_protocol::msgs::{self, DeBolt, SerBolt};

/// Mirrors `MAX_MESSAGE_SIZE` in `vls-protocol`.
const MAX_MESSAGE_SIZE: u32 = 128 * 1024;
/// The fd `remote_hsmd_socket` expects its parent on.
const PARENT_FD: RawFd = 3;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Protocol(vls_protocol::Error),
    /// The signer rejected the request, typically a policy violation.
    Signer {
        code: u16,
        message: String,
    },
    Message(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(err) => write!(f, "hsmd transport: {err}"),
            Error::Protocol(err) => write!(f, "hsmd protocol: {err:?}"),
            Error::Signer { code, message } => write!(f, "signer error {code}: {message}"),
            Error::Message(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<vls_protocol::Error> for Error {
    fn from(err: vls_protocol::Error) -> Self {
        Error::Protocol(err)
    }
}

/// One hsmd connection: the root fd or a per-channel fd.
pub struct Conn {
    stream: Mutex<UnixStream>,
}

impl Conn {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream: Mutex::new(stream),
        }
    }

    /// Send a typed request and decode its typed reply.
    pub fn call<T: SerBolt, R: DeBolt>(&self, msg: T) -> Result<R, Error> {
        log::trace!(target: "lampo-vls", "-> {msg:?}");
        let reply = self.call_raw(msg.as_vec())?;
        let reply = R::from_vec(reply)?;
        log::trace!(target: "lampo-vls", "<- {reply:?}");
        Ok(reply)
    }

    pub fn call_raw(&self, msg: Vec<u8>) -> Result<Vec<u8>, Error> {
        let mut stream = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        write_frame(&mut stream, &msg)?;
        let reply = read_frame(&mut stream)?;
        check_signer_error(&reply)?;
        Ok(reply)
    }

    /// Ask the proxy for a channel fd. Must run on the root connection.
    /// The reply and the fd-carrying message follow each other on the same
    /// socket, so both are read under the one lock.
    pub fn open_channel(&self, peer_id: PubKey, dbid: u64) -> Result<Conn, Error> {
        let mut stream = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        let request = msgs::ClientHsmFd {
            peer_id,
            dbid,
            capabilities: 0,
        };
        write_frame(&mut stream, &request.as_vec())?;
        let reply = read_frame(&mut stream)?;
        check_signer_error(&reply)?;
        msgs::ClientHsmFdReply::from_vec(reply)?;
        let fd = recv_fd(&stream)?;
        // SAFETY: the fd was just handed to us by the kernel via SCM_RIGHTS
        // and nothing else owns it.
        let channel = unsafe { UnixStream::from_raw_fd(fd) };
        Ok(Conn::new(channel))
    }
}

fn write_frame(stream: &mut UnixStream, msg: &[u8]) -> io::Result<()> {
    let len = u32::try_from(msg.len())
        .ok()
        .filter(|len| *len <= MAX_MESSAGE_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hsmd message too large"))?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(msg)?;
    stream.flush()
}

fn read_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len);
    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("hsmd reply of {len} bytes exceeds the {MAX_MESSAGE_SIZE} byte limit"),
        ));
    }
    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body)?;
    Ok(body)
}

/// `SignerError` replies share the frame format of every other reply, so a
/// typed decode would fail with an unhelpful type mismatch. Surface them.
fn check_signer_error(reply: &[u8]) -> Result<(), Error> {
    if reply.len() < 2 {
        return Err(Error::Message("hsmd reply shorter than a type id".into()));
    }
    let ty = u16::from_be_bytes([reply[0], reply[1]]);
    if ty == msgs::SignerError::TYPE {
        let err = msgs::SignerError::from_vec(reply.to_vec())?;
        return Err(Error::Signer {
            code: err.code,
            message: String::from_utf8_lossy(&err.message.0).into_owned(),
        });
    }
    Ok(())
}

/// Receive one fd over `SCM_RIGHTS`. The proxy sends a single `0xff` byte
/// as the payload next to the control message (`vls-proxy/src/connection.rs`).
fn recv_fd(stream: &UnixStream) -> io::Result<RawFd> {
    let mut payload = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    // SAFETY: CMSG_SPACE is a pure size computation.
    let space = unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) } as usize;
    // u64 storage keeps the control buffer suitably aligned for cmsghdr.
    let mut control = vec![0u64; space.div_ceil(mem::size_of::<u64>())];

    // SAFETY: msghdr is plain data; every pointer stays valid for the call.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = space as _;

    // SAFETY: `msg` is fully initialized and the fd is a live socket.
    let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
    if received < 0 {
        return Err(io::Error::last_os_error());
    }
    if received == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "hsmd proxy closed while passing a channel fd",
        ));
    }
    if payload[0] != 0xff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected ancillary payload byte {:#x}", payload[0]),
        ));
    }
    // SAFETY: CMSG_FIRSTHDR only reads `msg`; the header is checked for null.
    let header = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if header.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "hsmd proxy sent no control message with the channel fd",
        ));
    }
    // SAFETY: `header` points into `control`, which outlives this read.
    let (level, ty) = unsafe { ((*header).cmsg_level, (*header).cmsg_type) };
    if level != libc::SOL_SOCKET || ty != libc::SCM_RIGHTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected control message level {level} type {ty}"),
        ));
    }
    // SAFETY: an SCM_RIGHTS message of this size carries exactly one fd.
    let fd = unsafe { *(libc::CMSG_DATA(header) as *const RawFd) };
    Ok(fd)
}

/// A spawned `remote_hsmd_socket` and the root connection to it.
pub struct Proxy {
    child: Mutex<Child>,
    root: Conn,
}

impl Proxy {
    /// Spawn the proxy with one end of a socketpair on its fd 3 and the
    /// given environment (`VLS_NETWORK`, `BITCOIND_RPC_URL`, `VLS_PORT`, ...).
    /// `datadir` receives the proxy's own log files.
    pub fn spawn(bin: &Path, datadir: &Path, envs: &[(&str, String)]) -> Result<Self, Error> {
        std::fs::create_dir_all(datadir)?;
        let (parent_end, child_end) = UnixStream::pair()?;
        let child_fd = child_end.into_raw_fd();

        let mut cmd = Command::new(bin);
        // The proxy's default TXOO source writes into its working directory.
        cmd.current_dir(datadir)
            .arg("--datadir")
            .arg(datadir)
            .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
            .stdin(Stdio::null());
        // SAFETY: the closure only calls async-signal-safe libc functions.
        unsafe {
            cmd.pre_exec(move || {
                if child_fd != PARENT_FD && libc::dup2(child_fd, PARENT_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // dup2 clears FD_CLOEXEC on the target; a pair end that already
                // sits on fd 3 keeps its close-on-exec flag, so clear it too.
                if libc::fcntl(PARENT_FD, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn();
        // SAFETY: we own child_fd; the child got its own copy at exec.
        unsafe {
            libc::close(child_fd);
        }
        let child = child?;
        log::info!(target: "lampo-vls", "spawned {} (pid {})", bin.display(), child.id());
        Ok(Self {
            child: Mutex::new(child),
            root: Conn::new(parent_end),
        })
    }

    pub fn root(&self) -> &Conn {
        &self.root
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = child.kill() {
            log::debug!(target: "lampo-vls", "hsmd proxy already gone: {err}");
        }
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use super::{read_frame, write_frame, MAX_MESSAGE_SIZE};

    #[test]
    fn frames_round_trip_with_a_length_prefix() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        write_frame(&mut a, &[0x04, 0x0c, 1, 2, 3]).unwrap();
        let mut raw = [0u8; 9];
        b.read_exact(&mut raw).unwrap();
        assert_eq!(raw, [0, 0, 0, 5, 0x04, 0x0c, 1, 2, 3]);

        b.write_all(&[0, 0, 0, 2, 0xab, 0xcd]).unwrap();
        assert_eq!(read_frame(&mut a).unwrap(), vec![0xab, 0xcd]);
    }

    #[test]
    fn oversized_frames_are_rejected() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        b.write_all(&(MAX_MESSAGE_SIZE + 1).to_be_bytes()).unwrap();
        assert!(read_frame(&mut a).is_err());
        let big = vec![0u8; MAX_MESSAGE_SIZE as usize + 1];
        assert!(write_frame(&mut a, &big).is_err());
    }
}
