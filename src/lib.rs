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
//! **The client is answered after the whole receive cycle.** It waits on its
//! connection for the response: an empty response on
//! [`transport::Verdict::Accepted`]; on [`transport::Verdict::Refused`] an
//! exception the client does not send again (MODBUS Application Protocol
//! V1.1b3, section 7) — *illegal function* ([`adu::ILLEGAL_FUNCTION`]) for
//! a sender not identified or not permitted, *illegal data value*
//! ([`adu::ILLEGAL_DATA_VALUE`]) for content refused; the exception *server
//! device busy* ([`adu::SERVER_DEVICE_BUSY`]) on
//! [`transport::Verdict::Failed`], which tells it to send the request again.
//! Each request arrives whole, and the connection is kept for the client's
//! next.
//!
//! The origin URI carries what the header knew:
//! `modbus://peer/unit/1?transaction=7`.

pub mod adu;
pub mod connection;

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

pub use adu::{Adu, Header, MAX_PDU, frame};
pub use connection::{Connection, Request};
use transport::ArrivalIdentity;
use transport::Configured;
use transport::error::{Result, TransportError, classify, protocol_error};
use transport::listening::{Accepting, Listening};
use transport::loopback::{FarEnd, LOOPBACK_TIMEOUT, Loopback};
use transport::serving::Serving;
use transport::socket;
use transport::{Arrived, Directions, Taken, Transport};
use xcore::settings::{Applies, Kind, Presence, Setting, Settings};

use crate::adu::{SERVER_DEVICE_BUSY, exception_code, read_adu};

/// Exception code 05, *acknowledge*: the server took a long request and is
/// still working on it.
const ACKNOWLEDGE: u8 = 0x05;

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

/// The failure an exception response says, where the response is one:
/// *acknowledge* and *server device busy* retryable, the rest permanent.
fn refusal(response: &[u8]) -> Option<TransportError> {
    let code = exception_code(response)?;
    let said = format!("the server answered exception {code:#04x}");
    Some(match code {
        ACKNOWLEDGE | SERVER_DEVICE_BUSY => TransportError::retryable(said),
        _ => TransportError::permanent(said),
    })
}

#[derive(Clone)]
pub struct ModbusTransport {
    bind: String,
    timeout: Option<Duration>,
    unit: u8,
    /// The listener the first receive binds, and the clients' connections
    /// kept open on it between their requests.
    receiving: Serving<Connection>,
}

impl ModbusTransport {
    /// Listen or connect at `bind`, addressing unit 1 when sending.
    #[must_use]
    pub fn new(bind: impl Into<String>) -> Self {
        Self {
            bind: bind.into(),
            timeout: None,
            unit: 1,
            receiving: Serving::new(),
        }
    }

    /// The unit identifier a Send Location addresses.
    #[must_use]
    const fn for_unit(mut self, unit: u8) -> Self {
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
        Ok(Connection::new(stream, peer))
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

impl Configured for ModbusTransport {
    /// The address is where a Receive Location listens as a server and
    /// where a Send Location connects as a client.
    const SETTINGS: &'static Settings = &Settings {
        technology: env!("CARGO_PKG_NAME"),
        settings: &[
            Setting {
                name: "unit",
                kind: Kind::Integer {
                    minimum: 0,
                    maximum: 255,
                },
                presence: Presence::Optional,
                meaning: "The unit identifier a Send Location addresses; unit 1 when left out.",
                applies: Applies::Send,
            },
            Setting {
                name: "timeout",
                kind: Kind::Duration,
                presence: Presence::Optional,
                meaning: "How long a connection is waited for and a peer that stops mid-frame.",
                applies: Applies::Both,
            },
        ],
    };

    fn configured(address: &str, settings: &xcore::settings::Read) -> Result<Self> {
        let mut transport = Self::new(address);
        if let Some(unit) = settings.optional_integer("unit") {
            // The declaration holds it within a byte.
            transport = transport.for_unit(u8::try_from(unit).unwrap_or(0));
        }
        Ok(match settings.optional_duration("timeout") {
            Some(timeout) => transport.timing_out_after(timeout),
            None => transport,
        })
    }
}

impl Transport for ModbusTransport {
    fn name(&self) -> &'static str {
        "modbus"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    fn arrivals(&self) -> transport::Arrivals {
        transport::Arrivals::Ordered("a connection's requests are answered in the order they came")
    }

