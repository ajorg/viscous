//! Establishing a camera connection and identifying what's on the other end.

use grafton_visca::{
    BlockingClient, Error,
    camera::{Connect, profiles::GenericVisca},
    command::VersionInfo,
    transport::{BlockingTransport, BlockingTransportHandle, HasTransportConfig},
};

/// The prefix that marks a target as VISCA over IP rather than a serial port.
const TCP_SCHEME: &str = "tcp://";

/// What to connect to, and over which transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A serial port name, at a baud rate still to be discovered.
    Serial(String),
    /// A `host[:port]` endpoint speaking VISCA over TCP. The port may be left
    /// off, in which case the generic-VISCA default (5678) is used.
    Tcp(String),
}

/// Reads the single connection string both front ends accept: anything
/// starting with `tcp://` is a network endpoint, and everything else is a
/// serial port name.
///
/// A scheme rather than a guess at the string's shape: serial ports are
/// `/dev/ttyUSB0` and `COM3`, and no rule separating those from hostnames
/// stays true for long.
impl From<&str> for Target {
    fn from(text: &str) -> Self {
        match text.strip_prefix(TCP_SCHEME) {
            Some(address) => Self::Tcp(address.to_string()),
            None => Self::Serial(text.to_string()),
        }
    }
}

/// A camera client over whichever transport [`connect`] used.
///
/// All of grafton-visca's blocking transports hand back the same handle type,
/// so nothing downstream of the connection has to know which one it got.
pub type Camera = BlockingClient<GenericVisca, BlockingTransportHandle>;

/// A live connection, ready to hand to a worker thread.
pub struct Connected {
    /// The connected camera client.
    pub camera: Camera,
    /// How the connection was made, phrased for display ("Connected at 9600
    /// baud").
    pub link: String,
    /// The camera's reply to the version inquiry that proved the link works.
    pub version: VersionInfo,
}

/// Connects to `target` and identifies the camera on the other end, returning
/// a message fit to show the user if that doesn't work out.
pub fn connect(target: &Target) -> Result<Connected, String> {
    match target {
        Target::Serial(port) => connect_serial(port),
        Target::Tcp(address) => connect_tcp(address),
    }
}

/// Connects over serial, which means discovering the baud rate first.
fn connect_serial(port: &str) -> Result<Connected, String> {
    let outcome = discover_baud_rate(DEFAULT_CAMERA_BAUD_RATES, |baud_rate| {
        let camera = Connect::open_serial_blocking::<GenericVisca>(port, baud_rate)?;
        query_version(&camera)
    });

    let ProbeOutcome::Connected { baud_rate, version } = outcome else {
        return Err(format!(
            "No response from camera on {port} at any of the candidate baud rates: \
             {DEFAULT_CAMERA_BAUD_RATES:?}"
        ));
    };

    // Discovery already proved a camera answers at `baud_rate`; reconnect once
    // more for a client the worker thread can hold onto, since the discovery
    // closure's client only lived for the duration of that probe.
    let camera =
        Connect::open_serial_blocking::<GenericVisca>(port, baud_rate).map_err(|error| {
            format!("Connected during discovery but the follow-up connection failed: {error}")
        })?;

    Ok(Connected {
        camera,
        link: format!("Connected at {baud_rate} baud"),
        version,
    })
}

/// Connects over TCP, which needs no discovery — there's no baud rate to
/// guess, so one connection and one version inquiry settles whether a camera
/// is really there.
fn connect_tcp(address: &str) -> Result<Connected, String> {
    let camera = Connect::open_tcp_blocking::<GenericVisca>(address)
        .map_err(|error| format!("Could not reach {address} over TCP: {error}"))?;

    let version = query_version(&camera).map_err(|error| {
        format!("Connected to {address}, but it didn't answer a version inquiry: {error}")
    })?;

    Ok(Connected {
        camera,
        link: format!("Connected to {address} over TCP"),
        version,
    })
}

/// Candidate baud rates to try when a camera's serial configuration isn't
/// known ahead of time, most likely first.
///
/// The EVI-D70's RS-232C VISCA interface is fixed at 9600 baud, but other
/// VISCA-compatible cameras support configurable rates up to 38400.
pub const DEFAULT_CAMERA_BAUD_RATES: &[u32] = &[9600, 38400];

/// The result of probing a camera across one or more candidate baud rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// A candidate baud rate produced a valid version-inquiry reply.
    Connected {
        /// The baud rate that produced a response.
        baud_rate: u32,
        /// The camera's parsed version-inquiry reply.
        version: VersionInfo,
    },
    /// None of the candidate baud rates produced a response.
    NoResponse,
}

/// Queries a connected camera for its VISCA version-inquiry reply.
pub fn query_version<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<VersionInfo, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.system().version()
}

/// Tries each candidate baud rate in order, returning the first one whose
/// connection attempt succeeds.
///
/// `attempt` performs one connection and version-inquiry at the given baud
/// rate. A wrong baud rate is expected to surface as an `Err` (typically a
/// timeout, since the camera's replies won't frame correctly) rather than a
/// failure to open the port, so any error simply moves on to the next
/// candidate.
pub fn discover_baud_rate(
    candidates: &[u32],
    mut attempt: impl FnMut(u32) -> Result<VersionInfo, Error>,
) -> ProbeOutcome {
    for &baud_rate in candidates {
        if let Ok(version) = attempt(baud_rate) {
            return ProbeOutcome::Connected { baud_rate, version };
        }
    }
    ProbeOutcome::NoResponse
}

