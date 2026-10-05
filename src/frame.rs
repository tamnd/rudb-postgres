//! The frame reader and writer, independent of rudb.
//!
//! A frame is one type byte, a 4-byte big-endian length that counts itself, and a body. The
//! startup messages have no type byte. This module reads and writes frames and builds the
//! frontend messages. It does not interpret a backend message beyond what the client needs to
//! follow the exchange, because the comparison works on the bytes.

use std::io::{self, Read};

/// The largest frame the harness accepts. PostgreSQL allows 1 GB for a field, and no case here
/// comes near it, so a larger length means that the stream is out of step.
const MAX_FRAME: usize = 1 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) tag: u8,
    pub(crate) body: Vec<u8>,
}

impl Frame {
    pub(crate) fn new(tag: u8, body: Vec<u8>) -> Frame {
        Frame { tag, body }
    }

    /// The frame as bytes on the wire.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + self.body.len());
        out.push(self.tag);
        out.extend_from_slice(&((self.body.len() + 4) as u32).to_be_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    /// A reader over the body, for the messages that the client has to understand.
    pub(crate) fn fields(&self) -> Fields<'_> {
        Fields { rest: &self.body }
    }
}

/// Reads one tagged frame.
pub(crate) fn read(stream: &mut impl Read) -> io::Result<Frame> {
    let mut head = [0u8; 5];
    stream.read_exact(&mut head)?;
    let length = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if !(4..=MAX_FRAME).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a frame of type {:?} with length {length}", head[0] as char),
        ));
    }
    let mut body = vec![0u8; length - 4];
    stream.read_exact(&mut body)?;
    Ok(Frame { tag: head[0], body })
}

/// The largest startup packet that PostgreSQL accepts, `MAX_STARTUP_PACKET_LENGTH`.
const MAX_STARTUP: usize = 10_000;

/// Reads one message without a type byte, as a server reads it before the startup: a
/// `StartupMessage`, an `SSLRequest`, a `GSSENCRequest` or a `CancelRequest`. The bytes include
/// the length, as `PQtrace` takes them.
pub(crate) fn read_untagged(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    let length = u32::from_be_bytes(head) as usize;
    if !(8..=MAX_STARTUP).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a startup packet with length {length}"),
        ));
    }
    let mut message = vec![0u8; length];
    message[..4].copy_from_slice(&head);
    stream.read_exact(&mut message[4..])?;
    Ok(message)
}

/// The protocol version 3.0, as the startup message carries it.
pub(crate) const PROTOCOL_3_0: u32 = 196_608;
/// The request code of `CancelRequest`, in the place of the protocol version.
pub(crate) const CANCEL_REQUEST: u32 = 80_877_102;
/// The request code of `SSLRequest`.
pub(crate) const SSL_REQUEST: u32 = 80_877_103;
/// The request code of `GSSENCRequest`.
pub(crate) const GSSENC_REQUEST: u32 = 80_877_104;

/// The bytes of a `StartupMessage`.
pub(crate) fn startup(version: u32, parameters: &[(&str, &str)]) -> Vec<u8> {
    let mut body = version.to_be_bytes().to_vec();
    for (name, value) in parameters {
        cstr(&mut body, name);
        cstr(&mut body, value);
    }
    body.push(0);
    untagged(body)
}

fn untagged(body: Vec<u8>) -> Vec<u8> {
    let mut out = ((body.len() + 4) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

pub(crate) fn query(sql: &str) -> Frame {
    let mut body = Vec::new();
    cstr(&mut body, sql);
    Frame::new(b'Q', body)
}

/// `Parse` with the type OIDs of the parameters, zero for "let the server decide".
pub(crate) fn parse(name: &str, sql: &str, types: &[u32]) -> Frame {
    let mut body = Vec::new();
    cstr(&mut body, name);
    cstr(&mut body, sql);
    body.extend_from_slice(&(types.len() as u16).to_be_bytes());
    for t in types {
        body.extend_from_slice(&t.to_be_bytes());
    }
    Frame::new(b'P', body)
}

/// `Bind` with one format code for all parameters and one for all results. A parameter of
/// `None` is NULL.
pub(crate) fn bind(
    portal: &str,
    statement: &str,
    parameter_format: u16,
    parameters: &[Option<Vec<u8>>],
    result_format: u16,
) -> Frame {
    let mut body = Vec::new();
    cstr(&mut body, portal);
    cstr(&mut body, statement);
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&parameter_format.to_be_bytes());
    body.extend_from_slice(&(parameters.len() as u16).to_be_bytes());
    for parameter in parameters {
        match parameter {
            Some(value) => {
                body.extend_from_slice(&(value.len() as i32).to_be_bytes());
                body.extend_from_slice(value);
            }
            None => body.extend_from_slice(&(-1i32).to_be_bytes()),
        }
    }
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&result_format.to_be_bytes());
    Frame::new(b'B', body)
}

/// `Describe` of a statement (`S`) or a portal (`P`).
pub(crate) fn describe(kind: u8, name: &str) -> Frame {
    let mut body = vec![kind];
    cstr(&mut body, name);
    Frame::new(b'D', body)
}