    /// The next request from whichever client sends first, on the listener
    /// the first receive bound and kept, whole. The client waits for its
    /// response until the receive cycle has ended: an empty response on
    /// accepted, the exception *server device busy* on refused. Its
    /// connection is kept for its next request.
    fn receive(&self) -> Result<Vec<Arrived>> {
        let arrived = self.receiving.next(
            || self.bind(),
            self.timeout,
            |stream, peer| Ok(Connection::new(stream, peer)),
            Connection::turn,
        )?;
        Ok(vec![arrived])
    }

    /// One request; an exception response fails the send, retryable where
    /// the server said *acknowledge* or *server device busy*.
    fn send(&self, target: &str, bytes: &[u8]) -> Result<()> {
        let response = self.exchange(target, bytes)?;
        refusal(&response).map_or(Ok(()), Err)
    }
}

impl ModbusTransport {
    /// Both ends on this machine: an ephemeral local port, the loopback
    /// timeout on either side of the connection.
    #[must_use]
    pub fn loopback() -> Self {
        Self::new("127.0.0.1:0").timing_out_after(LOOPBACK_TIMEOUT)
    }
}

impl Accepting for ModbusTransport {
    fn take_one(self, listener: &TcpListener) -> Result<Taken> {
        let mut connection = self.accept_one(listener)?;
        let mut origin = String::from("modbus://");
        let mut bytes = Vec::new();
        while let Some(request) = connection.next_request()? {
            connection.respond(request.header, &request.pdu)?;
            origin = request.origin_uri;
            bytes.extend_from_slice(&request.pdu);
        }
        Ok(Taken::new(origin, bytes).from_peer(connection.peer()))
    }
}

/// A Stream longer than one PDU travels as transactions in turn on one
/// connection, each echoed back before the next goes.
impl Loopback for ModbusTransport {
    fn arrival_identity(&self) -> ArrivalIdentity {
        ArrivalIdentity::PEER
    }

    fn far_end(&self) -> Result<Box<dyn FarEnd>> {
        Ok(Box::new(Listening::new(self.clone(), self.bind()?)))
    }

