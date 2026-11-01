//! The conversation over a QUIC connection: a control stream (requests and their answers) and a
//! feed stream per session followed, each frame a [`po_wire`] message.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use po_wire::{Decoder, Envelope, Feed, Reply, Request, Subscribe};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use crate::tls::{Peer, Trust};

/// The first byte of a stream says what it carries.
const CONTROL: u8 = b'C';
const FEED: u8 = b'F';

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    Closed,
    Timeout,
    Protocol(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Closed => write!(f, "connection closed"),
            LinkError::Timeout => write!(f, "no answer in time"),
            LinkError::Protocol(e) => write!(f, "protocol: {e}"),
        }
    }
}

impl std::error::Error for LinkError {}

async fn write_frame<T: Serialize>(send: &mut quinn::SendStream, message: &T) -> Result<(), LinkError> {
    send.write_all(&po_wire::encode(message)).await.map_err(|_| LinkError::Closed)
}

/// Frames read off a stream as they arrive.
pub struct Frames {
    recv: quinn::RecvStream,
    decoder: Decoder,
}

impl Frames {
    fn new(recv: quinn::RecvStream) -> Self {
        Self { recv, decoder: Decoder::default() }
    }

    pub async fn next<T: for<'de> serde::Deserialize<'de>>(&mut self) -> Result<Option<T>, LinkError> {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            if let Some(message) = self.decoder.next_message().map_err(|e| LinkError::Protocol(e.to_string()))? {
                return Ok(Some(message));
            }
            match self.recv.read(&mut buf).await {
                Ok(Some(n)) => self.decoder.feed(&buf[..n]),
                Ok(None) => return Ok(None),
                Err(_) => return Err(LinkError::Closed),
            }
        }
    }
}

/// What the PC does with a phone's requests: the service implements it.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// The PC's name, shown on the phone.
    fn name(&self) -> String;
    async fn request(&self, peer: &Peer, request: Request) -> Reply;
    /// The feed of a session; it ends (the channel closes) when the session does.
    async fn subscribe(&self, peer: &Peer, subscribe: Subscribe) -> mpsc::Receiver<Feed>;
}

/// The inputs already carried out, by device: one sent again after a reconnection (the phone
/// did not get the answer) is answered without being carried out twice.
#[derive(Default)]
pub struct Inputs {
    last: Mutex<HashMap<[u8; 32], u64>>,
}

impl Inputs {
    /// True if input `id` of `device` is new (and from now on, seen).
    pub fn fresh(&self, device: &[u8; 32], id: u64) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let seen = last.entry(*device).or_insert(0);
        if id <= *seen {
            return false;
        }
        *seen = id;
        true
    }
}

/// Serves phones on `endpoint` until it closes.
pub async fn serve(endpoint: quinn::Endpoint, pc_id: String, trust: Arc<Trust>, backend: Arc<dyn Backend>) {
    let inputs = Arc::new(Inputs::default());
    while let Some(incoming) = endpoint.accept().await {
        let (trust, backend, inputs, pc_id) = (trust.clone(), backend.clone(), inputs.clone(), pc_id.clone());
        tokio::spawn(async move {
            // An unknown key fails the handshake itself: nothing of ours is read before.
            let Ok(Ok(connection)) = tokio::time::timeout(Duration::from_secs(15), async { incoming.accept()?.await }).await else { return };
            let Some(peer) = crate::tls::peer_key(&connection).and_then(|k| trust.get(&k)) else {
                connection.close(1u32.into(), b"unknown device");
                return;
            };
            tracing::info!("po-link: {} connected from {}", peer.name, crate::net::canonical(connection.remote_address()));
            handle_connection(connection, peer, pc_id, trust, backend, inputs).await;
        });
    }
}

async fn handle_connection(connection: quinn::Connection, peer: Peer, pc_id: String, trust: Arc<Trust>, backend: Arc<dyn Backend>, inputs: Arc<Inputs>) {
    // Revoked while connected: the connection goes at once.
    let watch = connection.clone();
    let key = peer.ed;
    let watcher = {
        let trust = trust.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if trust.get(&key).is_none() {
                    watch.close(2u32.into(), b"revoked");
                    return;
                }
            }
        })
    };
    loop {
        let Ok((send, recv)) = connection.accept_bi().await else { break };
        let (peer, backend, inputs, pc_id) = (peer.clone(), backend.clone(), inputs.clone(), pc_id.clone());
        tokio::spawn(async move {
            let _ = handle_stream(send, recv, peer, pc_id, backend, inputs).await;
        });
    }
    watcher.abort();
}