/// Known VISCA vendor IDs, for naming a vendor instead of just showing its
/// raw number.
///
/// VISCA has no central vendor registry (unlike USB's usb.org); this is the
/// one ID actually documented in practice — Sony's own, from its FCB-series
/// SDK documentation as reproduced by the open-source libVISCA2 project.
/// Anything else just shows as a plain hex number.
const KNOWN_VENDORS: &[(u16, &str)] = &[(0x0020, "Sony")];

fn vendor_name(vendor: u16) -> Option<&'static str> {
    KNOWN_VENDORS
        .iter()
        .find_map(|&(id, name)| (id == vendor).then_some(name))
}

/// Formats a camera's version-inquiry reply for display.
pub fn format_version(info: &VersionInfo) -> String {
    let vendor = match vendor_name(info.vendor) {
        Some(name) => format!("{name} (0x{:04X})", info.vendor),
        None => format!("0x{:04X}", info.vendor),
    };
    format!(
        "vendor={vendor} model=0x{:04X} rom=0x{:08X} max_socket={}",
        info.model, info.rom_version, info.max_socket
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_version() -> VersionInfo {
        VersionInfo {
            vendor: 0x0001,
            model: 0x0002,
            rom_version: 0x0304,
            max_socket: 2,
        }
    }

    #[test]
    fn format_version_renders_fields_as_hex() {
        assert_eq!(
            format_version(&sample_version()),
            "vendor=0x0001 model=0x0002 rom=0x00000304 max_socket=2"
        );
    }

    #[test]
    fn format_version_names_a_known_vendor() {
        let info = VersionInfo {
            vendor: 0x0020,
            ..sample_version()
        };
        assert_eq!(
            format_version(&info),
            "vendor=Sony (0x0020) model=0x0002 rom=0x00000304 max_socket=2"
        );
    }

    #[test]
    fn discover_baud_rate_returns_first_successful_candidate() {
        let outcome = discover_baud_rate(&[9600, 38400], |baud_rate| {
            if baud_rate == 9600 {
                Ok(sample_version())
            } else {
                panic!("should not try a second candidate after the first succeeds")
            }
        });

        assert_eq!(
            outcome,
            ProbeOutcome::Connected {
                baud_rate: 9600,
                version: sample_version(),
            }
        );
    }

    #[test]
    fn discover_baud_rate_falls_back_to_next_candidate_on_error() {
        let outcome = discover_baud_rate(&[9600, 38400], |baud_rate| {
            if baud_rate == 9600 {
                Err(Error::Timeout)
            } else {
                Ok(sample_version())
            }
        });

        assert_eq!(
            outcome,
            ProbeOutcome::Connected {
                baud_rate: 38400,
                version: sample_version(),
            }
        );
    }

    #[test]
    fn discover_baud_rate_reports_no_response_when_all_candidates_fail() {
        let outcome = discover_baud_rate(&[9600, 38400], |_| Err(Error::Timeout));

        assert_eq!(outcome, ProbeOutcome::NoResponse);
    }

    #[test]
    fn a_tcp_url_names_a_network_target() {
        assert_eq!(
            Target::from("tcp://192.168.1.5:5678"),
            Target::Tcp("192.168.1.5:5678".to_string())
        );
        assert_eq!(
            Target::from("tcp://camera.local"),
            Target::Tcp("camera.local".to_string())
        );
    }

    #[test]
    fn anything_else_names_a_serial_port() {
        assert_eq!(
            Target::from("/dev/ttyUSB0"),
            Target::Serial("/dev/ttyUSB0".to_string())
        );
        assert_eq!(Target::from("COM3"), Target::Serial("COM3".to_string()));
    }

    /// Answers a single VISCA version inquiry on a loopback port, standing in
    /// for a camera so the TCP connection path can be exercised for real
    /// rather than through a scripted transport.
    fn serve_one_version_inquiry() -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback port should bind");
        let address = listener
            .local_addr()
            .expect("a bound listener should have an address")
            .to_string();

        let served = std::thread::spawn(move || {
            let (mut client, _) = listener.accept().expect("client should connect");
            let mut inquiry = [0u8; 5];
            client
                .read_exact(&mut inquiry)
                .expect("client should send a version inquiry");
            assert_eq!(inquiry, [0x81, 0x09, 0x00, 0x02, 0xFF]);
            client
                .write_all(&[0x90, 0x50, 0x00, 0x01, 0x00, 0x02, 0x03, 0x04, 0x02, 0xFF])
                .expect("reply should send");
        });

        (address, served)
    }

    #[test]
    fn connecting_over_tcp_identifies_the_camera_that_answers() {
        let (address, served) = serve_one_version_inquiry();

        let connected =
            connect(&Target::Tcp(address.clone())).expect("the loopback camera should answer");

        assert_eq!(connected.version, sample_version());
        assert_eq!(connected.link, format!("Connected to {address} over TCP"));
        served.join().expect("the loopback camera should not panic");
    }

    #[test]
    fn connecting_over_tcp_to_nothing_reports_where_it_failed() {
        // Port 1 on loopback: privileged, so nothing this test could race with
        // is listening there.
        let error = connect(&Target::Tcp("127.0.0.1:1".to_string()))
            .err()
            .expect("connecting to a closed port should fail");

        assert!(
            error.contains("127.0.0.1:1"),
            "the failure should name the address it tried: {error}"
        );
    }

    #[test]
    fn query_version_reads_a_scripted_camera_reply() {
        use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

        let request = vec![0x81, 0x09, 0x00, 0x02, 0xFF];
        let response = vec![0x90, 0x50, 0x00, 0x01, 0x00, 0x02, 0x03, 0x04, 0x02, 0xFF];
        let transport =
            ScriptedBlockingTransport::new(vec![helpers::inquiry_response(request, 1, response)]);

        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        let version = query_version(&camera).expect("scripted camera should reply");

        assert_eq!(version, sample_version());
    }
}
