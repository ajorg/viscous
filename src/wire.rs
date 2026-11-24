//! The one place that talks to the camera, and how long it waits when it does.
//!
//! Everything else in the program builds VISCA commands as plain values and
//! hands them to the worker; only the worker sends them, through [`Wire`].
//! That keeps the camera behind a single owner, and keeps everything that
//! decides *what* to send testable without a camera at all.

use std::time::Duration;

use grafton_visca::{
    Error, Executor,
    camera::{Camera, profiles::GenericVisca},
    command::{InquiryData, Response, ViscaCommand},
    mode::{Async, BoxFuture},
    transport::{AsyncTransport, HasTransportConfig},
};

/// A camera that commands and questions can be sent to.
///
/// One method, because a command and a question differ only in what comes
/// back: [`execute`] and [`ask`] are built on it.
pub trait Wire {
    /// Sends `message` and resolves to the camera's reply, or to the library
    /// giving up on one.
    fn send<C: ViscaCommand>(&self, message: &C) -> BoxFuture<'static, Result<Response, Error>>;
}

impl<T, E> Wire for Camera<Async, GenericVisca, T, E>
where
    T: AsyncTransport + HasTransportConfig + Send + Sync + 'static,
    E: Executor + Send + Sync + Clone,
{
    fn send<C: ViscaCommand>(&self, message: &C) -> BoxFuture<'static, Result<Response, Error>> {
        self.send_command(message)
    }
}

/// Sends `command` and waits for the camera to say it has done it.
pub async fn execute<C: ViscaCommand>(camera: &impl Wire, command: C) -> Result<(), Error> {
    camera.send(&command).await?.into_result()
}

/// Asks the camera `inquiry` and returns what it said, giving up after
/// [`QUESTION`].
pub async fn ask<C: ViscaCommand>(camera: &impl Wire, inquiry: C) -> Result<InquiryData, Error> {
    let answer = tokio::time::timeout(QUESTION, camera.send(&inquiry))
        .await
        .map_err(|_| Error::Timeout)?;
    match answer? {
        Response::Inquiry(data) => Ok(data),
        other => Err(Error::ParseError(
            format!("expected an answer to an inquiry, got {other:?}").into(),
        )),
    }
}

/// How long the controls under a hand wait for the camera to take a command,
/// before giving up on it and getting on with the next.
///
/// A drive is answered as soon as the camera takes it, which over 9600 baud
/// is a few tens of milliseconds, and a step's answer comes when the head has
/// travelled a hair's breadth, a few more. Anything still unanswered after
/// this has been lost on the way, and the operator is the one kept waiting by
/// it: everything after it is queued behind it.
pub const AT_HAND: Duration = Duration::from_millis(500);

/// How long a one-off command — a preset recall, home, reset, power — may
/// take. These answer when the camera has finished, which for a preset across
/// the room or a camera waking from standby is many seconds, and the operator
/// has asked to wait for exactly that.
pub const ONE_OFF: Duration = Duration::from_secs(60);

/// How long a question about the camera's state may go unanswered.
///
/// Nothing waits on these but the readout, so this needn't be short. It only
/// has to end, so that the next question can be asked.
pub const QUESTION: Duration = Duration::from_secs(5);

/// The bytes `command` goes out as, addressed to the first camera: what a
/// command-building test checks, with no camera needed to check it.
#[cfg(test)]
pub(crate) fn encoded(command: &impl ViscaCommand) -> Vec<u8> {
    let mut buffer = vec![0; 64];
    let length = command
        .write_into(grafton_visca::CameraId::CAMERA_1, &mut buffer)
        .expect("a command fits in 64 bytes");
    buffer.truncate(length);
    buffer
}

/// A camera that answers from a script, for the tests that need one to talk
/// to: the worker's and the state query's.
#[cfg(test)]
pub(crate) mod scripted {
    use std::sync::Arc;

    use grafton_visca::{
        CameraBuilder, TokioExecutor,
        camera::{Camera, profiles::GenericVisca},
        mode::Async,
        testing::testkit::{ScriptedTransport, Step},
    };

    pub(crate) type Transport = ScriptedTransport<TokioExecutor>;
    pub(crate) type ScriptedCamera = Camera<Async, GenericVisca, Transport, TokioExecutor>;

    /// A camera that replies to what it is sent as `steps` say, and the
    /// transport, for asking what it was sent. Needs a Tokio runtime.
    pub(crate) async fn camera(steps: Vec<Step>) -> (ScriptedCamera, Transport) {
        let executor = Arc::new(TokioExecutor::from_current().expect("tests run in Tokio"));
        let transport = Transport::new(steps).with_executor(executor.clone());
        let camera = CameraBuilder::<TokioExecutor>::with_executor(executor)
            .retry_config(crate::connection::NEVER_RESEND)
            .open_async::<GenericVisca, _>(transport.clone())
            .await
            .expect("a scripted camera opens");
        (camera, transport)
    }

    /// The camera taking a command in socket 1 and finishing it.
    pub(crate) fn done() -> Step {
        done_in(1)
    }

    /// The camera taking a command in `socket` and finishing it.
    pub(crate) fn done_in(socket: u8) -> Step {
        Step::OnSend {
            matches: None,
            responses: vec![
                vec![0x90, 0x40 | socket, 0xFF],
                vec![0x90, 0x50 | socket, 0xFF],
            ],
        }
    }

    /// The camera taking a command and never saying it has finished.
    pub(crate) fn taken() -> Step {
        Step::OnSend {
            matches: None,
            responses: vec![vec![0x90, 0x41, 0xFF]],
        }
    }

    /// The camera hearing something and saying nothing at all.
    pub(crate) fn silence() -> Step {
        Step::OnSend {
            matches: None,
            responses: Vec::new(),
        }
    }

    /// The camera refusing what it is in no state to do.
    pub(crate) fn refused() -> Step {
        Step::OnSend {
            matches: None,
            responses: vec![vec![0x90, 0x61, 0x41, 0xFF]],
        }
    }

    /// The camera answering a question with `payload`.
    pub(crate) fn answer(payload: &[u8]) -> Step {
        let mut reply = vec![0x90, 0x50];
        reply.extend_from_slice(payload);
        reply.push(0xFF);
        Step::OnSend {
            matches: None,
            responses: vec![reply],
        }
    }

    /// The camera saying whether it is awake.
    pub(crate) fn power(on: bool) -> Step {
        answer(&[if on { 0x02 } else { 0x03 }])
    }
}