    fn send_to(&self, address: &str, payload: &[u8]) -> Result<()> {
        let mut client = self.clone().connect(address)?;
        for pdu in payload.chunks(MAX_PDU) {
            if client.request(pdu)? != pdu {
                return Err(protocol_error("the echo differed"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transport::Refusal;
    use transport::payload::edge_payloads;

    #[test]
    fn every_receive_takes_from_the_listener_the_first_bound() {
        let receiver = ModbusTransport::loopback();
        let address = receiver
            .receiving
            .bound(|| receiver.bind())
            .expect("bound")
            .to_string();
        transport::kept::held_across_receives(&receiver, &address, 5, |at, payload| {
            ModbusTransport::loopback().send(at, payload)
        });
    }

    #[test]
    fn a_request_is_answered_by_its_verdict_refused_for_good_failed_busy_then_accepted() {
        const WRITE: &[u8] = &[0x10, 0x00, 0x01, 0x00, 0x01, 0x02, 0x00, 0x2a];
        let receiver = ModbusTransport::loopback();
        let address = receiver
            .receiving
            .bound(|| receiver.bind())
            .expect("bound")
            .to_string();
        // One client sends the request, is answered, and sends it again.
        let client = std::thread::spawn(move || {
            let mut client = ModbusTransport::loopback()
                .connect(&address)
                .expect("connecting");
            [0; 4].map(|_| client.request(WRITE))
        });
        for why in [Refusal::Forbidden, Refusal::Unacceptable] {
            let mut refused = receiver.receive().expect("refused");
            let refused = refused.remove(0);
            assert!(refused.defers(), "the client waits for the verdict");
            refused.refused(why).expect("answered");
        }
        let mut failed = receiver.receive().expect("the third");
        failed.remove(0).failed().expect("busy");
        let mut again = receiver
            .receive()
            .expect("the fourth, on the kept connection");
        let again = again.remove(0).taken().expect("accepted");
        assert_eq!(again.bytes, WRITE);
        assert!(again.origin_uri.ends_with("?transaction=4"));
        let [forbidden, unacceptable, failed, accepted] = client.join().expect("client thread");
        for (refused, code) in [
            (forbidden, adu::ILLEGAL_FUNCTION),
            (unacceptable, adu::ILLEGAL_DATA_VALUE),
        ] {
            let refused = refused.expect("answered");
            assert_eq!(refused, [0x90, code]);
            assert!(!refusal(&refused).expect("a refusal").retryable);
        }
        let failed = failed.expect("answered");
        assert_eq!(failed, [0x90, SERVER_DEVICE_BUSY]);
        assert!(refusal(&failed).expect("a refusal").retryable);
        assert_eq!(accepted.expect("answered"), Vec::<u8>::new());
    }

    #[test]
    fn modbus_declares_its_settings_and_reads_through_them() {
        use xcore::settings::Given;
        assert!(ModbusTransport::SETTINGS.problems().is_empty());
        let given = [
            ("unit".to_string(), Given::Integer(17)),
            ("timeout".to_string(), Given::Text("5s".to_string())),
        ];
        let built = ModbusTransport::open("10.0.0.5:502", Applies::Send, &given).expect("built");
        assert_eq!(built.bind, "10.0.0.5:502");
        assert_eq!(built.unit, 17);
        assert_eq!(built.timeout, Some(Duration::from_secs(5)));
        let Err(refused) = ModbusTransport::open("0.0.0.0:502", Applies::Receive, &given) else {
            panic!("a Receive Location addresses no unit");
        };
        assert!(refused.message.contains("\"unit\""), "{refused}");
    }

    #[test]
    fn a_loopback_round_carries_a_stream_as_transactions() {
        let loopback = ModbusTransport::loopback();
        let arrived = loopback.round(b"read holding").expect("round");
        assert_eq!(arrived.bytes, b"read holding");
        assert!(arrived.origin_uri.contains("/unit/1?transaction=1"));
        let long = vec![0x2a; 1000];
        let arrived = loopback.round(&long).expect("four transactions");
        assert_eq!(arrived.bytes, long);
        assert!(arrived.origin_uri.ends_with("?transaction=4"));
        assert!(loopback.ceiling().is_none());
        assert!(loopback.refuses(&long).is_none());
    }

    #[test]
    fn the_loopback_returns_the_edges_whole() {
        let loopback = ModbusTransport::loopback();
        for (name, bytes) in edge_payloads() {
            let arrived = loopback
                .round(&bytes)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(arrived.bytes, bytes, "{name}");
        }
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
        let request = connection
            .next_request()
            .expect("first")
            .expect("a request");
        assert_eq!(request.pdu, [0x03, 0x00, 0x10, 0x00, 0x02]);
        assert!(request.origin_uri.contains("/unit/9?transaction=1"));
        connection
            .respond(request.header, &[0x03, 0x04, 0x00, 0x2a, 0x00, 0x01])
            .expect("responding");
        let request = connection
            .next_request()
            .expect("second")
            .expect("a request");
        assert!(request.origin_uri.ends_with("?transaction=2"));
        connection
            .respond(request.header, &request.pdu)
            .expect("echoing");
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
