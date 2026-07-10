//! Connection abstraction over the two transports provided by `ipmi-rs`:
//! a local kernel device (`/dev/ipmi0`) and a remote LAN session (RMCP/RMCP+).
//!
//! This mirrors the `common.rs` helper from the ipmi-rs examples, extended with a
//! `send_raw` method so we can issue commands (e.g. Chassis Control) that the
//! library does not yet model as typed `IpmiCommand`s.

use std::collections::HashSet;
use std::time::Duration;

use ipmi_rs::connection::{
    Address, Channel, IpmiCommand, IpmiConnection, LogicalUnit, Message, NetFn, Request,
    RequestTargetAddress, Response,
};
use ipmi_rs::rmcp::{
    Rmcp, RmcpIpmiError, RmcpIpmiReceiveError, RmcpIpmiSendError, V1_5WriteError, V2_0WriteError,
};
use ipmi_rs::storage::sdr;
use ipmi_rs::{File, Ipmi, IpmiError};

const CMD_RESERVE_SDR_REPOSITORY: u8 = 0x22;
const CMD_GET_SDR: u8 = 0x23;
const CC_CANNOT_RETURN_REQUESTED_BYTES: u8 = 0xCA;
const SDR_HEADER_LEN: usize = 5;
const INITIAL_SDR_CHUNK_SIZE: usize = 32;

/// How the user asked us to reach the BMC.
pub enum Target {
    /// Local kernel device, e.g. `/dev/ipmi0`.
    Device(String),
    /// Remote LAN session.
    Lan {
        address: String,
        username: String,
        password: String,
    },
}

/// An open IPMI connection, generic over the underlying transport.
pub enum Conn {
    File(Ipmi<File>),
    // Boxed: the RMCP session is much larger than the File handle.
    Rmcp(Box<Ipmi<Rmcp>>),
}

fn io_err<T>(val: T) -> std::io::Error
where
    T: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    std::io::Error::other(val)
}

/// Collapse the rich RMCP error type into a plain `io::Error`, preserving the
/// underlying IO error where one exists (matching the ipmi-rs examples).
fn map_rmcp(e: RmcpIpmiError) -> std::io::Error {
    match e {
        RmcpIpmiError::Receive(RmcpIpmiReceiveError::Io(io))
        | RmcpIpmiError::Send(RmcpIpmiSendError::V1_5(V1_5WriteError::Io(io)))
        | RmcpIpmiError::Send(RmcpIpmiSendError::V2_0(V2_0WriteError::Io(io))) => io,
        other => io_err(format!("{other:?}")),
    }
}

impl Conn {
    /// Open a connection to the given target.
    pub fn connect(target: &Target, timeout: Duration) -> std::io::Result<Self> {
        match target {
            Target::Device(path) => {
                let file = File::new(path, timeout)
                    .map_err(|e| io_err(format!("could not open IPMI device '{path}': {e}")))?;
                Ok(Conn::File(Ipmi::new(file)))
            }
            Target::Lan {
                address,
                username,
                password,
            } => {
                let mut rmcp = Rmcp::new(address.as_str(), timeout)
                    .map_err(|e| io_err(format!("could not reach BMC at '{address}': {e}")))?;
                rmcp.activate(true, Some(username), Some(password.as_bytes()))
                    .map_err(|e| io_err(format!("RMCP authentication failed: {e:?}")))?;
                Ok(Conn::Rmcp(Box::new(Ipmi::new(rmcp))))
            }
        }
    }

    /// Send a typed command and receive its parsed response.
    pub fn send_recv<CMD>(
        &mut self,
        request: CMD,
    ) -> Result<CMD::Output, IpmiError<std::io::Error, CMD::Error>>
    where
        CMD: IpmiCommand,
    {
        match self {
            Conn::File(ipmi) => ipmi.send_recv(request),
            Conn::Rmcp(ipmi) => ipmi.send_recv(request).map_err(|e| e.map(map_rmcp)),
        }
    }

