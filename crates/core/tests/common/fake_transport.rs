//! A fake `twitch_irc::Transport` backed by `tokio::io::duplex`.
//!
//! `Transport::new()` takes no arguments, so tests cannot directly hand a
//! stream to the client. Instead, the test calls [`install`] before
//! constructing the `TwitchIRCClient`; `install` pushes the client-side of
//! a duplex stream onto a global queue. `FakeTransport::new()` pops from
//! the front to obtain its stream.
//!
//! **Safety model:** `cargo nextest` runs each test in its own OS process.
//! The queue is isolated per-process, and each test process spawns exactly
//! one `TestBotBuilder` (one `install()` → one `FakeTransport::new()`).
//! FIFO order alone does NOT guarantee correct pairing if two bots were
//! spawned concurrently in the same process: `Transport::new()` fires from
//! an async connection task spawned by `TwitchIRCClient::new()`, so push
//! and pop order can diverge. The defensive assertion in `install()` catches
//! that misuse at runtime.

use std::collections::VecDeque;
use std::fmt::{self, Debug};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use either::Either;
use futures_util::sink::Sink;
use futures_util::stream::FusedStream;
use futures_util::{SinkExt, StreamExt, TryStreamExt, future};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::mpsc;
use tokio_stream::wrappers::LinesStream;
use tokio_util::codec::{BytesCodec, FramedWrite};
use twitch_irc::message::{AsRawIRC, IRCMessage, IRCParseError};
use twitch_irc::transport::Transport;

static QUEUE: Mutex<VecDeque<DuplexStream>> = Mutex::new(VecDeque::new());

/// Handle returned to the test for injecting incoming lines and capturing outgoing ones.
pub struct TransportHandle {
    pub inject: mpsc::Sender<String>,
    pub capture: mpsc::Receiver<String>,
}

/// Set up a fake transport pair. Call BEFORE constructing `TwitchIRCClient`.
///
/// Pushes the client-side duplex half onto the global queue; `FakeTransport::new()`
/// pops it. The returned [`TransportHandle`] lets tests inject IRC lines (server→client)
/// and inspect lines the client sent (client→server).
pub async fn install() -> TransportHandle {
    let (client_side, test_side) = tokio::io::duplex(64 * 1024);
    let (inject_tx, mut inject_rx) = mpsc::channel::<String>(64);
    let (capture_tx, capture_rx) = mpsc::channel::<String>(64);

    tokio::spawn(async move {
        let (test_read, mut test_write) = tokio::io::split(test_side);

        // Send the Twitch handshake so the client believes auth succeeded,
        // then forward any lines the test injects.
        let handshake_task = async move {
            let handshake = [
                ":tmi.twitch.tv CAP * ACK :twitch.tv/commands twitch.tv/tags twitch.tv/membership\r\n",
                ":tmi.twitch.tv 001 bot :Welcome, GLHF!\r\n",
                ":tmi.twitch.tv 002 bot :Your host is tmi.twitch.tv\r\n",
                ":tmi.twitch.tv 003 bot :This server is rather new\r\n",
                ":tmi.twitch.tv 004 bot :-\r\n",
                ":tmi.twitch.tv 375 bot :-\r\n",
                ":tmi.twitch.tv 372 bot :You are in a maze of twisty passages, all alike.\r\n",
                ":tmi.twitch.tv 376 bot :>\r\n",
                "@badge-info=;badges=;color=;display-name=bot;emote-sets=0;user-id=12345;user-type= :tmi.twitch.tv GLOBALUSERSTATE\r\n",
            ];
            for line in handshake {
                if test_write.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
            while let Some(line) = inject_rx.recv().await {
                let payload = if line.ends_with("\r\n") {
                    line
                } else {
                    format!("{line}\r\n")
                };
                if test_write.write_all(payload.as_bytes()).await.is_err() {
                    return;
                }
            }
        };

        let capture_task = async move {
            let reader = BufReader::new(test_read);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if capture_tx.send(line).await.is_err() {
                    return;
                }
            }
        };

        tokio::join!(handshake_task, capture_task);
    });

    {
        let mut q = QUEUE.lock().unwrap();
        assert!(
            q.is_empty(),
            "FakeTransport: install() called while a stream is already queued — \
             only one TestBotBuilder per process is supported"
        );
        q.push_back(client_side);
    }

    TransportHandle {
        inject: inject_tx,
        capture: capture_rx,
    }
}

/// A fake `Transport` backed by a `tokio::io::duplex` stream.
///
/// Instantiated by `FakeTransport::new()` which pops from the global queue
/// populated by [`install`].
pub struct FakeTransport {
    incoming: Box<
        dyn FusedStream<Item = Result<IRCMessage, Either<std::io::Error, IRCParseError>>>
            + Unpin
            + Send
            + Sync,
    >,
    outgoing: Box<dyn Sink<IRCMessage, Error = std::io::Error> + Unpin + Send + Sync>,
}

impl Debug for FakeTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeTransport").finish()
    }
}

/// Error returned when `FakeTransport::new()` is called without a prior [`install`].
#[derive(Debug)]
pub struct FakeTransportConnectError(pub String);

impl fmt::Display for FakeTransportConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FakeTransport connect error: {}", self.0)
    }
}

impl std::error::Error for FakeTransportConnectError {}

#[async_trait]
impl Transport for FakeTransport {
    type ConnectError = FakeTransportConnectError;
    type IncomingError = std::io::Error;
    type OutgoingError = std::io::Error;
    type Incoming = Box<
        dyn FusedStream<Item = Result<IRCMessage, Either<std::io::Error, IRCParseError>>>
            + Unpin
            + Send
            + Sync,
    >;
    type Outgoing = Box<dyn Sink<IRCMessage, Error = std::io::Error> + Unpin + Send + Sync>;

    async fn new() -> Result<Self, FakeTransportConnectError> {
        let stream = {
            let mut q = QUEUE.lock().unwrap();
            let s = q.pop_front().ok_or_else(|| {
                FakeTransportConnectError(
                    "FakeTransport queue empty; call fake_transport::install() before building the client".to_owned(),
                )
            })?;
            assert!(
                q.is_empty(),
                "FakeTransport: queue has leftover streams after pop — \
                 multiple TestBotBuilder spawns in one process are not supported"
            );
            s
        };
        let (read_half, write_half) = tokio::io::split(stream);

        let lines = BufReader::new(read_half).lines();
        let message_stream = LinesStream::new(lines)
            .try_filter(|line| future::ready(!line.is_empty()))
            .map_err(Either::Left)
            .and_then(|s| future::ready(IRCMessage::parse(&s).map_err(Either::Right)))
            .fuse();

        let message_sink =
            FramedWrite::new(write_half, BytesCodec::new()).with(move |msg: IRCMessage| {
                let mut s = msg.as_raw_irc();
                s.push_str("\r\n");
                future::ready(Ok(Bytes::from(s)))
            });

        Ok(FakeTransport {
            incoming: Box::new(message_stream),
            outgoing: Box::new(message_sink),
        })
    }

    fn split(self) -> (Self::Incoming, Self::Outgoing) {
        (self.incoming, self.outgoing)
    }
}
