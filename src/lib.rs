#![forbid(unsafe_code)]

//! Streams that arrive as Modbus TCP application data units. One ADU is one
//! Stream: the MBAP header names the transaction, the unit and the length, and
//! the protocol data unit — function code and data — is what Xmip carries.
//!
//! Modbus is the plant floor's lingua franca: PLCs, meters, drives. Over TCP it
//! is a seven-byte header and a PDU, request and response alike, so this
//! transport is direction-neutral the way ADR-0010 wants: a Receive Location
//! listens as a server and hands each request's PDU up as a Stream; a Send
//! Location connects as a client, sends a PDU and returns the response PDU.
//! A connection carries many transactions in turn, which is how a Stream
//! longer than one PDU travels. Which registers mean what is a contract's
//! business, not this one's.
//!
//! The origin URI carries what the header knew:
//! `modbus://peer/unit/1?transaction=7`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use transport::error::{Result, classify, protocol_error};
use transport::socket;
use transport::{Arrived, Directions, Transport};

/// The Modbus protocol identifier in the MBAP header: always zero.
const PROTOCOL: u16 = 0;
/// The most a PDU may be: Modbus caps an ADU at 260 bytes.
pub const MAX_PDU: usize = 253;

/// The MBAP header, ADU less the PDU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub transaction: u16,
    pub unit: u8,
}

/// One application data unit, split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adu {
    pub header: Header,
    pub pdu: Vec<u8>,
}

/// Frame `pdu` under `header`.
///
/// # Errors
/// A PDU over [`MAX_PDU`] does not fit the length field Modbus gives it.
pub fn frame(header: Header, pdu: &[u8]) -> Result<Vec<u8>> {
    if pdu.len() > MAX_PDU {
        return Err(protocol_error(
            "a PDU over 253 bytes does not fit a Modbus ADU",
        ));
    }
    let length = u16::try_from(pdu.len() + 1).unwrap_or(0);
    let mut out = Vec::with_capacity(7 + pdu.len());
    out.extend_from_slice(&header.transaction.to_be_bytes());
    out.extend_from_slice(&PROTOCOL.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.push(header.unit);
    out.extend_from_slice(pdu);
    Ok(out)
}

/// Read one ADU from `reader`, or `None` when the peer closed between ADUs.
///
/// # Errors
/// A connection that closes mid-frame, a protocol identifier that is not
/// Modbus, or a length outside what an ADU may carry.
pub fn read_adu(reader: &mut impl Read) -> Result<Option<Adu>> {
    let mut head = [0u8; 7];
    let first = reader
        .read(&mut head[..1])
        .map_err(|e| classify("reading the MBAP header", &e))?;
    if first == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut head[1..])
        .map_err(|e| classify("reading the MBAP header", &e))?;
    let transaction = u16::from_be_bytes([head[0], head[1]]);
    let protocol = u16::from_be_bytes([head[2], head[3]]);
    let length = usize::from(u16::from_be_bytes([head[4], head[5]]));
    if protocol != PROTOCOL {
        return Err(protocol_error("a protocol identifier that is not Modbus"));
    }
    if length == 0 || length > MAX_PDU + 1 {
        return Err(protocol_error(
            "a length outside what a Modbus ADU may carry",
        ));
    }
    let mut pdu = vec![0u8; length - 1];
    reader
        .read_exact(&mut pdu)
        .map_err(|e| classify("reading the PDU", &e))?;
    Ok(Some(Adu {
        header: Header {
            transaction,
            unit: head[6],
        },
        pdu,
    }))
}

/// The server's side of one connection: requests in turn, each answered.
pub struct Connection {
    stream: TcpStream,
    peer: SocketAddr,
}

impl Connection {
    /// The next request, or `None` when the client closed the connection.
    ///
    /// # Errors
    /// A malformed ADU, or a connection that broke mid-frame.
    pub fn next_request(&mut self) -> Result<Option<(Arrived, Header)>> {
        let Some(adu) = read_adu(&mut self.stream)? else {
            return Ok(None);
        };
        let origin = format!(
            "modbus://{}/unit/{}?transaction={}",
            self.peer, adu.header.unit, adu.header.transaction
        );
        Ok(Some((Arrived::new(origin, adu.pdu), adu.header)))
    }

    /// Answer a request under its own transaction and unit.
    ///
    /// # Errors
    /// Where the peer went away before the answer, or the PDU does not fit.
    pub fn respond(&mut self, header: Header, pdu: &[u8]) -> Result<()> {
        self.stream
            .write_all(&frame(header, pdu)?)
            .map_err(|e| classify("writing the response", &e))?;
        self.stream
            .flush()
            .map_err(|e| classify("flushing the response", &e))
    }
}

/// The client's side of one connection: transactions numbered in turn.
pub struct Client {
    stream: TcpStream,
    unit: u8,
    transaction: u16,
}

impl Client {
    /// Send one request PDU and return the response PDU.
    ///
    /// # Errors
    /// Where the peer went away, answered malformed, or answered another
    /// transaction.
    pub fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>> {
        self.transaction = self.transaction.wrapping_add(1);
        let header = Header {
            transaction: self.transaction,
            unit: self.unit,
        };
        self.stream
            .write_all(&frame(header, pdu)?)
            .map_err(|e| classify("writing the request", &e))?;
        self.stream
            .flush()
            .map_err(|e| classify("flushing the request", &e))?;
        let response = read_adu(&mut self.stream)?
            .ok_or_else(|| protocol_error("the server closed before answering"))?;
        if response.header.transaction != header.transaction {
            return Err(protocol_error("a response to another transaction"));
        }
        Ok(response.pdu)
    }
}