    /// Collect the SDR repository while rejecting repeated record IDs.
    ///
    /// Normally each record is requested in one operation. Some BMCs (notably
    /// Cisco CIMC) reject the conventional `bytes to read = 0xFF` request with
    /// completion code 0xCA. Only in that case, reserve the repository and
    /// switch to bounded reads for the rest of this scan.
    pub fn collect_sdrs(&mut self) -> Result<Vec<sdr::Record>, String> {
        let mut records = Vec::new();
        let mut seen = HashSet::new();
        let mut record_id = 0u16;
        let mut chunked = None;

        while record_id != u16::MAX {
            if !seen.insert(record_id) {
                return Err(format!(
                    "SDR repository cycle detected at record 0x{record_id:04X}"
                ));
            }

            let result = match chunked.as_mut() {
                Some(state) => self.read_sdr_chunked(record_id, state),
                None => match self.read_sdr_full(record_id)? {
                    FullSdrRead::Record(result) => Ok(result),
                    FullSdrRead::NeedsChunks => {
                        let reservation = self.reserve_sdr_repository()?;
                        let mut state = ChunkedSdrRead {
                            reservation,
                            chunk_size: INITIAL_SDR_CHUNK_SIZE,
                        };
                        let result = self.read_sdr_chunked(record_id, &mut state);
                        chunked = Some(state);
                        result
                    }
                },
            }?;

            let parsed = sdr::Record::parse(&result.data).map_err(|error| {
                format!("failed to parse SDR record 0x{record_id:04X}: {error:?}")
            })?;
            // Record ID 0x0000 is the protocol's "first record" selector, so
            // the first returned record may have a different concrete ID.
            if !sdr_record_id_matches(record_id, parsed.header.id.value()) {
                return Err(format!(
                    "SDR record ID mismatch: requested 0x{record_id:04X}, received 0x{:04X}",
                    parsed.header.id.value()
                ));
            }
            records.push(parsed);
            record_id = result.next_id;
        }
        Ok(records)
    }

    fn read_sdr_full(&mut self, record_id: u16) -> Result<FullSdrRead, String> {
        let response = self
            .send_raw(
                NetFn::Storage,
                CMD_GET_SDR,
                sdr_request(0, record_id, 0, 0xFF),
            )
            .map_err(|error| format!("Get SDR 0x{record_id:04X} failed: {error}"))?;
        if response.cc() == CC_CANNOT_RETURN_REQUESTED_BYTES {
            return Ok(FullSdrRead::NeedsChunks);
        }
        if response.cc() != 0 {
            return Err(format!(
                "Get SDR 0x{record_id:04X} failed: completion code 0x{:02X}",
                response.cc()
            ));
        }
        Ok(FullSdrRead::Record(parse_sdr_response(
            record_id,
            response.data(),
        )?))
    }

    fn reserve_sdr_repository(&mut self) -> Result<u16, String> {
        let response = self
            .send_raw(NetFn::Storage, CMD_RESERVE_SDR_REPOSITORY, vec![])
            .map_err(|error| format!("Reserve SDR Repository failed: {error}"))?;
        if response.cc() != 0 {
            return Err(format!(
                "Reserve SDR Repository failed: completion code 0x{:02X}",
                response.cc()
            ));
        }
        let data = response.data();
        if data.len() < 2 {
            return Err("Reserve SDR Repository returned a short response".to_string());
        }
        Ok(u16::from_le_bytes([data[0], data[1]]))
    }

    fn read_sdr_chunked(
        &mut self,
        record_id: u16,
        state: &mut ChunkedSdrRead,
    ) -> Result<SdrRead, String> {
        let header_response =
            self.read_sdr_chunk(state.reservation, record_id, 0, SDR_HEADER_LEN as u8)?;
        if header_response.data.len() != SDR_HEADER_LEN {
            return Err(format!(
                "Get SDR 0x{record_id:04X} returned {} header bytes, expected {SDR_HEADER_LEN}",
                header_response.data.len()
            ));
        }

        let total_len = SDR_HEADER_LEN + header_response.data[4] as usize;
        let next_id = header_response.next_id;
        let mut data = header_response.data;
        while data.len() < total_len {
            let remaining = total_len - data.len();
            let mut requested = remaining.min(state.chunk_size);
            let response = loop {
                let response = self
                    .send_raw(
                        NetFn::Storage,
                        CMD_GET_SDR,
                        sdr_request(
                            state.reservation,
                            record_id,
                            data.len() as u8,
                            requested as u8,
                        ),
                    )
                    .map_err(|error| {
                        format!(
                            "Get SDR 0x{record_id:04X} at offset {} failed: {error}",
                            data.len()
                        )
                    })?;
                if response.cc() == CC_CANNOT_RETURN_REQUESTED_BYTES && requested > 1 {
                    requested = (requested / 2).max(1);
                    state.chunk_size = state.chunk_size.min(requested);
                    continue;
                }
                if response.cc() != 0 {
                    return Err(format!(
                        "Get SDR 0x{record_id:04X} at offset {} failed: completion code 0x{:02X}",
                        data.len(),
                        response.cc()
                    ));
                }
                break parse_sdr_response(record_id, response.data())?;
            };
            if response.next_id != next_id {
                return Err(format!(
                    "SDR repository changed while reading record 0x{record_id:04X}"
                ));
            }
            if response.data.is_empty() {
                return Err(format!(
                    "Get SDR 0x{record_id:04X} returned no data at offset {}",
                    data.len()
                ));
            }
            data.extend_from_slice(&response.data);
        }
        data.truncate(total_len);
        Ok(SdrRead { next_id, data })
    }

