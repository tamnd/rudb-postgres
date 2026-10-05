//! The trace format: one line for each frame, in the format of libpq's `PQtrace`.
//!
//! The format is the one of `src/interfaces/libpq/fe-trace.c` at the pin, without the
//! timestamps. A line has the direction (`F` for a frontend message, `B` for a backend message),
//! the length, the message name and the fields, with a tab between the first four parts. This
//! module writes the format in two styles.
//!
//! - [`Style::Regress`] is `PQtrace` with `PQTRACE_REGRESS_MODE`, byte for byte. It replaces the
//!   OIDs and some strings with `NNNN` and `SSSS`. `libpq_pipeline` writes this style, so a trace
//!   from the proxy in this style must be equal to the trace that libpq writes for the same
//!   session.
//! - [`Style::Exact`] keeps every value. It also writes a backslash, the quote character of the
//!   field and each byte that is not printable as `\xNN`, so that [`read`] can make the bytes of
//!   a frontend message again from its line. The trace files of the proxy use this style.
//!
//! The design is document 14 section 14.4 of the PostgreSQL compatibility notes.

use crate::frame::{self, Frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    Regress,
    Exact,
}

/// The kind of the next `p` message. The frontend sends four messages with the type byte `p`,
/// and only the authentication request before it tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthResponse {
    Unknown,
    Gss,
    Password,
    SaslInitial,
    Sasl,
}

/// Writes the lines of one session. It follows the authentication exchange, as libpq does, to
/// name each `p` message.
pub(crate) struct Tracer {
    style: Style,
    auth: AuthResponse,
}

impl Tracer {
    pub(crate) fn new(style: Style) -> Tracer {
        Tracer { style, auth: AuthResponse::Unknown }
    }

