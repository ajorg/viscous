//! Live camera state for the info panel: what the camera reports about
//! itself beyond the one-time version reply.

use grafton_visca::{
    Error,
    camera::PanTiltPosition,
    command::{
        FocusMode, FocusModeInquiry, FocusPositionInquiry, InquiryData, PanTiltPositionInquiry,
        PowerInquiry, ZoomPositionInquiry,
    },
    types::{FocusPosition, ZoomPosition},
};

use crate::wire::{self, Wire};

/// A snapshot of what the camera says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraState {
    /// Whether the camera is awake.
    pub power_on: bool,
    /// Where the camera is pointing, zoomed and focused — or `None` when it
    /// has nothing to say about that, which is what standby and the seconds
    /// after being woken both look like.
    pub lens: Option<Lens>,
}

/// Where the camera is pointing, zoomed and focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lens {
    /// Current pan/tilt position, in raw camera units.
    pub pan_tilt: PanTiltPosition,
    /// Current zoom position.
    pub zoom: ZoomPosition,
    /// Current focus position.
    pub focus: FocusPosition,
    /// Whether the camera is focusing automatically, in which case a manual
    /// focus drive won't hold.
    pub auto_focus: bool,
}

/// Queries a connected camera for its current state.
///
/// Power comes first and stands on its own: **what the camera says about its
/// power is never thrown away because some other inquiry failed.** An operator
/// who has just put the camera into standby needs the switch to say so, and it
/// can't if one all-or-nothing snapshot fails on the lens — the switch would
/// go on offering to do the thing that has already been done.
///
/// So the lens is only asked about once the camera says it is awake, and a
/// lens that won't answer is reported as a lens that won't answer. Cameras
/// differ in how they refuse: some return "not executable", some say nothing
/// and time out, and a camera part-way through waking up does whichever it
/// feels like. None of those are worth telling the operator apart, and keying
/// on any of them would be keying on the model rather than on the protocol.
pub async fn query_state(camera: &impl Wire) -> Result<CameraState, Error> {
    let power_on = match wire::ask(camera, PowerInquiry).await? {
        InquiryData::Power { on } => on,
        other => return Err(unexpected("power", other)),
    };
    // Nothing to ask a sleeping camera: it has parked its lens, and every
    // inquiry is a round trip spent being refused.
    let lens = if power_on {
        query_lens(camera).await.ok()
    } else {
        None
    };
    Ok(CameraState { power_on, lens })
}

async fn query_lens(camera: &impl Wire) -> Result<Lens, Error> {
    let pan_tilt = match wire::ask(camera, PanTiltPositionInquiry).await? {
        InquiryData::PanTiltPosition { pan, tilt } => PanTiltPosition { pan, tilt },
        other => return Err(unexpected("pan/tilt position", other)),
    };
    let zoom = match wire::ask(camera, ZoomPositionInquiry).await? {
        InquiryData::ZoomPosition { position } => ZoomPosition::try_from(position)
            .map_err(|error| Error::ParseError(error.to_string().into()))?,
        other => return Err(unexpected("zoom position", other)),
    };
    let focus = match wire::ask(camera, FocusPositionInquiry).await? {
        InquiryData::FocusPosition { position } => FocusPosition::new(position),
        other => return Err(unexpected("focus position", other)),
    };
    let auto_focus = match wire::ask(camera, FocusModeInquiry).await? {
        InquiryData::FocusMode { mode } => mode == FocusMode::Auto,
        other => return Err(unexpected("focus mode", other)),
    };
    Ok(Lens {
        pan_tilt,
        zoom,
        focus,
        auto_focus,
    })
}

/// The error for an answer that isn't to the question asked.
fn unexpected(asked: &str, answer: InquiryData) -> Error {
    Error::ParseError(format!("asked for {asked}, got {answer:?}").into())
}

/// Formats where the camera is pointing, zoomed and focused.
///
/// Split out from [`format_state`] for front ends that show the camera's power
/// and focus mode in controls of their own: repeating them in a readout
/// underneath only invites the two to disagree.
pub fn format_position(lens: &Lens) -> String {
    // `ZoomPosition`/`FocusPosition`'s own `Display` impls already read
    // "Zoom 0x1000"/"Focus 0x1000" (grafton-visca bakes the label in), so
    // pairing that with our own "zoom="/"focus=" label would double it up;
    // use the raw value instead.
    format!(
        "pan={} tilt={} zoom=0x{:04X} focus=0x{:04X}",
        lens.pan_tilt.pan,
        lens.pan_tilt.tilt,
        lens.zoom.value(),
        lens.focus.value(),
    )
}