    fn read_sdr_chunk(
        &mut self,
        reservation: u16,
        record_id: u16,
        offset: u8,
        count: u8,
    ) -> Result<SdrRead, String> {
        let response = self
            .send_raw(
                NetFn::Storage,
                CMD_GET_SDR,
                sdr_request(reservation, record_id, offset, count),
            )
            .map_err(|error| {
                format!("Get SDR 0x{record_id:04X} at offset {offset} failed: {error}")
            })?;
        if response.cc() != 0 {
            return Err(format!(
                "Get SDR 0x{record_id:04X} at offset {offset} failed: completion code 0x{:02X}",
                response.cc()
            ));
        }
        parse_sdr_response(record_id, response.data())
    }

    /// Send a raw request to the BMC (LUN 0) and return the raw response.
    ///
    /// Used for commands not yet modelled by ipmi-rs, such as Chassis Control
    /// and the FRU read commands.
    pub fn send_raw(&mut self, netfn: NetFn, cmd: u8, data: Vec<u8>) -> std::io::Result<Response> {
        self.send_raw_target(
            netfn,
            cmd,
            data,
            RequestTargetAddress::Bmc(LogicalUnit::Zero),
        )
    }

    /// Send a raw request to a specific BMC or IPMB target.
    pub fn send_raw_to(
        &mut self,
        netfn: NetFn,
        cmd: u8,
        data: Vec<u8>,
        address: Address,
        channel: Channel,
        lun: LogicalUnit,
    ) -> std::io::Result<Response> {
        self.send_raw_target(
            netfn,
            cmd,
            data,
            RequestTargetAddress::BmcOrIpmb(address, channel, lun),
        )
    }

    fn send_raw_target(
        &mut self,
        netfn: NetFn,
        cmd: u8,
        data: Vec<u8>,
        target: RequestTargetAddress,
    ) -> std::io::Result<Response> {
        let message = Message::new_request(netfn, cmd, data);
        let mut request = Request::new(message, target);
        match self {
            Conn::File(ipmi) => ipmi.inner_mut().send_recv(&mut request),
            Conn::Rmcp(ipmi) => ipmi.inner_mut().send_recv(&mut request).map_err(map_rmcp),
        }
    }
}

enum FullSdrRead {
    Record(SdrRead),
    NeedsChunks,
}

struct ChunkedSdrRead {
    reservation: u16,
    chunk_size: usize,
}

struct SdrRead {
    next_id: u16,
    data: Vec<u8>,
}

fn sdr_request(reservation: u16, record_id: u16, offset: u8, count: u8) -> Vec<u8> {
    let mut request = Vec::with_capacity(6);
    request.extend_from_slice(&reservation.to_le_bytes());
    request.extend_from_slice(&record_id.to_le_bytes());
    request.push(offset);
    request.push(count);
    request
}

fn parse_sdr_response(record_id: u16, response: &[u8]) -> Result<SdrRead, String> {
    if response.len() < 2 {
        return Err(format!(
            "Get SDR 0x{record_id:04X} returned a short response"
        ));
    }
    Ok(SdrRead {
        next_id: u16::from_le_bytes([response[0], response[1]]),
        data: response[2..].to_vec(),
    })
}

fn sdr_record_id_matches(requested: u16, actual: u16) -> bool {
    requested == 0 || requested == actual
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_get_sdr_request() {
        assert_eq!(
            sdr_request(0x1234, 0x5678, 5, 32),
            [0x34, 0x12, 0x78, 0x56, 5, 32]
        );
    }

    #[test]
    fn separates_next_record_id_from_sdr_data() {
        let response = parse_sdr_response(1, &[2, 0, 1, 0, 0x51, 1, 0x3A]).unwrap();
        assert_eq!(response.next_id, 2);
        assert_eq!(response.data, [1, 0, 0x51, 1, 0x3A]);
    }

    #[test]
    fn first_record_selector_accepts_concrete_record_id() {
        assert!(sdr_record_id_matches(0, 1));
        assert!(sdr_record_id_matches(2, 2));
        assert!(!sdr_record_id_matches(2, 3));
    }
}
