//! Live camera state for the info panel: what the camera reports about
//! itself beyond the one-time version reply.

use grafton_visca::{
    BlockingClient, Error,
    camera::{PanTiltPosition, profiles::GenericVisca},
    command::FocusMode,
    transport::{BlockingTransport, HasTransportConfig},
    types::{FocusPosition, ZoomPosition},
};

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
pub fn query_state<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<CameraState, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    let power_on = camera.power_state()?;
    Ok(CameraState {
        power_on,
        // Nothing to ask a sleeping camera: it has parked its lens, and every
        // inquiry is a round trip spent being refused.
        lens: power_on.then(|| query_lens(camera).ok()).flatten(),
    })
}

fn query_lens<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<Lens, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    Ok(Lens {
        pan_tilt: camera.pan_tilt_position()?,
        zoom: camera.zoom_position()?,
        focus: camera.focus_position()?,
        auto_focus: camera.focus_mode()? == FocusMode::Auto,
    })
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
    use grafton_visca::testing::testkit::{
        ScriptedBlockingTransport, Step,
        helpers::{self, not_executable},
    };

    fn scripted_camera(
        replies: Vec<Step>,
    ) -> BlockingClient<GenericVisca, ScriptedBlockingTransport> {
        grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(ScriptedBlockingTransport::new(replies))
            .expect("camera should build from a scripted transport")
    }

    /// The reply a camera gives to something it won't do in its current state.
    fn refusal() -> Step {
        Step::OnSend {
            matches: None,
            responses: vec![not_executable(1)],
        }
    }

    #[test]
    fn a_sleeping_camera_reports_its_power_and_is_not_asked_anything_else() {
        // One scripted reply, so a lens inquiry would find nothing to answer
        // it: the camera is in standby and there is nothing there to ask.
        let camera = scripted_camera(vec![helpers::power_inquiry_response(false)]);

        let state = query_state(&camera).expect("standby is an answer, not a failure");

        assert!(!state.power_on);
        assert!(state.lens.is_none());
    }

    #[test]
    fn a_camera_that_will_not_answer_for_its_lens_yet_still_reports_its_power() {
        // What waking up looks like: power answers, the lens refuses. The
        // power reading is the one that decides what the switch offers, so it
        // has to survive the lens failing beside it.
        let camera = scripted_camera(vec![helpers::power_inquiry_response(true), refusal()]);

        let state = query_state(&camera).expect("a refused lens inquiry is not a failed snapshot");

        assert!(state.power_on);
        assert!(state.lens.is_none());
    }

    #[test]
    fn a_camera_that_answers_nothing_at_all_is_a_failed_snapshot() {
        // The distinction the front ends need: a lens that won't answer is
        // reported, but a camera that won't answer for its own power is not
        // something to make up a state for.
        let camera = scripted_camera(vec![refusal()]);

        assert!(query_state(&camera).is_err());
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
