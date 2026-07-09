//! Connection abstraction over the two transports provided by `ipmi-rs`:
//! a local kernel device (`/dev/ipmi0`) and a remote LAN session (RMCP/RMCP+).
//!
//! This mirrors the `common.rs` helper from the ipmi-rs examples, extended with a
//! `send_raw` method so we can issue commands (e.g. Chassis Control) that the
//! library does not yet model as typed `IpmiCommand`s.

use std::time::Duration;

use ipmi_rs::connection::{
    Address, Channel, IpmiCommand, IpmiConnection, LogicalUnit, Message, NetFn, Request,
    RequestTargetAddress, Response,
};
use ipmi_rs::rmcp::{
    Rmcp, RmcpIpmiError, RmcpIpmiReceiveError, RmcpIpmiSendError, V1_5WriteError, V2_0WriteError,
};
use ipmi_rs::storage::sdr;
use ipmi_rs::{File, Ipmi, IpmiError, SdrIter};

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

enum SdrIterInner<'a> {
    File(SdrIter<'a, File>),
    Rmcp(SdrIter<'a, Rmcp>),
}

impl Iterator for SdrIterInner<'_> {
    type Item = sdr::Record;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            SdrIterInner::File(it) => it.next(),
            SdrIterInner::Rmcp(it) => it.next(),
        }
    }
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

    /// Iterate over all Sensor Data Records in the repository.
    pub fn sdrs(&mut self) -> impl Iterator<Item = sdr::Record> + '_ {
        match self {
            Conn::File(ipmi) => SdrIterInner::File(ipmi.sdrs()),
            Conn::Rmcp(ipmi) => SdrIterInner::Rmcp(ipmi.sdrs()),
        }
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
