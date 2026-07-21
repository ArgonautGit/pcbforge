//! WS-DRV — LightBurn UDP command client.
//!
//! LightBurn's documented "Automation with UDP" interface: ASCII datagrams are
//! sent to the app on `127.0.0.1:19840`, and it replies on the FIXED port
//! `19841` (not the source port), so the client must bind `19841` to hear the
//! answers. Every reply is one of `OK`, `!` (busy), or `?` (unknown command).
//!
//! This drives the operator's existing LightBurn install for a one-click
//! "load + run" from the console, until the native JCZ/EZCAD driver (DRV-6)
//! replaces it. Commands used: `PING`, `LASER:<name>`, `FORCELOAD:<abs path>`,
//! `START`, `STATUS`.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

/// Default LightBurn command port (where the app listens).
pub const DEFAULT_TARGET: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 19840);

/// The fixed port LightBurn sends its replies to (regardless of the command's
/// source port), so the client binds it to receive them.
pub const DEFAULT_REPLY_PORT: u16 = 19841;

/// How long a single reply is waited for before giving up.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// A LightBurn reply datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// `OK` — command accepted (and, for `STATUS`, connected + idle).
    Ok,
    /// `!` — busy (a job is running, or `STATUS` on a running machine).
    Busy,
    /// `?` — unknown command.
    Unknown,
}

/// A UDP client for LightBurn's automation interface.
///
/// Holds one socket bound to `reply_bind` (the port LightBurn answers on);
/// commands go out from it to `target` and replies come back to it.
pub struct LightburnClient {
    socket: UdpSocket,
    target: SocketAddr,
}

impl LightburnClient {
    /// Bind `reply_bind` for replies and target `target` for commands, with a
    /// [`READ_TIMEOUT`] on reads so a missing reply fails instead of blocking.
    pub fn connect(target: SocketAddr, reply_bind: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(reply_bind)?;
        socket.set_read_timeout(Some(READ_TIMEOUT))?;
        Ok(Self { socket, target })
    }

    /// `PING` — `Ok` iff the app is open with no modal dialog.
    pub fn ping(&self) -> io::Result<Reply> {
        self.command("PING")
    }

    /// `LASER:<name>` — select the configured device.
    pub fn select_laser(&self, name: &str) -> io::Result<Reply> {
        self.command(&format!("LASER:{name}"))
    }

    /// `FORCELOAD:<abs path>` — load a file, suppressing the save prompt.
    pub fn force_load(&self, path: &str) -> io::Result<Reply> {
        self.command(&format!("FORCELOAD:{path}"))
    }

    /// `START` — run the loaded job.
    pub fn start(&self) -> io::Result<Reply> {
        self.command("START")
    }

    /// `STATUS` — `Ok` = connected + idle, `Busy` = a job is running.
    pub fn status(&self) -> io::Result<Reply> {
        self.command("STATUS")
    }

    /// Send `cmd` to the target and parse the first reply from its IP. A read
    /// timeout surfaces as [`io::ErrorKind::TimedOut`] on every platform (Unix
    /// reports `WouldBlock`, Windows `TimedOut`); unexpected reply text becomes
    /// an [`io::ErrorKind::InvalidData`] carrying the text.
    fn command(&self, cmd: &str) -> io::Result<Reply> {
        self.socket.send_to(cmd.as_bytes(), self.target)?;
        let mut buf = [0u8; 512];
        loop {
            let (n, src) = match self.socket.recv_from(&mut buf) {
                Ok(v) => v,
                // Normalize the platform-specific timeout kind.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, e));
                }
                Err(e) => return Err(e),
            };
            // Ignore stray datagrams from a host other than the target.
            if src.ip() != self.target.ip() {
                continue;
            }
            let text = String::from_utf8_lossy(&buf[..n]);
            return match text.trim() {
                "OK" => Ok(Reply::Ok),
                "!" => Ok(Reply::Busy),
                "?" => Ok(Reply::Unknown),
                other => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected LightBurn reply: {other:?}"),
                )),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// A fake LightBurn: binds an ephemeral port, records each received command,
    /// and replies to the datagram's source with a caller-supplied answer per
    /// command. Runs on a std thread; both sides use ephemeral ports so tests
    /// run in parallel. The recorded commands are sent back over `cmd_tx`.
    ///
    /// `reply_for` maps a received command to the reply bytes to send (return
    /// `None` to stay silent, e.g. to force a read timeout).
    fn fake_lightburn<F>(reply_for: F) -> (SocketAddr, mpsc::Receiver<String>)
    where
        F: Fn(&str) -> Option<&'static str> + Send + 'static,
    {
        let socket = UdpSocket::bind(loopback(0)).unwrap();
        let addr = socket.local_addr().unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 512];
            loop {
                let Ok((n, src)) = socket.recv_from(&mut buf) else {
                    return;
                };
                let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                if cmd_tx.send(cmd.clone()).is_err() {
                    return;
                }
                if let Some(reply) = reply_for(&cmd) {
                    let _ = socket.send_to(reply.as_bytes(), src);
                }
            }
        });
        (addr, cmd_rx)
    }

    #[test]
    fn commands_carry_the_documented_wire_format() {
        let (addr, cmds) = fake_lightburn(|_| Some("OK"));
        let client = LightburnClient::connect(addr, loopback(0)).unwrap();

        assert_eq!(client.ping().unwrap(), Reply::Ok);
        assert_eq!(cmds.recv().unwrap(), "PING");

        assert_eq!(client.select_laser("BSLFiber").unwrap(), Reply::Ok);
        assert_eq!(cmds.recv().unwrap(), "LASER:BSLFiber");

        assert_eq!(
            client.force_load("C:\\jobs\\placed.lbrn2").unwrap(),
            Reply::Ok
        );
        assert_eq!(cmds.recv().unwrap(), "FORCELOAD:C:\\jobs\\placed.lbrn2");

        assert_eq!(client.start().unwrap(), Reply::Ok);
        assert_eq!(cmds.recv().unwrap(), "START");

        assert_eq!(client.status().unwrap(), Reply::Ok);
        assert_eq!(cmds.recv().unwrap(), "STATUS");
    }

    #[test]
    fn parses_busy_and_unknown_replies() {
        let (busy_addr, _b) = fake_lightburn(|_| Some("!"));
        let client = LightburnClient::connect(busy_addr, loopback(0)).unwrap();
        assert_eq!(client.status().unwrap(), Reply::Busy);

        let (unk_addr, _u) = fake_lightburn(|_| Some("?"));
        let client = LightburnClient::connect(unk_addr, loopback(0)).unwrap();
        assert_eq!(client.ping().unwrap(), Reply::Unknown);
    }

    #[test]
    fn unexpected_reply_text_is_an_invalid_data_error() {
        let (addr, _c) = fake_lightburn(|_| Some("NOPE"));
        let client = LightburnClient::connect(addr, loopback(0)).unwrap();
        let err = client.ping().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("NOPE"), "carries the text: {err}");
    }

    #[test]
    fn a_silent_server_times_out() {
        // A server that receives but never replies: recv cleanly hits the read
        // timeout on every platform (an unbound port would instead race an ICMP
        // port-unreachable → ConnectionReset on Windows).
        let (addr, _c) = fake_lightburn(|_| None);
        let client = LightburnClient::connect(addr, loopback(0)).unwrap();
        let err = client.status().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "got: {err}");
    }
}