    /// The line of one tagged frame, without the line end. A frame that does not have the
    /// layout of its type gets a second line, as in libpq.
    pub(crate) fn frame(&mut self, frontend: bool, frame: &Frame) -> Vec<u8> {
        let regress = self.style == Style::Regress;
        let length = frame.body.len() + 4;
        let mut out = Out::new(self.style, &frame.body);
        let tag = frame.tag;
        out.text(if frontend { "F\t" } else { "B\t" });
        if regress && !frontend && (tag == b'E' || tag == b'N') {
            out.text("NN\t");
        } else {
            out.text(&format!("{length}\t"));
        }
        match (tag, frontend) {
            (b'1', _) => out.text("ParseComplete"),
            (b'2', _) => out.text("BindComplete"),
            (b'3', _) => out.text("CloseComplete"),
            (b'A', _) => {
                out.text("NotificationResponse\t");
                out.int32(regress);
                out.string(false);
                out.string(false);
            }
            (b'B', _) => {
                out.text("Bind\t");
                out.string(false);
                out.string(false);
                let formats = out.int16();
                out.repeat(formats, |out| {
                    out.int16();
                });
                let values = out.int16();
                out.repeat(values, |out| {
                    let n = out.int32(false);
                    if n != -1 {
                        out.nchar(n, false);
                    }
                });
                let formats = out.int16();
                out.repeat(formats, |out| {
                    out.int16();
                });
            }
            (b'c', _) => out.text("CopyDone"),
            (b'C', true) => {
                out.text("Close\t");
                out.byte1();
                out.string(false);
            }
            (b'C', false) => {
                out.text("CommandComplete\t");
                out.string(false);
            }
            (b'd', _) => {
                out.text("CopyData\t");
                out.rest(regress);
            }
            (b'D', true) => {
                out.text("Describe\t");
                out.byte1();
                out.string(false);
            }
            (b'D', false) => {
                out.text("DataRow\t");
                let fields = out.int16();
                out.repeat(fields, |out| {
                    let n = out.int32(false);
                    if n != -1 {
                        out.nchar(n, false);
                    }
                });
            }
            (b'E', true) => {
                out.text("Execute\t");
                out.string(false);
                out.int32(false);
            }
            (b'E', false) => out.notice("ErrorResponse", regress),
            (b'f', _) => {
                out.text("CopyFail\t");
                out.string(false);
            }
            (b'p', _) => {
                match self.auth {
                    AuthResponse::Gss => {
                        out.text("GSSResponse\t");
                        out.rest(regress);
                    }
                    AuthResponse::Password => {
                        out.text("PasswordMessage\t");
                        out.string(false);
                    }
                    AuthResponse::SaslInitial => {
                        out.text("SASLInitialResponse\t");
                        out.string(false);
                        let n = out.int32(false);
                        if n != -1 {
                            out.nchar(n, regress);
                        }
                    }
                    AuthResponse::Sasl => {
                        out.text("SASLResponse\t");
                        out.rest(regress);
                    }
                    AuthResponse::Unknown => out.text("UnknownAuthenticationResponse"),
                }
                self.auth = AuthResponse::Unknown;
            }
            (b'F', _) => {
                out.text("FunctionCall\t");
                out.int32(regress);
                let formats = out.int16();
                out.repeat(formats, |out| {
                    out.int16();
                });
                let values = out.int16();
                out.repeat(values, |out| {
                    let n = out.int32(false);
                    if n != -1 {
                        out.nchar(n, false);
                    }
                });
                out.int16();
            }
            (b'G', _) | (b'H', false) => {
                out.text(if tag == b'G' { "CopyInResponse\t" } else { "CopyOutResponse\t" });
                out.byte1();
                let formats = out.int16();
                out.repeat(formats, |out| {
                    out.int16();
                });
            }
            (b'H', true) => out.text("Flush"),
            (b'I', _) => out.text("EmptyQueryResponse"),
            (b'K', _) => {
                out.text("BackendKeyData\t");
                out.int32(regress);
                out.rest(regress);
            }
            (b'n', _) => out.text("NoData"),
            (b'N', _) => out.notice("NoticeResponse", regress),
            (b'P', _) => {
                out.text("Parse\t");
                out.string(false);
                out.string(false);
                let types = out.int16();
                out.repeat(types, |out| {
                    out.int32(regress);
                });
            }
            (b'Q', _) => {
                out.text("Query\t");
                out.string(false);
            }
            (b'R', _) => self.authentication(&mut out, regress),
            (b's', _) => out.text("PortalSuspended"),
            (b'S', true) => out.text("Sync"),
            (b'S', false) => {
                out.text("ParameterStatus\t");
                out.string(false);
                out.string(false);
            }
            (b't', _) => {
                out.text("ParameterDescription\t");
                let types = out.int16();
                out.repeat(types, |out| {
                    out.int32(regress);
                });
            }
            (b'T', _) => {
                out.text("RowDescription\t");
                let fields = out.int16();
                out.repeat(fields, |out| {
                    out.string(false);
                    out.int32(regress);
                    out.int16();
                    out.int32(regress);
                    out.int16();
                    out.int32(false);
                    out.int16();
                });
            }
            (b'v', _) => {
                out.text("NegotiateProtocolVersion\t");
                out.int32(false);
                let options = out.int32(false);
                out.repeat(options, |out| out.string(false));
            }
            (b'V', _) => {
                out.text("FunctionCallResponse\t");
                let n = out.int32(false);
                if n != -1 {
                    out.nchar(n, false);
                }
            }
            (b'W', _) => {
                out.text("CopyBothResponse\t");
                out.byte1();
                while length > out.cursor + 1 && !out.short {
                    out.int16();
                }
            }
            (b'X', _) => out.text("Terminate"),
            (b'Z', _) => {
                out.text("ReadyForQuery\t");
                out.byte1();
            }
            _ => out.text(&format!("Unknown message: {tag:02x}")),
        }
        if out.short || out.cursor != frame.body.len() {
            out.text(&format!(
                "\nmismatched message length: consumed {}, expected {length}",
                out.cursor + 4
            ));
        }
        out.buf
    }

