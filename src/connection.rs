//! The server's side of one Modbus TCP connection: requests in turn, each
//! answered, and kept open between them.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};

use transport::answer::Answer;
use transport::error::{Result, classify};
use transport::serving::{Open, Turn};
use transport::{Acknowledgement, Arrived, Refusal, Verdict};

use crate::adu::{
    Header, ILLEGAL_DATA_VALUE, ILLEGAL_FUNCTION, SERVER_DEVICE_BUSY, exception, frame, read_adu,
};

/// One request as it arrived, not yet answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// `modbus://peer/unit/1?transaction=7`.
    pub origin_uri: String,
    pub header: Header,
    pub pdu: Vec<u8>,
}

/// The server's side of one connection: requests in turn, each answered.
pub struct Connection {
    stream: TcpStream,
    peer: SocketAddr,
}

impl Connection {
    /// A connection a client opened from `peer`.
    #[must_use]
    pub const fn new(stream: TcpStream, peer: SocketAddr) -> Self {
        Self { stream, peer }
    }

    /// The client the connection is from.
    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// The next request, or `None` when the client closed the connection.
    ///
    /// # Errors
    /// A malformed ADU, or a connection that broke mid-frame.
    pub fn next_request(&mut self) -> Result<Option<Request>> {
        let Some(adu) = read_adu(&mut self.stream)? else {
            return Ok(None);
        };
        Ok(Some(Request {
            origin_uri: format!(
                "modbus://{}/unit/{}?transaction={}",
                self.peer, adu.header.unit, adu.header.transaction
            ),
            header: adu.header,
            pdu: adu.pdu,
        }))
    }

    /// The next request as a Stream whose verdict answers it: an empty
    /// response on accepted; the exception *illegal function* or *illegal
    /// data value* on refused, which the client does not send again; the
    /// exception *server device busy* on failed, so the client sends it
    /// again; let go without a verdict, the
    /// connection is shut ([`Answer`]). `None` when the client closed.
    ///
    /// # Errors
    /// As [`Connection::next_request`], or the connection could not be
    /// held for the answer.
    pub fn next_arrival(&mut self) -> Result<Option<Arrived>> {
        let Some(request) = self.next_request()? else {
            return Ok(None);
        };
        let held = Answer::held(&self.stream)?;
        let Request {
            origin_uri,
            header,
            pdu,
        } = request;
        let function = pdu.first().copied().unwrap_or_default();
        let acknowledgement = Acknowledgement::deferred(move |verdict| {
            let answer = match verdict {
                Verdict::Accepted => Vec::new(),
                Verdict::Refused(Refusal::Unidentified | Refusal::Forbidden) => {
                    exception(function, ILLEGAL_FUNCTION).to_vec()
                }
                Verdict::Refused(Refusal::Unacceptable) => {
                    exception(function, ILLEGAL_DATA_VALUE).to_vec()
                }
                Verdict::Failed => exception(function, SERVER_DEVICE_BUSY).to_vec(),
            };
            held.with(|stream| write_response(stream, header, &answer))
        });
        Ok(Some(
            Arrived::whole(origin_uri, pdu, acknowledgement).from_peer(self.peer),
        ))
    }

    /// One turn for a kept listener: the next arrival, or the client gone.
    ///
    /// # Errors
    /// As [`Connection::next_arrival`].
    pub fn turn(&mut self) -> Result<Turn<Arrived>> {
        Ok(self.next_arrival()?.map_or(Turn::Closed, Turn::Taken))
    }

    /// Answer a request under its own transaction and unit.
    ///
    /// # Errors
    /// Where the peer went away before the answer, or the PDU does not fit.
    pub fn respond(&mut self, header: Header, pdu: &[u8]) -> Result<()> {
        write_response(&mut self.stream, header, pdu)
    }
}

impl Open for Connection {
    fn socket(&self) -> &TcpStream {
        &self.stream
    }
}

fn write_response(stream: &mut TcpStream, header: Header, pdu: &[u8]) -> Result<()> {
    stream
        .write_all(&frame(header, pdu)?)
        .map_err(|e| classify("writing the response", &e))?;
    stream
        .flush()
        .map_err(|e| classify("flushing the response", &e))
}
