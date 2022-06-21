pub use crate::codec::EncoderError;

use crate::frame::{self, GoAway, Reason, StreamId};
use crate::stream::StreamRef;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProtocolError {
    #[error("Unknown stream {0:?}")]
    UnknownStream(frame::Frame),
    #[error("Reason: {0}")]
    Reason(Reason),
    #[error("{0}")]
    Encoder(#[from] EncoderError),
    #[error("Stream idle: {0}")]
    StreamIdle(&'static str),
    #[error("{0:?} is closed")]
    StreamClosed(StreamId),
    /// An invalid stream identifier was provided
    #[error("An invalid stream identifier was provided")]
    InvalidStreamId,
    #[error("Unexpected setting ack received")]
    UnexpectedSettingsAck,
    /// Missing pseudo header
    #[error("Missing pseudo header {0:?}")]
    MissingPseudo(&'static str),
    /// Missing pseudo header
    #[error("Unexpected pseudo header {0:?}")]
    UnexpectedPseudo(&'static str),
    /// Window update value is zero
    #[error("Window update value is zero")]
    ZeroWindowUpdateValue,
    /// Keep-alive timeout
    #[error("Keep-alive timeout")]
    KeepaliveTimeout,
    #[error("{0}")]
    Frame(#[from] frame::FrameError),
}

impl From<Reason> for ProtocolError {
    fn from(r: Reason) -> Self {
        ProtocolError::Reason(r)
    }
}

impl ProtocolError {
    pub fn to_goaway(&self) -> GoAway {
        match self {
            ProtocolError::Reason(reason) => GoAway::new(*reason),
            ProtocolError::Encoder(_) => {
                GoAway::new(Reason::PROTOCOL_ERROR).set_data("error during frame encoding")
            }
            ProtocolError::MissingPseudo(s) => GoAway::new(Reason::PROTOCOL_ERROR)
                .set_data(format!("Missing pseudo header {:?}", s)),
            ProtocolError::UnexpectedPseudo(s) => GoAway::new(Reason::PROTOCOL_ERROR)
                .set_data(format!("Unexpected pseudo header {:?}", s)),
            ProtocolError::UnknownStream(_) => {
                GoAway::new(Reason::PROTOCOL_ERROR).set_data("unknown stream")
            }
            ProtocolError::InvalidStreamId => GoAway::new(Reason::PROTOCOL_ERROR)
                .set_data("An invalid stream identifier was provided"),
            ProtocolError::StreamIdle(s) => {
                GoAway::new(Reason::PROTOCOL_ERROR).set_data(format!("Stream idle: {}", s))
            }
            ProtocolError::StreamClosed(s) => {
                GoAway::new(Reason::STREAM_CLOSED).set_data(format!("{:?} is closed", s))
            }
            ProtocolError::UnexpectedSettingsAck => {
                GoAway::new(Reason::PROTOCOL_ERROR).set_data("received unexpected settings ack")
            }
            ProtocolError::ZeroWindowUpdateValue => GoAway::new(Reason::PROTOCOL_ERROR)
                .set_data("zero value for window update frame is not allowed"),
            ProtocolError::KeepaliveTimeout => {
                GoAway::new(Reason::NO_ERROR).set_data("keep-alive timeout")
            }
            ProtocolError::Frame(err) => {
                GoAway::new(Reason::PROTOCOL_ERROR).set_data(format!("protocol error: {:?}", err))
            }
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("Stream error: {kind:?}")]
pub(crate) struct StreamErrorInner {
    kind: StreamError,
    stream: StreamRef,
}

impl StreamErrorInner {
    pub(crate) fn new(stream: StreamRef, kind: StreamError) -> Self {
        Self { kind, stream }
    }

    pub(crate) fn into_inner(self) -> (StreamRef, StreamError) {
        (self.stream, self.kind)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    #[error("Stream in idle state: {0}")]
    Idle(&'static str),
    #[error("Stream is closed")]
    Closed,
    #[error("Window value is overflowed")]
    WindowOverflowed,
    #[error("Zero value for window")]
    WindowZeroUpdateValue,
    #[error("Trailers headers without end of stream flags")]
    TrailersWithoutEos,
    #[error("Invalid content length")]
    InvalidContentLength,
    #[error("Payload length does not match content-length header")]
    WrongPayloadLength,
    #[error("Non-empty payload for HEAD response")]
    NonEmptyPayload,
}

impl StreamError {
    #[inline]
    pub(crate) fn reason(&self) -> Reason {
        match self {
            StreamError::Idle(_) => Reason::PROTOCOL_ERROR,
            StreamError::Closed => Reason::STREAM_CLOSED,
            StreamError::WindowOverflowed => Reason::FLOW_CONTROL_ERROR,
            StreamError::WindowZeroUpdateValue => Reason::PROTOCOL_ERROR,
            StreamError::TrailersWithoutEos => Reason::PROTOCOL_ERROR,
            StreamError::InvalidContentLength => Reason::PROTOCOL_ERROR,
            StreamError::WrongPayloadLength => Reason::PROTOCOL_ERROR,
            StreamError::NonEmptyPayload => Reason::PROTOCOL_ERROR,
        }
    }
}

/// Operation errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum OperationError {
    #[error("{0:?}")]
    Stream(#[from] StreamError),
    #[error("{0}")]
    Protocol(#[from] ProtocolError),

    /// Cannot process operation for idle stream
    #[error("Cannot process operation for idle stream")]
    Idle,

    /// Cannot process operation for stream in payload state
    #[error("Cannot process operation for stream in payload state")]
    Payload,

    /// Stream is closed
    #[error("Stream is closed {0:?}")]
    Closed(Option<Reason>),

    /// Stream has been reset from the peer
    #[error("Stream has been reset from the peer with {0}")]
    RemoteReset(Reason),

    /// The stream ID space is overflowed
    ///
    /// A new connection is needed.
    #[error("The stream ID space is overflowed")]
    OverflowedStreamId,

    /// Disconnected
    #[error("Connection is closed")]
    Disconnected,
}
