//! The WebSocket bridge (feature `ws`): serve-net terminates the client's
//! WebSocket, ws-net opens a new one to the upstream, and every message is
//! parsed on one side and written again on the other. No frame passes
//! through as raw bytes, and a tunnel opens only after the upstream's
//! genuine 101 with the accept key for the key the proxy sent (T-11).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serve_net::{ConnectionId, Envelope, WebSocketConn, WebSocketService, WsMessage, WsSender};
use ws_net::{ClientHandshake, Connection, Event, HeadReader};

use crate::client::forward::prelude;
use crate::client::runtime::Runtime;
use crate::core::{ErrorClass, FieldList, ForwardFacts, Outcome, request_fields};
use crate::endpoint::EndpointAddr;

/// Fields ws-net writes itself on the upstream handshake.
const OWN: &[&str] = &[
    "host",
    "upgrade",
    "connection",
    "origin",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-protocol",
    "sec-websocket-extensions",
];

/// The most distinct subprotocol names the bridge answers with. serve-net
/// takes the chosen name as `&'static str`, so each new one is kept for the
/// life of the process; the cap bounds that memory.
const MAX_PROTOCOLS: usize = 64;

fn intern(name: &str) -> Option<&'static str> {
    static NAMES: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(n) = names.iter().find(|n| **n == name) {
        return Some(n);
    }
    if names.len() >= MAX_PROTOCOLS {
        return None;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(leaked);
    Some(leaked)
}

/// An upstream WebSocket that passed its handshake, waiting for serve-net's
/// 101 to the client.
struct Pending {
    read: TcpStream,
    write: TcpStream,
    conn: Connection,
    idle: Duration,
    since: Instant,
}

/// A bridged connection in both directions.
struct Live {
    /// The upstream connection's state and its write half.
    out: Mutex<(Connection, TcpStream)>,
    closed: AtomicBool,
    code: AtomicU32,
}

/// Open bridges, by the client connection.
#[derive(Default)]
pub(crate) struct Bridges {
    pending: Mutex<HashMap<ConnectionId, Pending>>,
    live: Mutex<HashMap<ConnectionId, Arc<Live>>>,
}

impl Bridges {
    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<ConnectionId, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_live(&self) -> std::sync::MutexGuard<'_, HashMap<ConnectionId, Arc<Live>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn dial(addr: &EndpointAddr, deadline: Instant) -> Result<TcpStream, ErrorClass> {
    let addrs: Vec<SocketAddr> = match addr {
        EndpointAddr::Ip(a) => vec![*a],
        EndpointAddr::Name { host, port } => (&**host, *port)
            .to_socket_addrs()
            .map_err(|_| ErrorClass::Connect)?
            .collect(),
    };
    for a in addrs {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        if let Ok(s) = TcpStream::connect_timeout(&a, left) {
            let _ = s.set_nodelay(true);
            return Ok(s);
        }
    }
    Err(ErrorClass::Connect)
}

/// What `open` needs from the client's request.
pub(crate) struct Upgrade<'a> {
    pub fields: Vec<(&'a str, &'a [u8])>,
    pub method: &'a [u8],
    pub target: &'a [u8],
    pub host: &'a [u8],
    pub client: Option<SocketAddr>,
    pub https: bool,
    pub peer: Option<SocketAddr>,
    pub hops: u8,
}