    fn authentication(&mut self, out: &mut Out<'_>, regress: bool) {
        let kind = out.read(4).map_or(-1, |b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        match kind {
            0 => out.text("AuthenticationOk"),
            3 => out.text("AuthenticationCleartextPassword"),
            5 => out.text("AuthenticationMD5Password"),
            7 => out.text("AuthenticationGSS"),
            8 => {
                out.text("AuthenticationGSSContinue\t");
                out.rest(regress);
            }
            9 => out.text("AuthenticationSSPI"),
            10 => {
                out.text("AuthenticationSASL\t");
                while !out.short && out.data.get(out.cursor).is_some_and(|b| *b != 0) {
                    out.string(false);
                }
                out.string(false);
            }
            11 => {
                out.text("AuthenticationSASLContinue\t");
                out.rest(regress);
            }
            12 => {
                out.text("AuthenticationSASLFinal\t");
                out.rest(regress);
            }
            _ => out.text(&format!("Unknown authentication message {kind}")),
        }
        self.auth = match kind {
            3 | 5 => AuthResponse::Password,
            7..=9 => AuthResponse::Gss,
            10 => AuthResponse::SaslInitial,
            11 => AuthResponse::Sasl,
            _ => self.auth,
        };
    }

    /// The line of a message without a type byte, which only a client sends. The bytes include
    /// the length.
    pub(crate) fn untagged(&self, message: &[u8]) -> Vec<u8> {
        let regress = self.style == Style::Regress;
        let mut out = Out::new(self.style, message);
        let length = out.read(4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        out.text(&format!("F\t{length}\t"));
        if length < 8 {
            out.text("Unknown message");
            return out.buf;
        }
        let code = message.get(4..8).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        match code {
            frame::CANCEL_REQUEST if length >= 16 => {
                out.text("CancelRequest\t");
                out.int16();
                out.int16();
                out.int32(regress);
                out.nchar(length as i32 - out.cursor as i32, regress);
            }
            frame::SSL_REQUEST | frame::GSSENC_REQUEST => {
                out.text(if code == frame::SSL_REQUEST {
                    "SSLRequest\t"
                } else {
                    "GSSENCRequest\t"
                });
                out.int16();
                out.int16();
            }
            _ => {
                out.text("StartupMessage\t");
                out.int16();
                out.int16();
                while !out.short && out.data.get(out.cursor).is_some_and(|b| *b != 0) {
                    out.string(false);
                    out.string(false);
                }
            }
        }
        out.buf
    }

    /// The line of the one byte that a server sends to answer `SSLRequest` or `GSSENCRequest`.
    pub(crate) fn char_response(&self, name: &str, response: u8) -> Vec<u8> {
        format!("B\t1\t{name}\t {}", response as char).into_bytes()
    }
}

/// A line under construction and a cursor over the body of its message.
struct Out<'a> {
    style: Style,
    buf: Vec<u8>,
    data: &'a [u8],
    cursor: usize,
    /// The body ended before the layout of its type. The values after that point are zero.
    short: bool,
}

impl<'a> Out<'a> {
    fn new(style: Style, data: &'a [u8]) -> Out<'a> {
        Out { style, buf: Vec::new(), data, cursor: 0, short: false }
    }

    fn text(&mut self, text: &str) {
        self.buf.extend_from_slice(text.as_bytes());
    }

    fn read(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.data.get(self.cursor..self.cursor + n);
        match bytes {
            Some(_) => self.cursor += n,
            None => {
                self.short = true;
                self.cursor = self.data.len();
            }
        }
        bytes
    }

    fn hex(&mut self, b: u8) {
        self.text(&format!("\\x{b:02x}"));
    }

    /// Runs `field` once for each of `count` values, and stops when the body ends.
    fn repeat(&mut self, count: i32, mut field: impl FnMut(&mut Self)) {
        for _ in 0..count.max(0) {
            if self.short {
                break;
            }
            field(self);
        }
    }

    fn byte1(&mut self) {
        let b = self.read(1).map_or(0, |b| b[0]);
        self.buf.push(b' ');
        let plain = match self.style {
            Style::Regress => printable(b),
            Style::Exact => b.is_ascii_graphic() && b != b'\\',
        };
        if plain { self.buf.push(b) } else { self.hex(b) }
    }

    /// A 2-byte integer. libpq reads it as unsigned, so -1 prints as 65535.
    fn int16(&mut self) -> i32 {
        let value = self.read(2).map_or(0, |b| i32::from(u16::from_be_bytes([b[0], b[1]])));
        self.text(&format!(" {value}"));
        value
    }

    fn int32(&mut self, suppress: bool) -> i32 {
        let value = self.read(4).map_or(0, |b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        if suppress {
            self.text(" NNNN");
        } else {
            self.text(&format!(" {value}"));
        }
        value
    }

    fn string(&mut self, suppress: bool) {
        let rest = &self.data[self.cursor..];
        let (value, used) = match rest.iter().position(|b| *b == 0) {
            Some(end) => (&rest[..end], end + 1),
            None => {
                self.short = true;
                (rest, rest.len())
            }
        };
        self.cursor += used;
        if suppress {
            self.text(" \"SSSS\"");
            return;
        }
        self.text(" \"");
        for &b in value {
            match self.style {
                Style::Regress => self.buf.push(b),
                Style::Exact if printable(b) && b != b'\\' && b != b'"' => self.buf.push(b),
                Style::Exact => self.hex(b),
            }
        }
        self.buf.push(b'"');
    }

    /// Exactly `n` bytes.
    fn nchar(&mut self, n: i32, suppress: bool) {
        let n = usize::try_from(n).unwrap_or_else(|_| {
            self.short = true;
            0
        });
        let value = self.read(n).unwrap_or(&[]);
        if suppress {
            self.text(" 'BBBB'");
            return;
        }
        self.text(" '");
        for &b in value {
            let plain = match self.style {
                Style::Regress => printable(b),
                Style::Exact => printable(b) && b != b'\\' && b != b'\'',
            };
            if plain { self.buf.push(b) } else { self.hex(b) }
        }
        self.buf.push(b'\'');
    }

    /// The bytes from the cursor to the end of the body.
    fn rest(&mut self, suppress: bool) {
        let n = self.data.len() - self.cursor;
        self.nchar(n as i32, suppress);
    }

    /// `ErrorResponse` and `NoticeResponse`. Regress mode hides the file, the line and the
    /// routine, because they change when the server code changes.
    fn notice(&mut self, name: &str, regress: bool) {
        self.text(name);
        self.text("\t");
        loop {
            self.byte1();
            let field = self.data.get(self.cursor.wrapping_sub(1)).copied().unwrap_or(0);
            if field == 0 || self.short {
                break;
            }
            self.string(regress && matches!(field, b'L' | b'F' | b'R'));
        }
    }
}

/// `isprint` in the C locale.
fn printable(b: u8) -> bool {
    (0x20..=0x7e).contains(&b)
}

/// One line of a trace file in the exact style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    /// A message that the client sent, as the bytes on the wire.
    Frontend(Vec<u8>),
    /// A message that the server sent, as its line. The replay compares lines.
    Backend(Vec<u8>),
}

/// Reads a trace file in the exact style. Empty lines and lines that start with `#` are notes
/// and not frames, so a person can write a trace by hand.
pub(crate) fn read(text: &[u8]) -> Result<Vec<Line>, String> {
    let mut lines = Vec::new();
    for (number, line) in text.split(|b| *b == b'\n').enumerate() {
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let at = |e: String| format!("line {}: {e}", number + 1);
        if line.starts_with(b"B\t") {
            lines.push(Line::Backend(line.to_vec()));
        } else if line.starts_with(b"mismatched message length") {
            // The second line of a backend frame with a bad layout belongs to that frame.
            match lines.last_mut() {
                Some(Line::Backend(previous)) => {
                    previous.push(b'\n');
                    previous.extend_from_slice(line);
                }
                _ => return Err(at("a frontend message has a bad layout".into())),
            }
        } else if let Some(rest) = line.strip_prefix(b"F\t") {
            lines.push(Line::Frontend(encode(rest).map_err(at)?));
        } else {
            return Err(at("a line starts with F, B or #".into()));
        }
    }
    Ok(lines)
}

/// The values of a `DataRow` line in the exact style, with `None` for a null. It is `None` when
/// the line is not a `DataRow` or does not read.
pub(crate) fn data_row(line: &[u8]) -> Option<Vec<Option<Vec<u8>>>> {
    let mut parts = line.strip_prefix(b"B\t")?.splitn(3, |b| *b == b'\t');
    parts.next()?;
    if parts.next()? != b"DataRow" {
        return None;
    }
    let mut fields = Tokens::new(parts.next()?).ok()?;
    let mut values = Vec::new();
    for _ in 0..fields.number().ok()? {
        let length = fields.number().ok()?;
        if length < 0 {
            values.push(None);
        } else {
            let mut value = Vec::new();
            fields.nchar(&mut value, i32::try_from(length).ok()).ok()?;
            values.push(Some(value));
        }
    }
    fields.done().then_some(values)
}

/// Makes the bytes of a frontend message from its line without the `F` part.
fn encode(line: &[u8]) -> Result<Vec<u8>, String> {
    let mut parts = line.splitn(3, |b| *b == b'\t');
    let length = parts.next().unwrap_or_default();
    let length: usize = std::str::from_utf8(length)
        .ok()
        .and_then(|l| l.parse().ok())
        .ok_or("the length is not a number")?;
    let name = std::str::from_utf8(parts.next().unwrap_or_default())
        .map_err(|_| "the message name is not text")?;
    let mut fields = Tokens::new(parts.next().unwrap_or_default())?;
    let f = &mut fields;
    let mut body = Vec::new();
    let tag = match name {
        "Query" => {
            f.string(&mut body)?;
            b'Q'
        }
        "Parse" => {
            f.string(&mut body)?;
            f.string(&mut body)?;
            for _ in 0..f.int16(&mut body)? {
                f.int32(&mut body)?;
            }
            b'P'
        }
        "Bind" | "FunctionCall" => {
            if name == "Bind" {
                f.string(&mut body)?;
                f.string(&mut body)?;
            } else {
                f.int32(&mut body)?;
            }
            for _ in 0..f.int16(&mut body)? {
                f.int16(&mut body)?;
            }
            for _ in 0..f.int16(&mut body)? {
                let n = f.int32(&mut body)?;
                if n != -1 {
                    f.nchar(&mut body, Some(n))?;
                }
            }
            if name == "Bind" {
                for _ in 0..f.int16(&mut body)? {
                    f.int16(&mut body)?;
                }
                b'B'
            } else {
                f.int16(&mut body)?;
                b'F'
            }
        }
        "Describe" | "Close" => {
            f.byte(&mut body)?;
            f.string(&mut body)?;
            if name == "Describe" { b'D' } else { b'C' }
        }
        "Execute" => {
            f.string(&mut body)?;
            f.int32(&mut body)?;
            b'E'
        }
        "Sync" => b'S',
        "Flush" => b'H',
        "Terminate" => b'X',
        "CopyDone" => b'c',
        "CopyData" => {
            f.nchar(&mut body, None)?;
            b'd'
        }
        "CopyFail" => {
            f.string(&mut body)?;
            b'f'
        }
        "PasswordMessage" => {
            f.string(&mut body)?;
            b'p'
        }
        "SASLInitialResponse" => {
            f.string(&mut body)?;
            let n = f.int32(&mut body)?;
            if n != -1 {
                f.nchar(&mut body, Some(n))?;
            }
            b'p'
        }
        "SASLResponse" | "GSSResponse" => {
            f.nchar(&mut body, None)?;
            b'p'
        }
        "StartupMessage" | "SSLRequest" | "GSSENCRequest" | "CancelRequest" => {
            let mut message = vec![0; 4];
            f.int16(&mut message)?;
            f.int16(&mut message)?;
            match name {
                "CancelRequest" => {
                    f.int32(&mut message)?;
                    f.nchar(&mut message, None)?;
                }
                "StartupMessage" => {
                    while !f.done() {
                        f.string(&mut message)?;
                        f.string(&mut message)?;
                    }
                    message.push(0);
                }
                _ => {}
            }
            f.end()?;
            let n = message.len();
            message[..4].copy_from_slice(&(n as u32).to_be_bytes());
            if n != length {
                return Err(format!("the fields make {n} bytes, and the line says {length}"));
            }
            return Ok(message);
        }
        other => return Err(format!("{other:?} is not a frontend message that a trace can send")),
    };
    f.end()?;
    if body.len() + 4 != length {
        return Err(format!(
            "the fields make {} bytes, and the line says {length}",
            body.len() + 4
        ));
    }
    Ok(Frame::new(tag, body).bytes())
}

/// The fields of one line in the exact style. Each field starts with a space.
struct Tokens {
    tokens: Vec<(u8, Vec<u8>)>,
    next: usize,
}

impl Tokens {
    fn new(text: &[u8]) -> Result<Tokens, String> {
        let mut tokens = Vec::new();
        let mut i = 0;
        while i < text.len() {
            if text[i] != b' ' {
                return Err(format!("a field does not start with a space at byte {i}"));
            }
            i += 1;
            let quote = text.get(i).copied().unwrap_or(0);
            let (kind, end) = if quote == b'"' || quote == b'\'' {
                let close = text[i + 1..]
                    .iter()
                    .position(|b| *b == quote)
                    .ok_or("a quoted field has no end")?;
                (quote, i + 1 + close + 1)
            } else {
                (b' ', text[i..].iter().position(|b| *b == b' ').map_or(text.len(), |p| i + p))
            };
            let raw = if kind == b' ' { &text[i..end] } else { &text[i + 1..end - 1] };
            tokens.push((kind, unescape(raw)?));
            i = end;
        }
        Ok(Tokens { tokens, next: 0 })
    }