async fn handle_stream(mut send: quinn::SendStream, mut recv: quinn::RecvStream, peer: Peer, pc_id: String, backend: Arc<dyn Backend>, inputs: Arc<Inputs>) -> Result<(), LinkError> {
    let mut tag = [0u8; 1];
    recv.read_exact(&mut tag).await.map_err(|_| LinkError::Closed)?;
    let mut frames = Frames::new(recv);
    match tag[0] {
        CONTROL => {
            // Answers go out through one writer: requests run side by side (opening a session
            // takes seconds, a ping must not wait for it).
            let (tx, mut rx) = mpsc::unbounded_channel::<Envelope<Reply>>();
            let writer = tokio::spawn(async move {
                while let Some(reply) = rx.recv().await {
                    if write_frame(&mut send, &reply).await.is_err() {
                        break;
                    }
                }
            });
            while let Some(Envelope { rid, body }) = frames.next::<Envelope<Request>>().await? {
                let reply = match &body {
                    Request::Hello { .. } => Some(Reply::Welcome { v: po_wire::VERSION, pc: pc_id.clone(), name: backend.name() }),
                    Request::Ping { t } => Some(Reply::Pong { t: *t }),
                    Request::Input { id, .. } if !inputs.fresh(&peer.ed, *id) => Some(Reply::Done { text: "duplicate".into() }),
                    _ => None,
                };
                if let Some(reply) = reply {
                    let _ = tx.send(Envelope { rid, body: reply });
                    continue;
                }
                let (tx, backend, peer) = (tx.clone(), backend.clone(), peer.clone());
                tokio::spawn(async move {
                    let reply = backend.request(&peer, body).await;
                    let _ = tx.send(Envelope { rid, body: reply });
                });
            }
            drop(tx);
            let _ = writer.await;
            Ok(())
        }
        FEED => {
            let Some(subscribe) = frames.next::<Subscribe>().await? else { return Ok(()) };
            let mut feed = backend.subscribe(&peer, subscribe).await;
            loop {
                tokio::select! {
                    item = feed.recv() => match item {
                        Some(item) => write_frame(&mut send, &item).await?,
                        None => break,
                    },
                    // The phone stopped following: its side of the stream ends.
                    end = frames.next::<Subscribe>() => if !matches!(end, Ok(Some(_))) { break },
                }
            }
            let _ = send.finish();
            Ok(())
        }
        _ => Err(LinkError::Protocol("unknown stream".into())),
    }
}

/// A phone's connection to a PC.
pub struct Link {
    connection: quinn::Connection,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    next: AtomicU64,
    /// The PC's id and name, as it introduced itself.
    pub pc: String,
    pub name: String,
}

impl Link {
    /// Opens the control stream on an established connection and introduces ourselves.
    pub async fn open(connection: quinn::Connection, my_name: &str, app: &str) -> Result<Self, LinkError> {
        let (mut send, recv) = connection.open_bi().await.map_err(|_| LinkError::Closed)?;
        send.write_all(&[CONTROL]).await.map_err(|_| LinkError::Closed)?;
        let (outgoing, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if send.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>> = Arc::default();
        let waiting = pending.clone();
        tokio::spawn(async move {
            let mut frames = Frames::new(recv);
            while let Ok(Some(Envelope { rid, body })) = frames.next::<Envelope<Reply>>().await {
                if let Some(tx) = waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid) {
                    let _ = tx.send(body);
                }
            }
            // Closed: whoever still waits is told so by the dropped senders.
            waiting.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        let mut link = Self { connection, outgoing, pending, next: AtomicU64::new(1), pc: String::new(), name: String::new() };
        match link.request(Request::Hello { v: po_wire::VERSION, name: my_name.to_owned(), app: app.to_owned() }, Duration::from_secs(10)).await? {
            Reply::Welcome { pc, name, .. } => {
                link.pc = pc;
                link.name = name;
                Ok(link)
            }
            other => Err(LinkError::Protocol(format!("unexpected {other:?}"))),
        }
    }

    pub async fn request(&self, request: Request, wait: Duration) -> Result<Reply, LinkError> {
        let rid = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(rid, tx);
        if self.outgoing.send(po_wire::encode(&Envelope { rid, body: request })).is_err() {
            return Err(LinkError::Closed);
        }
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(LinkError::Closed),
            Err(_) => {
                self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid);
                Err(LinkError::Timeout)
            }
        }
    }

    /// Follows a session: the snapshot (or the missed events), then the events as they come.
    /// The channel closes with the stream.
    pub async fn subscribe(&self, subscribe: &Subscribe) -> Result<mpsc::Receiver<Feed>, LinkError> {
        let (mut send, recv) = self.connection.open_bi().await.map_err(|_| LinkError::Closed)?;
        send.write_all(&[FEED]).await.map_err(|_| LinkError::Closed)?;
        write_frame(&mut send, subscribe).await?;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            let mut frames = Frames::new(recv);
            loop {
                tokio::select! {
                    frame = frames.next::<Feed>() => match frame {
                        Ok(Some(feed)) => if tx.send(feed).await.is_err() { break },
                        _ => break,
                    },
                    // Nobody reads any more: the stream is let go, and the PC stops sending.
                    _ = tx.closed() => break,
                }
            }
            let _ = send.finish();
        });
        Ok(rx)
    }

    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }

    pub async fn closed(&self) {
        self.connection.closed().await;
    }

    pub fn remote(&self) -> SocketAddr {
        crate::net::canonical(self.connection.remote_address())
    }

    pub fn rtt(&self) -> Duration {
        self.connection.rtt()
    }

    pub fn close(&self) {
        self.connection.close(0u32.into(), b"bye");
    }
}
