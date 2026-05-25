// Length-prefixed TCP client for the handsets daemon. Each request is
// `<u32 BE length><utf-8 payload>` in both directions.
//
// Mirrors handsets-cli/src/main.rs::Conn — kept as a separate copy because
// this crate sits outside that crate's module tree, same trade-off the
// handsets-viewer crate makes.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Conn {
    sock: TcpStream,
}

impl Conn {
    pub fn connect(host: &str, port: u16) -> io::Result<Self> {
        let sock = TcpStream::connect((host, port))?;
        sock.set_nodelay(true)?;
        sock.set_read_timeout(Some(Duration::from_secs(30)))?;
        sock.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self { sock })
    }

    pub fn call(&mut self, cmd: &str) -> io::Result<Vec<u8>> {
        let payload = cmd.as_bytes();
        let len = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cmd too long"))?;
        self.sock.write_all(&len.to_be_bytes())?;
        self.sock.write_all(payload)?;

        let mut hdr = [0u8; 4];
        self.sock.read_exact(&mut hdr)?;
        let n = u32::from_be_bytes(hdr) as usize;
        if n > 256 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("oversized response: {n} bytes"),
            ));
        }
        let mut buf = vec![0u8; n];
        self.sock.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub fn call_str(&mut self, cmd: &str) -> io::Result<String> {
        let bytes = self.call(cmd)?;
        String::from_utf8(bytes).map_err(|e| io::Error::other(format!("bad utf-8: {e}")))
    }
}