    fn take(&mut self, kind: u8, what: &str) -> Result<Vec<u8>, String> {
        let (found, value) = self.tokens.get(self.next).ok_or(format!("{what} is missing"))?;
        if *found != kind {
            return Err(format!("field {} is not {what}", self.next + 1));
        }
        self.next += 1;
        Ok(value.clone())
    }

    fn number(&mut self) -> Result<i64, String> {
        let value = self.take(b' ', "a number")?;
        std::str::from_utf8(&value)
            .ok()
            .and_then(|v| v.parse().ok())
            .ok_or(format!("field {} is not a number", self.next))
    }

    fn int16(&mut self, out: &mut Vec<u8>) -> Result<i64, String> {
        let value = self.number()?;
        let bits = u16::try_from(value)
            .or_else(|_| i16::try_from(value).map(|v| v as u16))
            .map_err(|_| format!("{value} does not fit in 2 bytes"))?;
        out.extend_from_slice(&bits.to_be_bytes());
        Ok(i64::from(bits))
    }

    fn int32(&mut self, out: &mut Vec<u8>) -> Result<i32, String> {
        let value = self.number()?;
        let bits = i32::try_from(value)
            .or_else(|_| u32::try_from(value).map(|v| v as i32))
            .map_err(|_| format!("{value} does not fit in 4 bytes"))?;
        out.extend_from_slice(&bits.to_be_bytes());
        Ok(bits)
    }

