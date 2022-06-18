use std::{cell::Cell, cell::RefCell, future::Future, marker::PhantomData, rc::Rc};

use ntex::connect::{self, Address, Connect, Connector as DefaultConnector};
use ntex_bytes::{PoolId, PoolRef};
use ntex_io::IoBoxed;
use ntex_service::{IntoService, Service};
use ntex_util::time::{timeout_checked, Seconds};

use crate::codec::Codec;
use crate::connection::{Config, Connection};
use crate::frame::Settings;
use crate::{consts, frame};

use super::{ClientConnection, ClientError};

/// Mqtt client connector
pub struct Connector<A, T>(Rc<RefCell<Inner<A, T>>>);

struct Inner<A, T> {
    connector: T,

    /// Time to keep locally reset streams around before reaping.
    pub(super) reset_stream_duration: Seconds,

    /// Maximum number of locally reset streams to keep at a time.
    pub(super) reset_stream_max: usize,

    /// Initial `Settings` frame to send as part of the handshake.
    pub(super) settings: Settings,

    /// Initial target window size for new connections.
    pub(super) initial_target_connection_window_size: Option<u32>,

    pub(super) handshake_timeout: Seconds,
    pub(super) disconnect_timeout: Seconds,
    pub(super) keepalive_timeout: Seconds,
    pub(super) pool: Cell<PoolRef>,

    _t: PhantomData<A>,
}

impl<A> Connector<A, ()>
where
    A: Address,
{
    #[allow(clippy::new_ret_no_self)]
    /// Create new h2 connector
    pub fn new() -> Connector<A, DefaultConnector<A>> {
        Connector(Rc::new(RefCell::new(Inner {
            connector: DefaultConnector::default(),
            settings: Settings::default(),
            reset_stream_duration: consts::DEFAULT_RESET_STREAM_SECS,
            reset_stream_max: consts::DEFAULT_RESET_STREAM_MAX,
            initial_target_connection_window_size: None,
            handshake_timeout: Seconds(5),
            disconnect_timeout: Seconds(3),
            keepalive_timeout: Seconds(120),
            pool: Cell::new(PoolId::P5.pool_ref()),
            _t: PhantomData,
        })))
    }
}

