//! Establishing a camera connection and identifying what's on the other end.

use std::time::Duration;

use grafton_visca::{
    Error, TokioRuntime, TransportHandle,
    camera::{CameraConfig, CameraSession, profiles::GenericVisca},
    command::{InquiryData, VersionInfo, VersionInquiry},
    mode::Async,
    transport::{BackoffStrategy, RetryConfig},
};

use crate::wire::{self, Wire};

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

/// A camera session over whichever transport [`open`] used.
///
/// All of grafton-visca's Tokio transports hand back the same handle type, so
/// nothing downstream of the connection has to know which one it got.
pub type Session = CameraSession<Async, GenericVisca, TransportHandle<TokioRuntime>, TokioRuntime>;

/// A live connection.
pub struct Opened {
    /// The camera session, which the worker owns from here on.
    pub session: Session,
    /// How the connection was made, phrased for display ("COM4 at 9600
    /// baud").
    pub link: String,
    /// The camera's reply to the version inquiry that proved the link works.
    pub version: VersionInfo,
}

/// Connects to `target` and identifies the camera on the other end, returning
/// a message fit to show the user if that doesn't work out.
///
/// Must run inside a Tokio runtime, which goes on serving the session.
pub async fn open(target: &Target) -> Result<Opened, String> {
    let runtime = TokioRuntime::from_current()
        .map_err(|error| format!("No runtime to connect in: {error}"))?;
    match target {
        Target::Serial(port) => open_serial(port, runtime).await,
        Target::Tcp(address) => open_tcp(address, runtime).await,
    }
}

/// Connects over serial, which means discovering the baud rate first.
async fn open_serial(port: &str, runtime: TokioRuntime) -> Result<Opened, String> {
    for &baud_rate in DEFAULT_CAMERA_BAUD_RATES {
        // A wrong baud rate surfaces as a version inquiry that goes
        // unanswered, since the camera's replies won't frame, rather than as
        // a failure to open the port; either way, on to the next.
        let Ok(session) = CameraConfig::<GenericVisca>::serial(port, baud_rate)
            .retry_config(NEVER_RESEND)
            .open_serial_async(runtime.clone())
            .await
        else {
            continue;
        };
        match query_version(session.camera()).await {
            Ok(version) => {
                return Ok(Opened {
                    session,
                    link: format!("{port} at {baud_rate} baud"),
                    version,
                });
            }
            // Let go of the port before opening it again at the next rate.
            Err(_) => drop(session.close().await),
        }
    }
    Err(format!(
        "No response from camera on {port} at any of the candidate baud rates: \
         {DEFAULT_CAMERA_BAUD_RATES:?}"
    ))
}

/// Connects over TCP, which needs no discovery — there's no baud rate to
/// guess, so one connection and one version inquiry settles whether a camera
/// is really there.
async fn open_tcp(address: &str, runtime: TokioRuntime) -> Result<Opened, String> {
    let session = CameraConfig::<GenericVisca>::tcp(address)
        .retry_config(NEVER_RESEND)
        .open_async(runtime)
        .await
        .map_err(|error| format!("Could not reach {address} over TCP: {error}"))?;

    let version = query_version(session.camera()).await.map_err(|error| {
        format!("Connected to {address}, but it didn't answer a version inquiry: {error}")
    })?;

    Ok(Opened {
        session,
        link: format!("{address} over TCP"),
        version,
    })
}

/// The library left to resend nothing that moves the camera.
///
/// The worker stops waiting for a command that goes unanswered and gets on
/// with the next, and the library, left to itself, would send the abandoned
/// one again later — a drive resent after the stop that followed it leaves
/// the camera moving with nobody asking it to. A command the operator still
/// wants is one they ask for again. (The library still repeats unanswered
/// questions, which is harmless: asking twice changes nothing.)
pub(crate) const NEVER_RESEND: RetryConfig = RetryConfig {
    max_retries: 0,
    base_retry_delay: Duration::from_millis(100),
    max_retry_duration: Duration::from_secs(10),
    backoff_strategy: BackoffStrategy::Exponential,
};

/// Candidate baud rates to try when a camera's serial configuration isn't
/// known ahead of time, most likely first.
///
/// The EVI-D70's RS-232C VISCA interface is fixed at 9600 baud, but other
/// VISCA-compatible cameras support configurable rates up to 38400.
pub const DEFAULT_CAMERA_BAUD_RATES: &[u32] = &[9600, 38400];

/// Asks a connected camera for its VISCA version-inquiry reply.
pub async fn query_version(camera: &impl Wire) -> Result<VersionInfo, Error> {
    match wire::ask(camera, VersionInquiry).await? {
        InquiryData::Version {
            vendor,
            model,
            rom_version,
            max_socket,
        } => Ok(VersionInfo {
            vendor,
            model,
            rom_version,
            max_socket,
        }),
        other => Err(Error::ParseError(
            format!("asked for the version, got {other:?}").into(),
        )),
    }
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

/// Names the camera that answered, in as many words as a header line can
/// spare: who made it, and nothing else.
///
/// The rest of the version reply — model, ROM, socket count — identifies a
/// firmware build rather than a camera, which is worth having to hand but not
/// worth the width of a window.
pub fn format_camera(info: &VersionInfo) -> String {
    match vendor_name(info.vendor) {
        Some(name) => name.to_string(),
        None => format!("vendor 0x{:04X}", info.vendor),
    }
}

/// Formats a camera's version-inquiry reply in full, for a transcript or a
/// tooltip with room for it.
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
    fn a_camera_is_named_by_who_made_it() {
        let mut info = sample_version();
        info.vendor = 0x0020;
        assert_eq!(format_camera(&info), "Sony");
        assert_eq!(format_camera(&sample_version()), "vendor 0x0001");
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

    #[tokio::test]
    async fn connecting_over_tcp_identifies_the_camera_that_answers() {
        let (address, served) = serve_one_version_inquiry();

        let Ok(opened) = open(&Target::Tcp(address.clone())).await else {
            panic!("the loopback camera should answer");
        };

        assert_eq!(opened.version, sample_version());
        assert_eq!(opened.link, format!("{address} over TCP"));
        served.join().expect("the loopback camera should not panic");
    }

    #[tokio::test]
    async fn connecting_over_tcp_to_nothing_reports_where_it_failed() {
        // Port 1 on loopback: privileged, so nothing this test could race with
        // is listening there.
        let Err(error) = open(&Target::Tcp("127.0.0.1:1".to_string())).await else {
            panic!("connecting to a closed port should fail");
        };

        assert!(
            error.contains("127.0.0.1:1"),
            "the failure should name the address it tried: {error}"
        );
    }

    #[tokio::test]
    async fn query_version_reads_a_scripted_camera_reply() {
        use crate::wire::scripted::{self, answer};

        let (camera, transport) =
            scripted::camera(vec![answer(&[0x00, 0x01, 0x00, 0x02, 0x03, 0x04, 0x02])]).await;

        let version = query_version(&camera)
            .await
            .expect("scripted camera should reply");

        assert_eq!(version, sample_version());
        assert_eq!(transport.sent(), [[0x81, 0x09, 0x00, 0x02, 0xFF]]);
    }
}