/// Opens the upstream WebSocket for a client's upgrade on `route`: picks an
/// endpoint, connects, sends a fresh handshake (the client's subprotocols
/// offered, no extensions), and checks the upstream's 101 and accept key.
/// Returns the subprotocol the upstream chose.
pub(crate) fn open(
    bridges: &Bridges,
    rt: &Runtime,
    route: usize,
    pool: usize,
    up: &Upgrade<'_>,
    connection: ConnectionId,
) -> Result<Option<&'static str>, ErrorClass> {
    let (Some(r), Some(cfg), Some(prt)) = (
        rt.config.routes().get(route),
        rt.config.pool_at(pool),
        rt.pools.get(pool),
    ) else {
        return Err(ErrorClass::NoEndpoint);
    };
    if cfg.tls().is_some() {
        // A TLS stream cannot be read and written from two threads; see the
        // roadmap.
        return Err(ErrorClass::Tls);
    }
    let id = prt.balancer.pick(None, &[]).ok_or(ErrorClass::NoEndpoint)?;
    let endpoint = cfg
        .endpoints()
        .get(id.index())
        .ok_or(ErrorClass::NoEndpoint)?;
    let timeouts = cfg.timeouts();
    let result = (|| {
        let deadline = Instant::now() + timeouts.connect_value();
        let mut stream = dial(endpoint.addr(), deadline)?;
        let pre = prelude(cfg.proxy_protocol(), up.client, rt.listener, up.host);
        if !pre.is_empty() {
            stream.write_all(&pre).map_err(|_| ErrorClass::Connect)?;
        }
        // The upstream request's fields, rewritten as for any request.
        let facts = ForwardFacts::new(up.method, up.target, up.host, endpoint)
            .client(up.client)
            .https(up.https)
            .listener(rt.listener)
            .chain(up.peer, up.hops);
        let mut list = FieldList::new();
        let mut target = Vec::new();
        request_fields(&up.fields, &facts, r, &rt.config, &mut list, &mut target)?;
        let host = String::from_utf8_lossy(list.get("host").unwrap_or(b"")).into_owned();
        let target = String::from_utf8(target).map_err(|_| ErrorClass::UpstreamProtocol)?;
        let offered: Vec<String> = up
            .fields
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("sec-websocket-protocol"))
            .flat_map(|(_, v)| {
                String::from_utf8_lossy(v)
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|p| !p.is_empty())
            .collect();
        let offered: Vec<&str> = offered.iter().map(String::as_str).collect();
        let mut hs = ClientHandshake::try_new().map_err(|_| ErrorClass::UpstreamProtocol)?;
        if !offered.is_empty() {
            hs = hs.with_protocols(&offered);
        }
        if let Some(origin) = up
            .fields
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("origin"))
            .and_then(|(_, v)| std::str::from_utf8(v).ok())
        {
            hs = hs.with_origin(origin);
        }
        for (name, value) in list.iter() {
            if !OWN.contains(&name) {
                hs = hs.with_header(name, value);
            }
        }
        let mut out = Vec::new();
        hs.write_request(&host, &target, &mut out)
            .map_err(|_| ErrorClass::UpstreamProtocol)?;
        let head_deadline = Instant::now() + timeouts.response_head_value();
        stream
            .set_write_timeout(Some(timeouts.write_idle_value()))
            .map_err(|_| ErrorClass::Connect)?;
        stream
            .write_all(&out)
            .map_err(|_| ErrorClass::UpstreamReset)?;
        let mut buf: Vec<u8> = Vec::with_capacity(1024);
        let mut reader = HeadReader::new();
        let mut chunk = [0u8; 2048];
        let (protocol, conn, leftover) = loop {
            let left = head_deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ErrorClass::Timeout(crate::core::TimeoutKind::ResponseHead));
            }
            stream
                .set_read_timeout(Some(left))
                .map_err(|_| ErrorClass::UpstreamReset)?;
            let n = match stream.read(&mut chunk) {
                Ok(0) => return Err(ErrorClass::UpstreamReset),
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(ErrorClass::Timeout(crate::core::TimeoutKind::ResponseHead));
                }
                Err(_) => return Err(ErrorClass::UpstreamReset),
            };
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > 16 * 1024 {
                return Err(ErrorClass::UpstreamProtocol);
            }
            // A response that is not a valid 101 for this key fails here:
            // no tunnel opens (T-11).
            match reader.client(&hs, &buf) {
                Ok(Some((verified, len))) => {
                    let protocol = match verified.protocol {
                        Some(p) => Some(intern(p).ok_or(ErrorClass::UpstreamProtocol)?),
                        None => None,
                    };
                    let conn = hs.connection(&verified);
                    break (protocol, conn, buf[len..].to_vec());
                }
                Ok(None) => {}
                Err(_) => return Err(ErrorClass::UpstreamProtocol),
            }
        };
        let mut conn = conn;
        let _ = conn.feed(&leftover);
        let write = stream.try_clone().map_err(|_| ErrorClass::UpstreamReset)?;
        let _ = bridges.lock_pending().insert(
            connection,
            Pending {
                read: stream,
                write,
                conn,
                idle: timeouts.tunnel_idle_value(),
                since: Instant::now(),
            },
        );
        Ok(protocol)
    })();
    let outcome = match &result {
        Ok(_) => Outcome::Success,
        Err(c) => Outcome::from(*c),
    };
    if let Some(t) = prt.health.record(id, outcome, rt.tick()) {
        rt.changed(pool, t);
    }
    // Pending entries the client never took (an admission refused the 101)
    // go after a minute.
    bridges
        .lock_pending()
        .retain(|_, p| p.since.elapsed() < Duration::from_secs(60));
    result
}

/// The serve-net side of the bridge.
pub(crate) struct BridgeService {
    pub bridges: Arc<Bridges>,
}