/// Formats a camera state snapshot in full, for a display with nowhere else to
/// put the power and focus mode.
pub fn format_state(state: &CameraState) -> String {
    let power = if state.power_on { "on" } else { "off" };
    match &state.lens {
        Some(lens) => format!(
            "power={power} {} ({})",
            format_position(lens),
            if lens.auto_focus { "auto" } else { "manual" },
        ),
        // Nothing to say about the lens is itself worth saying: the numbers
        // that were there a moment ago are not the camera's current answer.
        None => format!("power={power} (lens not reporting)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::scripted::{self, answer, power, refused};

    #[tokio::test]
    async fn an_awake_camera_reports_where_its_lens_is() {
        let (camera, transport) = scripted::camera(vec![
            power(true),
            // pan -2, tilt 0x12
            answer(&[0x0F, 0x0F, 0x0F, 0x0E, 0x00, 0x00, 0x01, 0x02]),
            answer(&[0x01, 0x00, 0x00, 0x00]),
            answer(&[0x02, 0x00, 0x00, 0x00]),
            answer(&[0x02]),
        ])
        .await;

        let state = query_state(&camera).await.expect("every question answered");

        let lens = state.lens.expect("an awake camera has a lens to report");
        assert_eq!((lens.pan_tilt.pan, lens.pan_tilt.tilt), (-2, 0x12));
        assert_eq!(lens.zoom.value(), 0x1000);
        assert_eq!(lens.focus.value(), 0x2000);
        assert!(lens.auto_focus);
        assert_eq!(transport.sent().len(), 5);
    }

    #[tokio::test]
    async fn a_sleeping_camera_reports_its_power_and_is_not_asked_anything_else() {
        let (camera, transport) = scripted::camera(vec![power(false)]).await;

        let state = query_state(&camera)
            .await
            .expect("standby is an answer, not a failure");

        assert!(!state.power_on);
        assert!(state.lens.is_none());
        assert_eq!(transport.sent().len(), 1, "only power was asked about");
    }

    #[tokio::test]
    async fn a_camera_that_will_not_answer_for_its_lens_yet_still_reports_its_power() {
        // What waking up looks like: power answers, the lens refuses. The
        // power reading is the one that decides what the switch offers, so it
        // has to survive the lens failing beside it.
        let (camera, _) = scripted::camera(vec![power(true), refused()]).await;

        let state = query_state(&camera)
            .await
            .expect("a refused lens inquiry is not a failed snapshot");

        assert!(state.power_on);
        assert!(state.lens.is_none());
    }

    #[tokio::test]
    async fn a_camera_that_answers_nothing_at_all_is_a_failed_snapshot() {
        // The distinction the front ends need: a lens that won't answer is
        // reported, but a camera that won't answer for its own power is not
        // something to make up a state for.
        let (camera, _) = scripted::camera(vec![refused()]).await;

        assert!(query_state(&camera).await.is_err());
    }

    fn sample_lens() -> Lens {
        Lens {
            pan_tilt: PanTiltPosition::new(-120, 45),
            zoom: ZoomPosition::try_from(0x1000u16).unwrap(),
            focus: FocusPosition::new(0x2000),
            auto_focus: false,
        }
    }

    fn sample_state() -> CameraState {
        CameraState {
            power_on: true,
            lens: Some(sample_lens()),
        }
    }

    #[test]
    fn format_state_renders_all_fields() {
        let text = format_state(&sample_state());
        assert!(text.contains("power=on"));
        assert!(text.contains("pan=-120"));
        assert!(text.contains("tilt=45"));
        assert!(text.contains("zoom=0x1000"));
        assert!(text.contains("focus=0x2000"));
        assert!(text.contains("manual"));
    }

    #[test]
    fn format_state_says_which_way_focus_is_being_driven() {
        let auto = CameraState {
            lens: Some(Lens {
                auto_focus: true,
                ..sample_lens()
            }),
            ..sample_state()
        };

        assert!(format_state(&auto).contains("auto"));
    }

    #[test]
    fn format_state_reports_a_sleeping_camera_without_inventing_a_position() {
        let asleep = CameraState {
            power_on: false,
            lens: None,
        };
        let text = format_state(&asleep);

        assert!(text.contains("power=off"));
        assert!(!text.contains("pan="));
    }

    #[test]
    fn format_position_leaves_out_what_a_control_already_shows() {
        let text = format_position(&sample_lens());

        assert!(text.contains("pan=-120"));
        assert!(text.contains("zoom=0x1000"));
        assert!(!text.contains("power"));
        assert!(!text.contains("manual"));
    }

    #[test]
    fn format_state_does_not_double_up_the_zoom_and_focus_labels() {
        // ZoomPosition/FocusPosition's own Display impls already read
        // "Zoom 0x1000"/"Focus 0x1000"; make sure format_state doesn't pair
        // that with its own "zoom="/"focus=" label.
        let text = format_state(&sample_state());
        assert!(!text.contains("Zoom"));
        assert!(!text.contains("Focus"));
    }
}