    fn byte(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        match self.take(b' ', "a byte")?.as_slice() {
            [b] => {
                out.push(*b);
                Ok(())
            }
            _ => Err(format!("field {} is not one byte", self.next)),
        }
    }

    fn string(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        let value = self.take(b'"', "a string")?;
        if value.contains(&0) {
            return Err(format!("string {} has a zero byte", self.next));
        }
        out.extend_from_slice(&value);
        out.push(0);
        Ok(())
    }

    /// A byte field, with the length from the field before it when the layout has one.
    fn nchar(&mut self, out: &mut Vec<u8>, length: Option<i32>) -> Result<(), String> {
        let value = self.take(b'\'', "a byte string")?;
        if let Some(n) = length
            && usize::try_from(n).ok() != Some(value.len())
        {
            return Err(format!("field {} has {} bytes, not {n}", self.next, value.len()));
        }
        out.extend_from_slice(&value);
        Ok(())
    }

    fn done(&self) -> bool {
        self.next == self.tokens.len()
    }

    fn end(&self) -> Result<(), String> {
        if self.done() {
            Ok(())
        } else {
            Err(format!("the line has {} fields after the layout", self.tokens.len() - self.next))
        }
    }
}

/// Reads the `\xNN` escapes of the exact style.
fn unescape(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            let hex = raw
                .get(i + 1..i + 4)
                .filter(|h| h[0] == b'x')
                .and_then(|h| std::str::from_utf8(&h[1..]).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or("a backslash is not the start of \\xNN")?;
            out.push(hex);
            i += 4;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Line, Style, Tracer, read};
    use crate::frame::{self, Frame};

    fn line(style: Style, frontend: bool, tag: u8, body: &[u8]) -> String {
        let bytes = Tracer::new(style).frame(frontend, &Frame::new(tag, body.to_vec()));
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn the_regress_style_is_the_style_of_libpq() {
        // The lines come from src/test/modules/libpq_pipeline/traces/prepared.trace.
        let mut parse = b"select_one\0SELECT $1\0".to_vec();
        parse.extend_from_slice(&[0, 1, 0, 0, 0, 23]);
        assert_eq!(
            line(Style::Regress, true, b'P', &parse),
            "F\t31\tParse\t \"select_one\" \"SELECT $1\" 1 NNNN"
        );
        let mut row = vec![0, 1];
        row.extend_from_slice(b"numeric\0");
        row.extend_from_slice(&[
            0, 0, 0, 0, 0, 0, 0, 0, 6, 164, 255, 255, 255, 255, 255, 255, 0, 0,
        ]);
        assert_eq!(
            line(Style::Regress, false, b'T', &row),
            "B\t32\tRowDescription\t 1 \"numeric\" NNNN 0 NNNN 65535 -1 0"
        );
        let error = b"SERROR\0VERROR\0C26000\0Mno \"x\"\0Ffile.c\0L1\0Rf\0\0";
        assert_eq!(
            line(Style::Regress, false, b'E', error),
            "B\tNN\tErrorResponse\t S \"ERROR\" V \"ERROR\" C \"26000\" M \"no \"x\"\" F \"SSSS\" L \"SSSS\" R \"SSSS\" \\x00"
        );
        assert_eq!(line(Style::Regress, false, b'Z', b"I"), "B\t5\tReadyForQuery\t I");
        assert_eq!(line(Style::Regress, true, b'S', b""), "F\t4\tSync");
        assert_eq!(
            line(Style::Regress, false, b'S', b"a\0b\0"),
            "B\t8\tParameterStatus\t \"a\" \"b\""
        );
    }

    #[test]
    fn a_short_frame_gets_the_line_of_libpq() {
        assert_eq!(
            line(Style::Exact, false, b'Z', b""),
            "B\t4\tReadyForQuery\t \\x00\nmismatched message length: consumed 4, expected 4"
        );
    }

    #[test]
    fn the_exact_style_reads_back_to_the_same_bytes() {
        let frames = [
            frame::query("select '\"quoted\"', E'\\\\', 'tab\there'"),
            frame::parse("s1", "select $1::int", &[23]),
            frame::bind("", "s1", 1, &[Some(vec![0, 0, 0, 7]), None], 0),
            frame::describe(b'P', ""),
            frame::execute("", 0),
            frame::sync(),
            frame::copy_data(b"1\t'a'\\N\n"),
            frame::copy_done(),
            frame::copy_fail("stop"),
            frame::terminate(),
        ];
        let mut tracer = Tracer::new(Style::Exact);
        let mut text = Vec::new();
        for f in &frames {
            text.extend_from_slice(&tracer.frame(true, f));
            text.push(b'\n');
        }
        let startup = frame::startup(frame::PROTOCOL_3_0, &[("user", "rpg"), ("database", "x y")]);
        text.extend_from_slice(&tracer.untagged(&startup));
        text.extend_from_slice(b"\n# a note\nB\t5\tReadyForQuery\t I\n");
        let lines = read(&text).unwrap();
        assert_eq!(lines.len(), frames.len() + 2);
        for (f, line) in frames.iter().zip(&lines) {
            assert_eq!(line, &Line::Frontend(f.bytes()));
        }
        assert_eq!(lines[frames.len()], Line::Frontend(startup));
        assert_eq!(lines[frames.len() + 1], Line::Backend(b"B\t5\tReadyForQuery\t I".to_vec()));
    }

    #[test]
    fn the_authentication_names_the_password_message() {
        let mut tracer = Tracer::new(Style::Exact);
        let mut request = 10i32.to_be_bytes().to_vec();
        request.extend_from_slice(b"SCRAM-SHA-256\0\0");
        let text = tracer.frame(false, &Frame::new(b'R', request));
        assert_eq!(text, b"B\t23\tAuthenticationSASL\t \"SCRAM-SHA-256\" \"\"");
        let initial = frame::sasl_initial("SCRAM-SHA-256", b"n,,n=,r=abc");
        let text = String::from_utf8(tracer.frame(true, &initial)).unwrap();
        assert_eq!(text, "F\t33\tSASLInitialResponse\t \"SCRAM-SHA-256\" 11 'n,,n=,r=abc'");
        let text = String::from_utf8(tracer.frame(true, &initial)).unwrap();
        assert_eq!(
            text,
            "F\t33\tUnknownAuthenticationResponse\nmismatched message length: consumed 4, expected 33"
        );
    }

    #[test]
    fn a_data_row_line_gives_its_values() {
        let mut body = 3i16.to_be_bytes().to_vec();
        for value in [Some(&b"16384"[..]), None, Some(&b"it's"[..])] {
            match value {
                Some(v) => {
                    body.extend_from_slice(&(v.len() as i32).to_be_bytes());
                    body.extend_from_slice(v);
                }
                None => body.extend_from_slice(&(-1i32).to_be_bytes()),
            }
        }
        let line = line(Style::Exact, false, b'D', &body);
        let values = super::data_row(line.as_bytes()).unwrap();
        assert_eq!(values, [Some(b"16384".to_vec()), None, Some(b"it's".to_vec())]);
        assert!(super::data_row(b"B\t5\tReadyForQuery\t I").is_none());
    }

    #[test]
    fn a_bad_line_is_an_error() {
        assert!(read(b"F\t9\tQuery\t \"x\"").is_err());
        assert!(read(b"F\t6\tQuery\t \"x\"").is_ok());
        assert!(read(b"X\tsomething").is_err());
        assert!(read(b"F\t4\tCommandComplete").is_err());
    }
}