pub struct ModbusTransport {
    bind: String,
    timeout: Option<Duration>,
    unit: u8,
}

impl ModbusTransport {
    /// Listen or connect at `bind`, addressing unit 1 when sending.
    #[must_use]
    pub fn new(bind: impl Into<String>) -> Self {
        Self {
            bind: bind.into(),
            timeout: None,
            unit: 1,
        }
    }

    /// The unit identifier a Send Location addresses.
    #[must_use]
    pub const fn for_unit(mut self, unit: u8) -> Self {
        self.unit = unit;
        self
    }

    /// Give up on a peer that stops mid-frame.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Bind and report the address actually assigned.
    ///
    /// # Errors
    /// Where the address is taken, malformed, or not permitted.
    pub fn bind(&self) -> Result<(TcpListener, String)> {
        socket::bind_tcp(&self.bind)
    }

    /// Accept one client on an already-bound listener.
    ///
    /// # Errors
    /// Where the connection could not be accepted.
    pub fn accept_one(&self, listener: &TcpListener) -> Result<Connection> {
        let (stream, peer) = socket::accept_tcp(listener, self.timeout)?;
        Ok(Connection { stream, peer })
    }

    /// Connect to `target` as a client.
    ///
    /// # Errors
    /// Where the peer refused or could not be reached.
    pub fn connect(&self, target: &str) -> Result<Client> {
        let stream = socket::connect_tcp(target, self.timeout)?;
        Ok(Client {
            stream,
            unit: self.unit,
            transaction: 0,
        })
    }

    /// One request to `target`, on a connection of its own.
    ///
    /// # Errors
    /// As [`ModbusTransport::connect`] and [`Client::request`].
    pub fn exchange(&self, target: &str, pdu: &[u8]) -> Result<Vec<u8>> {
        self.connect(target)?.request(pdu)
    }
}

impl Transport for ModbusTransport {
    fn name(&self) -> &'static str {
        "modbus"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    /// One client's requests, each acknowledged with an empty response so the
    /// client proceeds. A Location that answers with data drives [`Connection`].
    fn receive(&self) -> Result<Vec<Arrived>> {
        let (listener, _) = self.bind()?;
        let mut connection = self.accept_one(&listener)?;
        let mut arrived = Vec::new();
        while let Some((request, header)) = connection.next_request()? {
            connection.respond(header, &[])?;
            arrived.push(request);
        }
        Ok(arrived)
    }

    fn send(&self, target: &str, bytes: &[u8]) -> Result<()> {
        self.exchange(target, bytes).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_adu_round_trips_and_a_bad_one_is_refused() {
        let header = Header {
            transaction: 7,
            unit: 3,
        };
        let framed = frame(header, &[0x03, 0x00, 0x10, 0x00, 0x02]).expect("frame");
        assert_eq!(framed, [0, 7, 0, 0, 0, 6, 3, 0x03, 0x00, 0x10, 0x00, 0x02]);
        let adu = read_adu(&mut framed.as_slice()).expect("adu").expect("one");
        assert_eq!(adu.header, header);
        assert_eq!(adu.pdu, [0x03, 0x00, 0x10, 0x00, 0x02]);
        assert!(
            read_adu(&mut &[][..]).expect("closed").is_none(),
            "closed between"
        );
        assert!(
            read_adu(&mut &[0, 1, 0, 9, 0, 2, 1, 0][..]).is_err(),
            "not Modbus"
        );
        assert!(
            read_adu(&mut &[0, 1, 0, 0, 0, 0, 1][..]).is_err(),
            "zero length"
        );
        assert!(
            read_adu(&mut &[0, 1, 0, 0][..]).is_err(),
            "closed mid-frame"
        );
        assert!(frame(header, &[0; 254]).is_err(), "too long");
    }

    #[test]
    fn transactions_follow_in_turn_on_one_connection() {
        let server = ModbusTransport::new("127.0.0.1:0").timing_out_after(Duration::from_secs(2));
        let (listener, address) = server.bind().expect("binding");
        let client = std::thread::spawn(move || {
            let mut client = ModbusTransport::new("127.0.0.1:0")
                .for_unit(9)
                .timing_out_after(Duration::from_secs(2))
                .connect(&address)
                .expect("connecting");
            let first = client.request(&[0x03, 0x00, 0x10, 0x00, 0x02]);
            let second = client.request(&[0x06, 0x00, 0x01, 0x00, 0x2a]);
            (first, second)
        });
        let mut connection = server.accept_one(&listener).expect("accepting");
        let (arrived, header) = connection
            .next_request()
            .expect("first")
            .expect("a request");
        assert_eq!(arrived.bytes, [0x03, 0x00, 0x10, 0x00, 0x02]);
        assert!(arrived.origin_uri.contains("/unit/9?transaction=1"));
        connection
            .respond(header, &[0x03, 0x04, 0x00, 0x2a, 0x00, 0x01])
            .expect("responding");
        let (arrived, header) = connection
            .next_request()
            .expect("second")
            .expect("a request");
        assert!(arrived.origin_uri.ends_with("?transaction=2"));
        connection.respond(header, &arrived.bytes).expect("echoing");
        assert!(connection.next_request().expect("closed").is_none());
        let (first, second) = client.join().expect("client thread");
        assert_eq!(first.expect("first"), [0x03, 0x04, 0x00, 0x2a, 0x00, 0x01]);
        assert_eq!(second.expect("second"), [0x06, 0x00, 0x01, 0x00, 0x2a]);
    }

    #[test]
    fn a_listening_socket_has_no_artefact_to_claim() {
        assert!(ModbusTransport::new("127.0.0.1:0").claims().is_none());
        assert_eq!(ModbusTransport::new("127.0.0.1:0").name(), "modbus");
    }
}