pub(crate) fn execute(portal: &str, max_rows: u32) -> Frame {
    let mut body = Vec::new();
    cstr(&mut body, portal);
    body.extend_from_slice(&max_rows.to_be_bytes());
    Frame::new(b'E', body)
}

pub(crate) fn sync() -> Frame {
    Frame::new(b'S', Vec::new())
}

pub(crate) fn copy_data(data: &[u8]) -> Frame {
    Frame::new(b'd', data.to_vec())
}

pub(crate) fn copy_done() -> Frame {
    Frame::new(b'c', Vec::new())
}

pub(crate) fn copy_fail(message: &str) -> Frame {
    let mut body = Vec::new();
    cstr(&mut body, message);
    Frame::new(b'f', body)
}

pub(crate) fn terminate() -> Frame {
    Frame::new(b'X', Vec::new())
}

/// `PasswordMessage`, which also carries the SASL responses.
pub(crate) fn password(bytes: &[u8]) -> Frame {
    Frame::new(b'p', bytes.to_vec())
}

pub(crate) fn sasl_initial(mechanism: &str, data: &[u8]) -> Frame {
    let mut body = Vec::new();
    cstr(&mut body, mechanism);
    body.extend_from_slice(&(data.len() as i32).to_be_bytes());
    body.extend_from_slice(data);
    Frame::new(b'p', body)
}

fn cstr(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// A cursor over a message body.
#[derive(Debug)]
pub(crate) struct Fields<'a> {
    rest: &'a [u8],
}

impl<'a> Fields<'a> {
    pub(crate) fn u8(&mut self) -> Option<u8> {
        let (&b, rest) = self.rest.split_first()?;
        self.rest = rest;
        Some(b)
    }

    pub(crate) fn i16(&mut self) -> Option<i16> {
        Some(i16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    pub(crate) fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.rest.len() < n {
            return None;
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Some(head)
    }

    /// A NUL-terminated string, without the NUL.
    pub(crate) fn cstr(&mut self) -> Option<&'a [u8]> {
        let end = self.rest.iter().position(|&b| b == 0)?;
        let s = &self.rest[..end];
        self.rest = &self.rest[end + 1..];
        Some(s)
    }

    pub(crate) fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.rest)
    }
}

/// The fields of an `ErrorResponse` or a `NoticeResponse`, as (code, value) pairs in wire order.
pub(crate) fn notice_fields(frame: &Frame) -> Vec<(u8, String)> {
    let mut fields = frame.fields();
    let mut out = Vec::new();
    while let Some(code) = fields.u8() {
        if code == 0 {
            break;
        }
        let Some(value) = fields.cstr() else { break };
        out.push((code, String::from_utf8_lossy(value).into_owned()));
    }
    out
}

/// The name of a backend message type, as `PQtrace` prints it.
pub(crate) fn backend_name(tag: u8) -> &'static str {
    match tag {
        b'R' => "Authentication",
        b'K' => "BackendKeyData",
        b'2' => "BindComplete",
        b'3' => "CloseComplete",
        b'C' => "CommandComplete",
        b'd' => "CopyData",
        b'c' => "CopyDone",
        b'G' => "CopyInResponse",
        b'H' => "CopyOutResponse",
        b'W' => "CopyBothResponse",
        b'D' => "DataRow",
        b'I' => "EmptyQueryResponse",
        b'E' => "ErrorResponse",
        b'V' => "FunctionCallResponse",
        b'v' => "NegotiateProtocolVersion",
        b'n' => "NoData",
        b'N' => "NoticeResponse",
        b'A' => "NotificationResponse",
        b't' => "ParameterDescription",
        b'S' => "ParameterStatus",
        b'1' => "ParseComplete",
        b's' => "PortalSuspended",
        b'Z' => "ReadyForQuery",
        b'T' => "RowDescription",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_back_what_was_written() {
        let frame = query("select 1");
        let bytes = frame.bytes();
        assert_eq!(&bytes[..5], &[b'Q', 0, 0, 0, 13]);
        assert_eq!(read(&mut &bytes[..]).unwrap(), frame);
    }

    #[test]
    fn a_length_below_four_is_an_error() {
        let bytes = [b'Z', 0, 0, 0, 3];
        assert!(read(&mut &bytes[..]).is_err());
    }

    #[test]
    fn the_startup_message_has_the_layout_of_the_protocol() {
        let bytes = startup(PROTOCOL_3_0, &[("user", "u")]);
        assert_eq!(bytes, [0, 0, 0, 16, 0, 3, 0, 0, b'u', b's', b'e', b'r', 0, b'u', 0, 0]);
    }

    #[test]
    fn notice_fields_come_out_in_wire_order() {
        let frame = Frame::new(b'E', b"SERROR\0C42P01\0Mno\0\0".to_vec());
        let fields = notice_fields(&frame);
        assert_eq!(fields, [(b'S', "ERROR".into()), (b'C', "42P01".into()), (b'M', "no".into())]);
    }
}