impl<A, T> Connector<A, T>
where
    A: Address,
{
    /// Indicates the initial window size (in octets) for stream-level
    /// flow control for received data.
    ///
    /// The initial window of a stream is used as part of flow control. For more
    /// details, see [`FlowControl`].
    ///
    /// The default value is 65,535.
    ///
    /// [`FlowControl`]: ../struct.FlowControl.html
    pub fn initial_window_size(&self, size: u32) -> &Self {
        self.0
            .borrow_mut()
            .settings
            .set_initial_window_size(Some(size));
        self
    }

    /// Indicates the initial window size (in octets) for connection-level flow control
    /// for received data.
    ///
    /// The initial window of a connection is used as part of flow control. For more details,
    /// see [`FlowControl`].
    ///
    /// The default value is 65,535.
    ///
    /// [`FlowControl`]: ../struct.FlowControl.html
    pub fn initial_connection_window_size(&self, size: u32) -> &Self {
        assert!(size <= consts::MAX_WINDOW_SIZE);
        self.0.borrow_mut().initial_target_connection_window_size = Some(size);
        self
    }

    /// Indicates the size (in octets) of the largest HTTP/2 frame payload that the
    /// configured server is able to accept.
    ///
    /// The sender may send data frames that are **smaller** than this value,
    /// but any data larger than `max` will be broken up into multiple `DATA`
    /// frames.
    ///
    /// The value **must** be between 16,384 and 16,777,215. The default value is 16,384.
    ///
    /// # Panics
    ///
    /// This function panics if `max` is not within the legal range specified
    /// above.
    pub fn max_frame_size(&self, max: u32) -> &Self {
        self.0.borrow_mut().settings.set_max_frame_size(Some(max));
        self
    }

    /// Sets the max size of received header frames.
    ///
    /// This advisory setting informs a peer of the maximum size of header list
    /// that the sender is prepared to accept, in octets. The value is based on
    /// the uncompressed size of header fields, including the length of the name
    /// and value in octets plus an overhead of 32 octets for each header field.
    ///
    /// This setting is also used to limit the maximum amount of data that is
    /// buffered to decode HEADERS frames.
    pub fn max_header_list_size(&self, max: u32) -> &Self {
        self.0
            .borrow_mut()
            .settings
            .set_max_header_list_size(Some(max));
        self
    }

    /// Sets the maximum number of concurrent streams.
    ///
    /// The maximum concurrent streams setting only controls the maximum number
    /// of streams that can be initiated by the remote peer. In other words,
    /// when this setting is set to 100, this does not limit the number of
    /// concurrent streams that can be created by the caller.
    ///
    /// It is recommended that this value be no smaller than 100, so as to not
    /// unnecessarily limit parallelism. However, any value is legal, including
    /// 0. If `max` is set to 0, then the remote will not be permitted to
    /// initiate streams.
    ///
    /// Note that streams in the reserved state, i.e., push promises that have
    /// been reserved but the stream has not started, do not count against this
    /// setting.
    ///
    /// Also note that if the remote *does* exceed the value set here, it is not
    /// a protocol level error. Instead, the `h2` library will immediately reset
    /// the stream.
    ///
    /// See [Section 5.1.2] in the HTTP/2 spec for more details.
    ///
    /// [Section 5.1.2]: https://http2.github.io/http2-spec/#rfc.section.5.1.2
    pub fn max_concurrent_streams(&self, max: u32) -> &Self {
        self.0
            .borrow_mut()
            .settings
            .set_max_concurrent_streams(Some(max));
        self
    }

    /// Sets the maximum number of concurrent locally reset streams.
    ///
    /// When a stream is explicitly reset by either calling
    /// [`SendResponse::send_reset`] or by dropping a [`SendResponse`] instance
    /// before completing the stream, the HTTP/2 specification requires that
    /// any further frames received for that stream must be ignored for "some
    /// time".
    ///
    /// In order to satisfy the specification, internal state must be maintained
    /// to implement the behavior. This state grows linearly with the number of
    /// streams that are locally reset.
    ///
    /// The `max_concurrent_reset_streams` setting configures sets an upper
    /// bound on the amount of state that is maintained. When this max value is
    /// reached, the oldest reset stream is purged from memory.
    ///
    /// Once the stream has been fully purged from memory, any additional frames
    /// received for that stream will result in a connection level protocol
    /// error, forcing the connection to terminate.
    ///
    /// The default value is 10.
    pub fn max_concurrent_reset_streams(&self, max: usize) -> &Self {
        self.0.borrow_mut().reset_stream_max = max;
        self
    }

    /// Sets the maximum number of concurrent locally reset streams.
    ///
    /// When a stream is explicitly reset by either calling
    /// [`SendResponse::send_reset`] or by dropping a [`SendResponse`] instance
    /// before completing the stream, the HTTP/2 specification requires that
    /// any further frames received for that stream must be ignored for "some
    /// time".
    ///
    /// In order to satisfy the specification, internal state must be maintained
    /// to implement the behavior. This state grows linearly with the number of
    /// streams that are locally reset.
    ///
    /// The `reset_stream_duration` setting configures the max amount of time
    /// this state will be maintained in memory. Once the duration elapses, the
    /// stream state is purged from memory.
    ///
    /// Once the stream has been fully purged from memory, any additional frames
    /// received for that stream will result in a connection level protocol
    /// error, forcing the connection to terminate.
    ///
    /// The default value is 10 seconds.
    pub fn reset_stream_duration(&self, dur: Seconds) -> &Self {
        self.0.borrow_mut().reset_stream_duration = dur;
        self
    }

    /// Enables the [extended CONNECT protocol].
    ///
    /// [extended CONNECT protocol]: https://datatracker.ietf.org/doc/html/rfc8441#section-4
    pub fn enable_connect_protocol(&self) -> &Self {
        self.0
            .borrow_mut()
            .settings
            .set_enable_connect_protocol(Some(1));
        self
    }

    /// Set handshake timeout.
    ///
    /// Hadnshake includes receiving preface and completing connection preparation.
    ///
    /// By default handshake timeuot is 5 seconds.
    pub fn handshake_timeout(&self, timeout: Seconds) -> &Self {
        self.0.borrow_mut().handshake_timeout = timeout;
        self
    }

    /// Set server connection disconnect timeout.
    ///
    /// Defines a timeout for disconnect connection. If a disconnect procedure does not complete
    /// within this time, the connection get dropped.
    ///
    /// To disable timeout set value to 0.
    ///
    /// By default disconnect timeout is set to 3 seconds.
    pub fn disconnect_timeout(&self, val: Seconds) -> &Self {
        self.0.borrow_mut().disconnect_timeout = val;
        self
    }

    /// Set keep-alive timeout.
    ///
    /// By default keep-alive time-out is set to 120 seconds.
    pub fn idle_timeout(&self, timeout: Seconds) -> &Self {
        self.0.borrow_mut().keepalive_timeout = timeout;
        self
    }

    /// Set memory pool.
    ///
    /// Use specified memory pool for memory allocations. By default P5
    /// memory pool is used.
    pub fn memory_pool(&self, id: PoolId) -> &Self {
        self.0.borrow_mut().pool.set(id.pool_ref());
        self
    }

    /// Use custom connector
    pub fn connector<U, F>(self, connector: F) -> Connector<A, U>
    where
        F: IntoService<U, Connect<A>>,
        U: Service<Connect<A>, Error = connect::ConnectError>,
        IoBoxed: From<U::Response>,
    {
        let inner = self.0.borrow();
        Connector(Rc::new(RefCell::new(Inner {
            connector: connector.into_service(),
            settings: inner.settings.clone(),
            reset_stream_duration: inner.reset_stream_duration,
            reset_stream_max: inner.reset_stream_max,
            initial_target_connection_window_size: inner.initial_target_connection_window_size,
            handshake_timeout: inner.handshake_timeout,
            disconnect_timeout: inner.disconnect_timeout,
            keepalive_timeout: inner.keepalive_timeout,
            pool: inner.pool.clone(),
            _t: PhantomData,
        })))
    }
}

