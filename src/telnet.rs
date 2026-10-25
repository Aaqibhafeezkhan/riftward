//! The telnet protocol, as much as a MUD needs: separating commands from text, and the options
//! Riftward negotiates.
//!
//! Telnet commands start with IAC (255). The server offers end-of-record (`IAC WILL EOR`) to every
//! client; MUD clients like Mudlet and TinTin++ accept (`IAC DO EOR`), and from then on each prompt
//! ends with `IAC EOR`, so they know it's a prompt even without a newline, and can draw gauges from
//! it. Other clients that speak telnet get the classic `IAC GA` instead. A raw client like `nc`
//! never sends a telnet command, so it never gets either.

pub const IAC: u8 = 255;
pub const DONT: u8 = 254;
pub const DO: u8 = 253;
pub const WONT: u8 = 252;
pub const WILL: u8 = 251;
const SB: u8 = 250;
/// Go ahead: the classic end-of-prompt marker.
pub const GA: u8 = 249;
const SE: u8 = 240;
/// End of record, as a command: the end-of-prompt marker for clients that agree to it.
pub const EOR: u8 = 239;

/// The echo option: the server echoing means the client doesn't, as for passwords.
pub const OPT_ECHO: u8 = 1;
/// The end-of-record option.
pub const OPT_EOR: u8 = 25;

/// A negotiation from the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Do(u8),
    Dont(u8),
    Will(u8),
    Wont(u8),
    /// Any other command, like GA or a subnegotiation, which Riftward ignores.
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Data,
    /// After an IAC.
    Iac,
    /// After IAC and a verb (DO, DONT, WILL, WONT), waiting for the option.
    Verb(u8),
    /// Inside a subnegotiation, skipped until IAC SE.
    Sub,
    SubIac,
}

/// Splits a client's bytes into text and telnet commands. Keeps its place between reads, since a
/// command can arrive split across them.
#[derive(Debug, Default)]
pub struct Parser {
    state: State,
}

impl Parser {
    /// Add the text in `bytes` to `text`, and return the commands found.
    pub fn feed(&mut self, bytes: &[u8], text: &mut Vec<u8>) -> Vec<Command> {
        let mut commands = Vec::new();
        for &byte in bytes {
            self.state = match (self.state, byte) {
                (State::Data, IAC) => State::Iac,
                (State::Data, byte) => {
                    text.push(byte);
                    State::Data
                }
                // IAC IAC is a literal 255.
                (State::Iac, IAC) => {
                    text.push(IAC);
                    State::Data
                }
                (State::Iac, DO | DONT | WILL | WONT) => State::Verb(byte),
                (State::Iac, SB) => State::Sub,
                (State::Iac, _) => {
                    commands.push(Command::Other);
                    State::Data
                }
                (State::Verb(verb), option) => {
                    commands.push(match verb {
                        DO => Command::Do(option),
                        DONT => Command::Dont(option),
                        WILL => Command::Will(option),
                        _ => Command::Wont(option),
                    });
                    State::Data
                }
                (State::Sub, IAC) => State::SubIac,
                (State::Sub, _) => State::Sub,
                (State::SubIac, SE) => {
                    commands.push(Command::Other);
                    State::Data
                }
                (State::SubIac, _) => State::Sub,
            };
        }
        commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_separated_from_text() {
        let mut parser = Parser::default();
        let mut text = Vec::new();
        let commands = parser.feed(
            &[b'h', IAC, DO, OPT_EOR, b'i', IAC, IAC, IAC, GA, b'\n'],
            &mut text,
        );
        assert_eq!(text, [b'h', b'i', IAC, b'\n']);
        assert_eq!(commands, [Command::Do(OPT_EOR), Command::Other]);
    }

    #[test]
    fn commands_split_across_reads_still_parse() {
        let mut parser = Parser::default();
        let mut text = Vec::new();
        assert!(parser.feed(&[b'a', IAC], &mut text).is_empty());
        assert!(parser.feed(&[WILL], &mut text).is_empty());
        assert_eq!(parser.feed(&[24, b'b'], &mut text), [Command::Will(24)]);
        assert_eq!(text, b"ab");
    }

    #[test]
    fn subnegotiations_are_skipped() {
        let mut parser = Parser::default();
        let mut text = Vec::new();
        let commands = parser.feed(&[IAC, SB, 24, 0, b'x', IAC, SE, b'y'], &mut text);
        assert_eq!(commands, [Command::Other]);
        assert_eq!(text, b"y");
    }
}
