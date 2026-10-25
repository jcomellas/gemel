//! ssh under a pseudo-terminal, so that a graphical interface can answer
//! its prompts: what ssh prints there, and the answers sent back.
//!
//! Ports Unison `src/terminal.ml`, Unison `src/pty.c`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The patterns for OpenSSH's messages ([`password`], [`passphrase`],
//! [`authenticity`]) are the Mac interface's: `uigtk3` shows whatever ssh
//! prints that isn't blank.

use std::sync::OnceLock;

use gemel_base::rx::Rx;

// ---- OpenSSH's messages ----

/// Compiles one of the patterns below, once.
fn pattern(cell: &'static OnceLock<Rx>, re: &str) -> &'static Rx {
    cell.get_or_init(|| Rx::regex(re).expect("a valid pattern"))
}

/// Whether `s` is a password prompt (`passwordRx`).
pub fn password(s: &[u8]) -> bool {
    static RX: OnceLock<Rx> = OnceLock::new();
    pattern(&RX, ".*assword:[ ]*").match_string(s)
}

/// Whether `s` asks for a key's passphrase (`passphraseRx`).
pub fn passphrase(s: &[u8]) -> bool {
    static RX: OnceLock<Rx> = OnceLock::new();
    pattern(&RX, "Enter passphrase for key.*").match_string(s)
}

/// Whether `s` asks whether to trust a host's key (`authenticityRx`).
pub fn authenticity(s: &[u8]) -> bool {
    static RX: OnceLock<Rx> = OnceLock::new();
    pattern(
        &RX,
        "The authenticity of host .* continue connecting \\(yes/no\\)\\? ",
    )
    .match_string(s)
}

// ---- Control sequences ----

/// Where [`process_escapes`] is in a control sequence (`controlSt`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    No,
    Escape,
    EscapeSeq,
    Csi,
    Osc,
    StringSeq,
    OscEsc,
    StringEsc,
}

const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const ESC: u8 = 0x1b;

/// The most spaces a cursor-forward sequence stands for. OCaml has no
/// limit, and would try to add as many as the count says.
const MAX_FORWARD: i64 = 1000;

/// `parseCtrlSeq` (`processEscapes`): `s` without its control sequences,
/// as a terminal would show it. A very small parser of ANSI X3.64 and
/// ECMA-48's 7-bit sequences: they are dropped, but for a cursor forward
/// (CSI n C), which stands for n spaces, and control characters inside
/// them, which are kept.
pub fn process_escapes(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut params = Vec::new();
    let mut st = Control::No;
    // `parseEsc`.
    let escape = |ch: u8, params: &mut Vec<u8>| {
        params.clear();
        match ch {
            0x20..=0x2f => Control::EscapeSeq,
            b'[' => Control::Csi,
            b']' => Control::Osc,
            b'X' | b'^' | b'_' => Control::StringSeq,
            _ => Control::No,
        }
    };
    for &ch in s {
        st = match st {
            Control::No if ch == ESC => Control::Escape,
            Control::No => {
                out.push(ch);
                Control::No
            }
            Control::Escape => escape(ch, &mut params),
            Control::EscapeSeq => match ch {
                CAN | SUB => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::Escape,
                0x30..=0xff => Control::No,
                _ => st,
            },
            Control::Csi => match ch {
                CAN | SUB => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::Escape,
                0x40..=0x7e => {
                    if ch == b'C' {
                        let n = std::str::from_utf8(&params)
                            .ok()
                            .and_then(gemel_base::util::int_of_string)
                            .unwrap_or(1);
                        out.extend(std::iter::repeat_n(b' ', n.clamp(0, MAX_FORWARD) as usize));
                    }
                    Control::No
                }
                0x7f..=0xff => Control::No,
                _ => {
                    params.push(ch);
                    st
                }
            },
            Control::Osc => match ch {
                CAN | SUB | BEL => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::OscEsc,
                _ => st,
            },
            Control::OscEsc | Control::StringEsc => match ch {
                b'\\' => Control::No,
                _ => escape(ch, &mut params),
            },
            Control::StringSeq => match ch {
                CAN | SUB => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::StringEsc,
                _ => st,
            },
        };
    }
    out
}