impl<A, T> Connector<A, T>
where
    A: Address,
    T: Service<Connect<A>, Error = connect::ConnectError>,
    IoBoxed: From<T::Response>,
{
    /// Connect to http2 server
    pub fn connect(
        &self,
        address: A,
    ) -> impl Future<Output = Result<ClientConnection, ClientError>> {
        let fut = timeout_checked(self.0.borrow().handshake_timeout, self._connect(address));
        async move {
            match fut.await {
                Ok(res) => res.map_err(From::from),
                Err(_) => Err(ClientError::HandshakeTimeout),
            }
        }
    }

    fn _connect(&self, address: A) -> impl Future<Output = Result<ClientConnection, ClientError>> {
        let inner = self.0.clone();
        let fut = {
            let slf = inner.borrow();
            slf.connector.call(Connect::new(address))
        };

        async move {
            let io = IoBoxed::from(fut.await?);
            let slf = inner.borrow();
            let codec = Rc::new(Codec::new());
            if let Some(max) = slf.settings.max_frame_size() {
                codec.set_max_recv_frame_size(max as usize);
            }
            if let Some(max) = slf.settings.max_header_list_size() {
                codec.set_max_recv_header_list_size(max as usize);
            }

            // send preface
            let _ = io.with_write_buf(|buf| buf.extend_from_slice(&consts::PREFACE));

            // send setting to the peer
            io.encode(slf.settings.clone().into(), &codec).unwrap();

            let cfg = Config {
                local_init_window_sz: slf
                    .settings
                    .initial_window_size()
                    .unwrap_or(frame::DEFAULT_INITIAL_WINDOW_SIZE),
                initial_max_send_streams: 0,
                local_next_stream_id: 2.into(),
                extended_connect_protocol_enabled: slf
                    .settings
                    .is_extended_connect_protocol_enabled()
                    .unwrap_or(false),
                local_reset_duration: slf.reset_stream_duration,
                local_reset_max: slf.reset_stream_max,
                remote_init_window_sz: frame::DEFAULT_INITIAL_WINDOW_SIZE,
                remote_max_initiated: slf
                    .settings
                    .max_concurrent_streams()
                    .map(|max| max as usize),
            };
            let con = Connection::new(cfg, io.get_ref(), codec.clone());

            Ok(ClientConnection::new(
                io,
                con,
                codec,
                slf.keepalive_timeout,
                slf.disconnect_timeout,
            ))
        }
    }
}
