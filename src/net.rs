//! Networking: accepts telnet connections and turns lines of input into `PlayerInput` messages.
//! Each connected client is an entity with a `Connection` component. The telnet protocol itself is
//! in `telnet.rs`.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};

use bevy::prelude::*;

use crate::telnet::{self, Command, IAC};

/// Address to listen on, unless overridden by the `MUD_ADDR` environment variable.
pub const DEFAULT_ADDRESS: &str = "0.0.0.0:4000";

/// Listens for players on `address`.
pub struct NetPlugin {
    pub address: String,
}

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        let address = &self.address;
        let listener = TcpListener::bind(address).expect("failed to bind MUD port");
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
    /// With `closing`: the player is leaving their character, not the game. Everything that
    /// happens when someone leaves happens, then the connection goes back to the character
    /// menu instead of being dropped (`login::return_to_menu`).
    pub to_menu: bool,
    /// In game, the connection shows a prompt after each batch of output. Off during login,
    /// which writes its own prompts ("Password: ").
    prompt: bool,
    /// The prompt to show, kept current by the game (see `vitals::show_prompts`).
    prompt_text: String,
    /// The prompt is the last thing written, so the cursor sits after it.
    at_prompt: bool,
    /// The player sent a line this tick, so they get a prompt even if nothing else was written.
    wants_prompt: bool,
    /// Separates telnet commands from what they type.
    telnet: telnet::Parser,
    /// They've sent a telnet command, so they speak telnet: prompts end with IAC GA.
    speaks_telnet: bool,
    /// They agreed to end-of-record: prompts end with IAC EOR instead.
    eor: bool,
    /// Options they asked about that we've already refused, so we answer each only once.
    refused: Vec<(u8, u8)>,
}

impl Connection {
    /// Queue a line of text to send to this client.
    pub fn send(&mut self, text: impl AsRef<str>) {
        self.send_raw(text);
        self.outbox.extend_from_slice(b"\r\n");
    }

    /// Queue text without a trailing newline.
    pub fn send_raw(&mut self, text: impl AsRef<str>) {
        // Text arriving while the prompt is showing starts on a fresh line, not after the `> `.
        if self.at_prompt {
            self.outbox.extend_from_slice(b"\r\n");
            self.at_prompt = false;
        }
        self.outbox.extend_from_slice(text.as_ref().as_bytes());
    }

    /// What the prompt says from now on.
    pub fn set_prompt(&mut self, text: String) {
        self.prompt_text = text;
    }

    /// Start showing the in-game prompt, once the player has entered the game.
    pub fn enable_prompt(&mut self) {
        self.prompt = true;
        self.wants_prompt = true;
    }

    /// Back to logging in: no longer leaving, and no in-game prompt.
    pub fn back_to_login(&mut self) {
        self.closing = false;
        self.to_menu = false;
        self.prompt = false;
        self.wants_prompt = false;
    }

    /// Ask the client to stop (or resume) echoing what the user types, for password entry.
    /// Telnet clients honor this; raw clients like `nc` will show a couple of stray characters.
    pub fn set_echo(&mut self, echo: bool) {
        // The server "will echo" means the client stops echoing locally.
        let verb = if echo { telnet::WONT } else { telnet::WILL };
        self.outbox
            .extend_from_slice(&[IAC, verb, telnet::OPT_ECHO]);
    }

    /// Act on a telnet command from the client.
    fn negotiate(&mut self, command: Command) {
        self.speaks_telnet = true;
        let refuse = match command {
            Command::Do(telnet::OPT_EOR) => {
                self.eor = true;
                None
            }
            Command::Dont(telnet::OPT_EOR) => {
                self.eor = false;
                None
            }
            // Replies to our own offers of echo.
            Command::Do(telnet::OPT_ECHO) | Command::Dont(telnet::OPT_ECHO) => None,
            // Anything else they'd like us or them to do, we politely decline.
            Command::Do(option) => Some((telnet::WONT, option)),
            Command::Will(option) => Some((telnet::DONT, option)),
            Command::Dont(_) | Command::Wont(_) | Command::Other => None,
        };
        if let Some((verb, option)) = refuse
            && !self.refused.contains(&(verb, option))
        {
            self.refused.push((verb, option));
            self.outbox.extend_from_slice(&[IAC, verb, option]);
        }
    }

    /// What ends a prompt for this client, so it can tell the prompt apart from text.
    fn prompt_marker(&self) -> &'static [u8] {
        match (self.eor, self.speaks_telnet) {
            (true, _) => &[IAC, telnet::EOR],
            (false, true) => &[IAC, telnet::GA],
            (false, false) => &[],
        }
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
                        // Offer end-of-record prompts; MUD clients accept, others ignore it.
                        outbox: vec![IAC, telnet::WILL, telnet::OPT_EOR],
                        closing: false,
                        to_menu: false,
                        prompt: false,
                        prompt_text: "> ".into(),
                        at_prompt: false,
                        wants_prompt: false,
                        telnet: telnet::Parser::default(),
                        speaks_telnet: false,
                        eor: false,
                        refused: Vec::new(),
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

fn read_input(
    mut connections: Query<(Entity, &mut Connection)>,
    mut input: MessageWriter<PlayerInput>,
) {
    let mut buf = [0u8; 1024];
    for (entity, mut conn) in &mut connections {
        loop {
            match conn.stream.read(&mut buf) {
                Ok(0) => {
                    conn.eof = true;
                    break;
                }
                Ok(n) => {
                    let mut text = Vec::new();
                    let commands = conn.telnet.feed(&buf[..n], &mut text);
                    conn.pending.extend_from_slice(&text);
                    for command in commands {
                        conn.negotiate(command);
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(_) => {
                    conn.closing = true;
                    break;
                }
            }
        }

        while let Some(pos) = conn.pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = conn.pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw).trim().to_string();
            conn.lines.push_back(line);
        }
        if let Some(line) = conn.lines.pop_front() {
            // The player pressed Enter, so their cursor is already on a fresh line.
            conn.at_prompt = false;
            conn.wants_prompt = true;
            input.write(PlayerInput { entity, line });
        } else if conn.eof {
            conn.closing = true;
        }
    }
}

/// Write each connection's queued output, ending every batch with exactly one prompt.
pub fn flush_output(mut connections: Query<&mut Connection>) {
    for mut conn in &mut connections {
        let wants_prompt = std::mem::take(&mut conn.wants_prompt);
        if conn.prompt && !conn.closing && (wants_prompt || !conn.outbox.is_empty()) {
            let prompt = conn.prompt_text.clone();
            conn.send_raw(prompt);
            let marker = conn.prompt_marker();
            conn.outbox.extend_from_slice(marker);
            conn.at_prompt = true;
        }
        if conn.outbox.is_empty() {
            continue;
        }
        let out = std::mem::take(&mut conn.outbox);
        if conn.stream.write_all(&out).is_err() {
            conn.closing = true;
        }
    }
}

pub fn cleanup_disconnected(mut commands: Commands, connections: Query<(Entity, &Connection)>) {
    for (entity, conn) in &connections {
        if conn.closing && !conn.to_menu {
            info!("client {entity} disconnected");
            commands.entity(entity).despawn();
        }
    }
}
