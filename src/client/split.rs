//! A TLS client session driven from two threads (features `ws` and `tls`).
//!
//! tls-net's `TlsStream` owns its transport, so one thread cannot block in
//! `read` while another writes. The WebSocket bridge needs exactly that: one
//! thread waits for the upstream while client messages go out from another.
//! `TlsHalf` holds the session without a transport. The bridge keeps it
//! under the same lock as the upstream WebSocket state, reads ciphertext
//! from its own clone of the socket without the lock, and hands it in with
//! `absorb`; every write to the socket happens under the lock, so records
//! leave in the order the session sealed them.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Instant;
use std::vec::Vec;

use tls_net::{
    ClientSession, Event, MAX_RECORD_PAYLOAD, MIN_OUTPUT_BUFFER, SessionState, TlsError,
};

use crate::core::{ErrorClass, TimeoutKind};

/// The most ciphertext held without the session consuming it. A record is
/// at most about 16 KiB; the session takes each one whole, so more than this
/// waiting means the peer is not sending TLS.
const MAX_HELD_INPUT: usize = 256 * 1024;

/// A client session after its handshake, without a transport.
pub(crate) struct TlsHalf {
    session: ClientSession,
    input: Vec<u8>,
    filled: usize,
    output: Vec<u8>,
    /// Plaintext produced while sending, for the next `absorb`.
    held: Vec<u8>,
    /// Plaintext of the last `absorb`.
    plain: Vec<u8>,
    peer_closed: bool,
    broken: bool,
}

fn invalid(e: TlsError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl TlsHalf {
    /// Runs the handshake of `session` over `stream`, which carries nothing
    /// else yet, until `deadline`.
    pub(crate) fn handshake(
        session: ClientSession,
        stream: &TcpStream,
        deadline: Instant,
    ) -> Result<TlsHalf, ErrorClass> {
        let mut half = TlsHalf {
            input: vec![0; session.input_capacity_required()],
            session,
            filled: 0,
            output: vec![0; MIN_OUTPUT_BUFFER],
            held: Vec::new(),
            plain: Vec::new(),
            peer_closed: false,
            broken: false,
        };
        let mut chunk = [0u8; 4096];
        let mut early = Vec::new();
        let (mut writer, mut reader) = (stream, stream);
        loop {
            let _ = half
                .run(&mut writer, &mut early)
                .map_err(|_| ErrorClass::Tls)?;
            if half.session.info().is_some() {
                break;
            }
            if half.peer_closed {
                return Err(ErrorClass::Tls);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ErrorClass::Timeout(TimeoutKind::Connect));
            }
            stream
                .set_read_timeout(Some(left))
                .map_err(|_| ErrorClass::UpstreamReset)?;
            let n = match reader.read(&mut chunk) {
                Ok(0) => return Err(ErrorClass::Tls),
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(ErrorClass::Timeout(TimeoutKind::Connect));
                }
                Err(_) => return Err(ErrorClass::UpstreamReset),
            };
            half.push(&chunk[..n]).map_err(|_| ErrorClass::Tls)?;
        }
        half.held = early;
        Ok(half)
    }

    /// Appends ciphertext after what the session has not consumed yet.
    fn push(&mut self, cipher: &[u8]) -> io::Result<()> {
        let want = (self.filled + cipher.len()).max(self.session.input_capacity_required());
        if want > MAX_HELD_INPUT {
            self.broken = true;
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        if self.input.len() < want {
            self.input.resize(want, 0);
        }
        self.input[self.filled..self.filled + cipher.len()].copy_from_slice(cipher);
        self.filled += cipher.len();
        Ok(())
    }

    /// Steps the session until it needs more input, sending what it writes
    /// to `wire` and appending plaintext to `plain`. True when anything moved.
    fn run(&mut self, wire: &mut impl Write, plain: &mut Vec<u8>) -> io::Result<bool> {
        if self.broken {
            return Err(invalid(TlsError::WrongState));
        }
        let mut moved = false;
        loop {
            let p = match self
                .session
                .step(&mut self.input[..self.filled], &mut self.output)
            {
                Ok(p) => p,
                Err(e) => {
                    self.broken = true;
                    return Err(invalid(e));
                }
            };
            let (consumed, written) = (p.consumed, p.written);
            let mut stop = false;
            match p.event {
                Event::Data(d) => {
                    plain.extend_from_slice(d);
                    moved = true;
                }
                Event::NeedInput => stop = consumed == 0 && written == 0,
                Event::PeerClosed | Event::Closed => {
                    self.peer_closed = true;
                    stop = true;
                }
                Event::OutputFull if written == 0 => {
                    self.broken = true;
                    return Err(invalid(TlsError::OutputFull {
                        required: MIN_OUTPUT_BUFFER,
                    }));
                }
                _ => {}
            }
            if written > 0 {
                if let Err(e) = wire.write_all(&self.output[..written]) {
                    self.broken = true;
                    return Err(e);
                }
            }
            self.input.copy_within(consumed..self.filled, 0);
            self.filled -= consumed;
            moved |= consumed > 0 || written > 0;
            if stop {
                return Ok(moved);
            }
        }
    }

    /// Takes ciphertext read from the socket and returns the plaintext it
    /// carried (with any held from a send), and whether the peer closed.
    pub(crate) fn absorb(
        &mut self,
        cipher: &[u8],
        wire: &mut impl Write,
    ) -> io::Result<(&[u8], bool)> {
        self.push(cipher)?;
        let mut plain = core::mem::take(&mut self.plain);
        plain.clear();
        plain.append(&mut self.held);
        let r = self.run(wire, &mut plain);
        self.plain = plain;
        let _ = r?;
        Ok((&self.plain, self.peer_closed))
    }

    /// Seals up to one record of `bytes` and sends it. Returns how many bytes
    /// went, or 0 when the session cannot seal until more input arrives (a
    /// post-handshake message that came in part).
    pub(crate) fn send(&mut self, bytes: &[u8], wire: &mut impl Write) -> io::Result<usize> {
        if self.broken {
            return Err(invalid(TlsError::WrongState));
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let part = &bytes[..bytes.len().min(MAX_RECORD_PAYLOAD)];
        loop {
            match self.session.encrypt(part, &mut self.output) {
                Ok(n) => {
                    if let Err(e) = wire.write_all(&self.output[..n]) {
                        self.broken = true;
                        return Err(e);
                    }
                    return Ok(part.len());
                }
                // Records the session queued go out through `step` first.
                Err(TlsError::WrongState)
                    if matches!(
                        self.session.state(),
                        SessionState::Established | SessionState::HalfClosed
                    ) =>
                {
                    let mut held = core::mem::take(&mut self.held);
                    let moved = self.run(wire, &mut held);
                    self.held = held;
                    if !moved? {
                        return Ok(0);
                    }
                }
                Err(e) => {
                    self.broken = true;
                    return Err(invalid(e));
                }
            }
        }
    }

    /// Sends `close_notify`.
    pub(crate) fn close(&mut self, wire: &mut impl Write) {
        if self.broken {
            return;
        }
        if let Ok(n) = self.session.close(&mut self.output) {
            let _ = wire.write_all(&self.output[..n]);
        }
    }
}