fn flush(conn: &mut Connection, write: &mut TcpStream) -> bool {
    loop {
        let out = conn.output();
        if out.is_empty() {
            return true;
        }
        let n = out.len();
        if write.write_all(out).is_err() {
            return false;
        }
        conn.advance_output(n);
    }
}

/// Upstream to client: reads the upstream, re-frames each message for the
/// client through serve-net's sender, until either side closes.
fn pump(mut read: TcpStream, live: Arc<Live>, sender: WsSender, idle: Duration) {
    let _ = read.set_read_timeout(Some(idle));
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match read.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut messages: Vec<(bool, Vec<u8>)> = Vec::new();
        let mut ended = false;
        {
            let mut guard = live.out.lock().unwrap_or_else(PoisonError::into_inner);
            let (conn, write) = &mut *guard;
            let mut at = 0;
            while at < n {
                at += conn.feed(&buf[at..n]).max(1);
                loop {
                    match conn.poll() {
                        Ok(Event::Text(t)) => messages.push((true, t.as_bytes().to_vec())),
                        Ok(Event::Binary(b)) => messages.push((false, b.to_vec())),
                        Ok(Event::Close(frame)) => {
                            let code = frame.map_or(1000, |f| u32::from(u16::from(f.code)));
                            live.code.store(code, Ordering::Release);
                            ended = true;
                            break;
                        }
                        Ok(Event::NeedInput) => break,
                        Ok(_) => {}
                        Err(_) => {
                            live.code.store(1002, Ordering::Release);
                            ended = true;
                            break;
                        }
                    }
                }
                if ended {
                    break;
                }
            }
            let _ = flush(conn, write);
        }
        for (text, bytes) in messages {
            let sent = if text {
                sender.send_text(&String::from_utf8_lossy(&bytes))
            } else {
                sender.send_binary(&bytes)
            };
            if sent.is_err() {
                ended = true;
                break;
            }
        }
        if ended {
            break;
        }
    }
    live.closed.store(true, Ordering::Release);
}

impl WebSocketService for BridgeService {
    fn opened(&self, env: &Envelope, conn: &mut WebSocketConn<'_>) {
        let Some(p) = self.bridges.lock_pending().remove(&env.connection()) else {
            let _ = conn.close(1011, "no upstream");
            return;
        };
        let live = Arc::new(Live {
            out: Mutex::new((p.conn, p.write)),
            closed: AtomicBool::new(false),
            code: AtomicU32::new(1001),
        });
        let _ = self
            .bridges
            .lock_live()
            .insert(env.connection(), Arc::clone(&live));
        let sender = conn.sender();
        let idle = p.idle;
        let read = p.read;
        let spawned = std::thread::Builder::new()
            .name("proxy-net-ws".into())
            .spawn(move || pump(read, live, sender, idle));
        if spawned.is_err() {
            let _ = conn.close(1011, "no thread for the bridge");
        }
    }

    fn message(&self, env: &Envelope, message: WsMessage<'_>, conn: &mut WebSocketConn<'_>) {
        let live = self.bridges.lock_live().get(&env.connection()).cloned();
        let Some(live) = live else {
            let _ = conn.close(1011, "no upstream");
            return;
        };
        if live.closed.load(Ordering::Acquire) {
            let code = u16::try_from(live.code.load(Ordering::Acquire)).unwrap_or(1011);
            let _ = conn.close(code, "");
            return;
        }
        let mut guard = live.out.lock().unwrap_or_else(PoisonError::into_inner);
        let (up, write) = &mut *guard;
        let sent = match message {
            WsMessage::Text(t) => up.send_text(t).is_ok(),
            WsMessage::Binary(b) => up.send_binary(b).is_ok(),
            _ => true,
        };
        if !sent || !flush(up, write) {
            live.closed.store(true, Ordering::Release);
            drop(guard);
            let _ = conn.close(1011, "upstream failed");
        }
    }

    fn closed(&self, env: &Envelope, code: Option<u16>) {
        let Some(live) = self.bridges.lock_live().remove(&env.connection()) else {
            return;
        };
        let mut guard = live.out.lock().unwrap_or_else(PoisonError::into_inner);
        let (up, write) = &mut *guard;
        let close = code
            .map(ws_net::CloseCode::new)
            .filter(ws_net::CloseCode::is_sendable)
            .unwrap_or(ws_net::CloseCode::NORMAL);
        let _ = up.close(close, "");
        let _ = flush(up, write);
        let _ = write.shutdown(std::net::Shutdown::Both);
        live.closed.store(true, Ordering::Release);
    }
}
