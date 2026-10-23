//! Networking: accepts telnet connections and turns lines of input into `PlayerInput` messages.
//! Each connected client is an entity with a `Connection` component.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};

use bevy::prelude::*;

/// Address to listen on, unless overridden by the `MUD_ADDR` environment variable.
pub const DEFAULT_ADDRESS: &str = "0.0.0.0:4000";

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        let address = std::env::var("MUD_ADDR").unwrap_or_else(|_| DEFAULT_ADDRESS.into());
        let listener = TcpListener::bind(&address).expect("failed to bind MUD port");
        listener
            .set_nonblocking(true)
            .expect("failed to set listener non-blocking");
        info!("MUD listening on {address}");

        app.insert_resource(Listener(listener))
            .add_message::<PlayerInput>()
            .add_systems(PreUpdate, (accept_connections, read_input).chain())
            .add_systems(PostUpdate, (flush_output, cleanup_disconnected).chain());
    }
}

#[derive(Resource)]
struct Listener(TcpListener);

/// A connected client.
#[derive(Component)]
pub struct Connection {
    stream: TcpStream,
    /// Bytes received that don't yet form a complete line.
    pending: Vec<u8>,
    /// Complete lines waiting to be handled, oldest first. One is handed out per tick, so a
    /// player's commands always run in the order typed, whichever system handles each one.
    lines: VecDeque<String>,
    /// The client stopped sending. It's dropped once its queued lines have been handled.
    eof: bool,
    /// Bytes queued to be sent at the end of the frame.
    outbox: Vec<u8>,
    /// Set when the client should be dropped at the end of the frame.
    pub closing: bool,
}

impl Connection {
    /// Queue a line of text to send to this client.
    pub fn send(&mut self, text: impl AsRef<str>) {
        self.send_raw(text);
        self.outbox.extend_from_slice(b"\r\n");
    }

    /// Queue text without a trailing newline (used for prompts).
    pub fn send_raw(&mut self, text: impl AsRef<str>) {
        self.outbox.extend_from_slice(text.as_ref().as_bytes());
    }

    /// Ask the client to stop (or resume) echoing what the user types, for password entry.
    /// Telnet clients honor this; raw clients like `nc` will show a couple of stray characters.
    pub fn set_echo(&mut self, echo: bool) {
        const IAC: u8 = 255;
        const WILL: u8 = 251;
        const WONT: u8 = 252;
        const ECHO: u8 = 1;
        // The server "will echo" means the client stops echoing locally.
        let verb = if echo { WONT } else { WILL };
        self.outbox.extend_from_slice(&[IAC, verb, ECHO]);
    }
}

/// Marks a client that connected this frame and hasn't been greeted yet.
#[derive(Component)]
pub struct JustConnected;

/// One line of input typed by a client.
#[derive(Message)]
pub struct PlayerInput {
    pub entity: Entity,
    pub line: String,
}

fn accept_connections(mut commands: Commands, listener: Res<Listener>) {
    loop {
        match listener.0.accept() {
            Ok((stream, addr)) => {
                if let Err(err) = stream.set_nonblocking(true) {
                    warn!("dropping {addr}: {err}");
                    continue;
                }
                info!("connection from {addr}");
                commands.spawn((
                    Connection {
                        stream,
                        pending: Vec::new(),
                        lines: VecDeque::new(),
                        eof: false,
                        outbox: Vec::new(),
                        closing: false,
                    },
                    JustConnected,
                ));
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
            Err(err) => {
                warn!("accept failed: {err}");
                break;
            }
        }
    }
}

fn read_input(mut connections: Query<(Entity, &mut Connection)>, mut input: MessageWriter<PlayerInput>) {
    let mut buf = [0u8; 1024];
    for (entity, mut conn) in &mut connections {
        loop {
            match conn.stream.read(&mut buf) {
                Ok(0) => {
                    conn.eof = true;
                    break;
                }
                Ok(n) => conn.pending.extend_from_slice(&buf[..n]),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(_) => {
                    conn.closing = true;
                    break;
                }
            }
        }

        while let Some(pos) = conn.pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = conn.pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&strip_telnet(&raw)).trim().to_string();
            conn.lines.push_back(line);
        }
        if let Some(line) = conn.lines.pop_front() {
            input.write(PlayerInput { entity, line });
        } else if conn.eof {
            conn.closing = true;
        }
    }
}

/// Remove telnet IAC negotiation sequences so they don't show up as garbage in commands.
fn strip_telnet(raw: &[u8]) -> Vec<u8> {
    const IAC: u8 = 255;
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == IAC {
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}

fn flush_output(mut connections: Query<&mut Connection>) {
    for mut conn in &mut connections {
        if conn.outbox.is_empty() {
            continue;
        }
        let out = std::mem::take(&mut conn.outbox);
        if conn.stream.write_all(&out).is_err() {
            conn.closing = true;
        }
    }
}

fn cleanup_disconnected(mut commands: Commands, connections: Query<(Entity, &Connection)>) {
    for (entity, conn) in &connections {
        if conn.closing {
            info!("client {entity} disconnected");
            commands.entity(entity).despawn();
        }
    }
}
