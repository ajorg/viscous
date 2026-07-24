//! Establishing a camera connection and identifying what's on the other end.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::VersionInfo,
    transport::{BlockingTransport, HasTransportConfig},
};

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
