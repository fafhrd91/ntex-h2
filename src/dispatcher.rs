use std::{cell, fmt, future::Future, marker, pin::Pin, rc::Rc, task::Context, task::Poll};

use ntex_io::DispatchItem;
use ntex_service::Service;
use ntex_util::{future::Either, future::Ready, ready};

use crate::control::{ControlMessage, ControlResult};
use crate::error::{ProtocolError, StreamError};
use crate::frame::{Frame, GoAway, Reason, StreamId};
use crate::{codec::Codec, connection::Connection, message::Message, stream::StreamRef};

/// Amqp server dispatcher service.
pub(crate) struct Dispatcher<Ctl, Pub>
where
    Ctl: Service<ControlMessage<Pub::Error>>,
    Pub: Service<Message>,
{
    inner: Rc<Inner<Ctl>>,
    publish: Pub,
    connection: Connection,
    shutdown: cell::RefCell<Shutdown<Ctl::Future>>,
}

enum Shutdown<F> {
    NotSet,
    Done,
    InProcess(Pin<Box<F>>),
}

struct Inner<Ctl> {
    control: Ctl,
    last_stream_id: StreamId,
}

type ServiceFut<Pub, Ctl, E> =
    Either<PublishResponse<Pub, Ctl>, Either<Ready<Option<Frame>, ()>, ControlResponse<E, Ctl>>>;

impl<Ctl, Pub> Dispatcher<Ctl, Pub>
where
    Ctl: Service<ControlMessage<Pub::Error>, Response = ControlResult>,
    Ctl::Error: fmt::Debug,
    Pub: Service<Message, Response = ()>,
    Pub::Error: fmt::Debug,
{
    pub(crate) fn new(connection: Connection, control: Ctl, publish: Pub) -> Self {
        Dispatcher {
            publish,
            connection,
            inner: Rc::new(Inner {
                control,
                last_stream_id: 0.into(),
            }),
            shutdown: cell::RefCell::new(Shutdown::NotSet),
        }
    }

    fn handle_message(
        &self,
        result: Result<Option<(StreamRef, Message)>, ProtocolError>,
    ) -> ServiceFut<Pub, Ctl, Pub::Error> {
        match result {
            Ok(Some((stream, msg))) => Either::Left(PublishResponse::new(
                self.publish.call(msg),
                stream,
                &self.inner,
            )),
            Ok(None) => Either::Right(Either::Left(Ready::Ok(None))),
            Err(err) => Either::Right(Either::Right(ControlResponse::new(
                ControlMessage::proto_error(err),
                &self.inner,
            ))),
        }
    }

    fn handle_proto_error(
        &self,
        result: Result<(), ProtocolError>,
    ) -> ServiceFut<Pub, Ctl, Pub::Error> {
        match result {
            Ok(()) => Either::Right(Either::Left(Ready::Ok(None))),
            Err(err) => Either::Right(Either::Right(ControlResponse::new(
                ControlMessage::proto_error(err),
                &self.inner,
            ))),
        }
    }

    fn handle_mixed_error(
        &self,
        result: Result<(), Either<ProtocolError, StreamError>>,
    ) -> ServiceFut<Pub, Ctl, Pub::Error> {
        match result {
            Ok(()) => Either::Right(Either::Left(Ready::Ok(None))),
            Err(Either::Left(err)) => Either::Right(Either::Right(ControlResponse::new(
                ControlMessage::proto_error(err),
                &self.inner,
            ))),
            Err(Either::Right(err)) => Either::Right(Either::Right(ControlResponse::new(
                ControlMessage::stream_error(err),
                &self.inner,
            ))),
        }
    }
}

