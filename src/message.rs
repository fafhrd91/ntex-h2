use std::mem;

use ntex_bytes::Bytes;
use ntex_http::HeaderMap;
use ntex_util::future::Either;

use crate::connection::Stream;
use crate::frame::{PseudoHeaders, Reason};

#[derive(Debug)]
pub struct Message {
    stream: Stream,
    kind: MessageKind,
}

#[derive(Debug)]
pub enum MessageKind {
    Headers {
        pseudo: PseudoHeaders,
        headers: HeaderMap,
        eof: bool,
    },
    Data(Bytes),
    Eof(StreamEof),
    Empty,
}

#[derive(Debug, Clone)]
pub enum StreamEof {
    Data(Bytes),
    Trailers(HeaderMap),
    Reset(Reason),
}

impl Message {
    pub(crate) fn new(
        pseudo: PseudoHeaders,
        headers: HeaderMap,
        eof: bool,
        stream: &Stream,
    ) -> Self {
        Message {
            stream: stream.clone(),
            kind: MessageKind::Headers {
                pseudo,
                headers,
                eof,
            },
        }
    }

    pub(crate) fn data(data: Bytes, eof: bool, stream: &Stream) -> Self {
        if eof {
            Message {
                stream: stream.clone(),
                kind: MessageKind::Eof(StreamEof::Data(data)),
            }
        } else {
            Message {
                stream: stream.clone(),
                kind: MessageKind::Data(data),
            }
        }
    }

    pub fn kind(&mut self) -> &mut MessageKind {
        &mut self.kind
    }

    pub fn stream(&self) -> &Stream {
        &self.stream
    }
}

impl MessageKind {
    pub fn take(&mut self) -> MessageKind {
        mem::replace(self, MessageKind::Empty)
    }
}
