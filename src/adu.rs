//! The Modbus TCP application data unit: the MBAP header, the PDU, and the
//! exception response a server answers a request it did not carry out with.

use std::io::Read;

use transport::error::{Result, classify, protocol_error};

/// The Modbus protocol identifier in the MBAP header: always zero.
const PROTOCOL: u16 = 0;
/// The most a PDU may be: Modbus caps an ADU at 260 bytes.
pub const MAX_PDU: usize = 253;
/// The bit an exception response sets on the request's function code.
const EXCEPTION: u8 = 0x80;
/// Exception code 01, *illegal function*: the request is not an allowable
/// action for this server — what a sender not identified or not permitted
/// is answered; the client does not send it again (MODBUS Application
/// Protocol Specification V1.1b3, section 7).
pub const ILLEGAL_FUNCTION: u8 = 0x01;
/// Exception code 03, *illegal data value*: the request's data is not
/// allowable for this server — what content refused is answered; the
/// client does not send it again (section 7).
pub const ILLEGAL_DATA_VALUE: u8 = 0x03;
/// Exception code 06, *server device busy*: the server could not carry the
/// request out now, and the client sends it again later (section 7).
pub const SERVER_DEVICE_BUSY: u8 = 0x06;

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

/// The exception response to a request whose function code is `function`.
#[must_use]
pub const fn exception(function: u8, code: u8) -> [u8; 2] {
    [function | EXCEPTION, code]
}

/// The exception code a response PDU carries, where it is one.
#[must_use]
pub fn exception_code(pdu: &[u8]) -> Option<u8> {
    match pdu {
        [function, code, ..] if function & EXCEPTION != 0 => Some(*code),
        _ => None,
    }
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
    fn an_exception_response_sets_the_function_codes_high_bit() {
        assert_eq!(exception(0x10, SERVER_DEVICE_BUSY), [0x90, 0x06]);
        assert_eq!(exception_code(&[0x90, 0x06]), Some(SERVER_DEVICE_BUSY));
        assert_eq!(exception_code(&[0x10, 0x00, 0x01]), None);
        assert_eq!(exception_code(&[]), None);
    }
}