impl<Ctl, Pub> Service<DispatchItem<Rc<Codec>>> for Dispatcher<Ctl, Pub>
where
    Ctl: Service<ControlMessage<Pub::Error>, Response = ControlResult> + 'static,
    Ctl::Error: fmt::Debug,
    Pub: Service<Message, Response = ()>,
    Pub::Error: fmt::Debug,
{
    type Response = Option<Frame>;
    type Error = ();
    type Future = Either<
        PublishResponse<Pub, Ctl>,
        Either<Ready<Self::Response, Self::Error>, ControlResponse<Pub::Error, Ctl>>,
    >;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // check publish service readiness
        let res1 = self.publish.poll_ready(cx);

        // check control service readiness
        let res2 = self.inner.control.poll_ready(cx);

        if res1.is_pending() || res2.is_pending() {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn poll_shutdown(&self, cx: &mut Context<'_>, is_error: bool) -> Poll<()> {
        let mut shutdown = self.shutdown.borrow_mut();
        if matches!(&*shutdown, &Shutdown::NotSet) {
            // self.inner.sink.drop_sink();
            *shutdown = Shutdown::InProcess(Box::pin(
                self.inner
                    .control
                    .call(ControlMessage::terminated(is_error)),
            ));
        }

        let shutdown_ready = match &mut *shutdown {
            Shutdown::NotSet => panic!("guard above"),
            Shutdown::Done => true,
            Shutdown::InProcess(ref mut fut) => {
                let res = fut.as_mut().poll(cx);
                if res.is_ready() {
                    *shutdown = Shutdown::Done;
                    true
                } else {
                    false
                }
            }
        };

        if shutdown_ready {
            let res1 = self.publish.poll_shutdown(cx, is_error);
            let res2 = self.inner.control.poll_shutdown(cx, is_error);
            if res1.is_pending() || res2.is_pending() {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        } else {
            Poll::Pending
        }
    }

    fn call(&self, request: DispatchItem<Rc<Codec>>) -> Self::Future {
        match request {
            DispatchItem::Item(frame) => match frame {
                Frame::Headers(hdrs) => self.handle_message(self.connection.recv_headers(hdrs)),
                Frame::Data(data) => self.handle_message(self.connection.recv_data(data)),
                Frame::Settings(settings) => {
                    self.handle_proto_error(self.connection.recv_settings(settings))
                }
                Frame::WindowUpdate(update) => {
                    self.handle_mixed_error(self.connection.recv_window_update(update))
                }
                Frame::Reset(reset) => {
                    log::trace!("processing RESET: {:#?}", reset);
                    Either::Right(Either::Left(Ready::Ok(None)))
                }
                Frame::Ping(ping) => {
                    log::trace!("processing PING: {:#?}", ping);
                    Either::Right(Either::Left(Ready::Ok(None)))
                }
                Frame::GoAway(frm) => {
                    log::trace!("processing GoAway: {:#?}", frm);
                    Either::Right(Either::Right(ControlResponse::new(
                        ControlMessage::go_away(frm),
                        &self.inner,
                    )))
                }
                Frame::Priority(prio) => {
                    log::debug!("PRIORITY frame is not supported: {:#?}", prio);
                    Either::Right(Either::Left(Ready::Ok(None)))
                }
            },
            DispatchItem::EncoderError(_err) => {
                // let frame = ControlMessage::new_kind(ControlFrameKind::ProtocolError(err.into()));
                // *self.ctl_fut.borrow_mut() =
                // Some((Some(frame.clone()), Box::pin(self.inner.control.call(frame))));
                Either::Right(Either::Left(Ready::Ok(None)))
            }
            DispatchItem::DecoderError(_err) => {
                // let frame = ControlMessage::new_kind(ControlFrameKind::ProtocolError(err.into()));
                // *self.ctl_fut.borrow_mut() =
                // Some((Some(frame.clone()), Box::pin(self.inner.control.call(frame))));
                Either::Right(Either::Left(Ready::Ok(None)))
            }
            DispatchItem::KeepAliveTimeout => {
                // let frame = ControlMessage::new_kind(ControlFrameKind::ProtocolError(
                //     AmqpProtocolError::KeepAliveTimeout,
                // ));
                //*self.ctl_fut.borrow_mut() =
                //    Some((Some(frame.clone()), Box::pin(self.inner.control.call(frame))));
                Either::Right(Either::Left(Ready::Ok(None)))
            }
            DispatchItem::Disconnect(err) => Either::Right(Either::Right(ControlResponse::new(
                ControlMessage::peer_gone(err),
                &self.inner,
            ))),
            DispatchItem::WBackPressureEnabled | DispatchItem::WBackPressureDisabled => {
                Either::Right(Either::Left(Ready::Ok(None)))
            }
        }
    }
}

pin_project_lite::pin_project! {
    /// Publish service response future
    pub(crate) struct PublishResponse<P: Service<Message>, C: Service<ControlMessage<P::Error>>> {
        stream: StreamRef,
        #[pin]
        state: PublishResponseState<P, C>,
        inner: Rc<Inner<C>>,
    }
}

pin_project_lite::pin_project! {
    #[project = PublishResponseStateProject]
    enum PublishResponseState<P: Service<Message>, C: Service<ControlMessage<P::Error>>> {
        Publish { #[pin] fut: P::Future },
        Control { #[pin] fut: ControlResponse<P::Error, C> },
    }
}

impl<P, C> PublishResponse<P, C>
where
    P: Service<Message, Response = ()>,
    P::Error: fmt::Debug,
    C: Service<ControlMessage<P::Error>, Response = ControlResult>,
    C::Error: fmt::Debug,
{
    fn new(fut: P::Future, stream: StreamRef, inner: &Rc<Inner<C>>) -> Self {
        Self {
            stream,
            inner: inner.clone(),
            state: PublishResponseState::Publish { fut },
        }
    }
}

impl<P, C> Future for PublishResponse<P, C>
where
    P: Service<Message, Response = ()>,
    P::Error: fmt::Debug,
    C: Service<ControlMessage<P::Error>, Response = ControlResult>,
    C::Error: fmt::Debug,
{
    type Output = Result<Option<Frame>, ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.as_mut().project();

        match this.state.as_mut().project() {
            PublishResponseStateProject::Publish { fut } => match fut.poll(cx) {
                Poll::Ready(Ok(_)) => Poll::Ready(Ok(None)),
                Poll::Ready(Err(e)) => {
                    this.state.set(PublishResponseState::Control {
                        fut: ControlResponse::new(
                            ControlMessage::app_error(e, this.stream.clone()),
                            this.inner,
                        ),
                    });
                    self.poll(cx)
                }
                Poll::Pending => Poll::Pending,
            },
            PublishResponseStateProject::Control { fut } => fut.poll(cx),
        }
    }
}

pin_project_lite::pin_project! {
    /// Control service response future
    pub(crate) struct ControlResponse<E, Ctl: Service<ControlMessage<E>>> {
        #[pin]
        fut: Ctl::Future,
        inner: Rc<Inner<Ctl>>,
        _t: marker::PhantomData<E>,
    }
}

impl<E, Ctl> ControlResponse<E, Ctl>
where
    E: fmt::Debug,
    Ctl: Service<ControlMessage<E>, Response = ControlResult>,
{
    fn new(pkt: ControlMessage<E>, inner: &Rc<Inner<Ctl>>) -> Self {
        Self {
            fut: inner.control.call(pkt),
            inner: inner.clone(),
            _t: marker::PhantomData,
        }
    }
}

impl<E, Ctl> Future for ControlResponse<E, Ctl>
where
    E: fmt::Debug,
    Ctl: Service<ControlMessage<E>, Response = ControlResult>,
{
    type Output = Result<Option<Frame>, ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().project();

        match ready!(this.fut.poll(cx)) {
            Ok(item) => Poll::Ready(Ok(item.frame)),
            Err(err) => {
                // we cannot handle control service errors, close connection
                Poll::Ready(Ok(Some(
                    GoAway::new(Reason::INTERNAL_ERROR)
                        .set_last_stream_id(this.inner.last_stream_id)
                        .into(),
                )))
            }
        }
    }
}
