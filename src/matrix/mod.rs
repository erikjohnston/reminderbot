use hyper;
use serde_json;
use futures::{future, stream, Future, Stream};
use failure::{Error, ResultExt};
use slog::Logger;
use tokio_timer::Timer;

use std::time::Duration;
use std::rc::Rc;
use std::cell::RefCell;

use futures_flag::{Flag, FutureExt};

pub mod types;

use self::types::{SyncResponse, SyncStreamItem};

type ClientService = hyper::server::Service<
    Request = hyper::client::Request<hyper::Body>,
    Response = hyper::client::Response,
    Error = hyper::Error,
    Future = hyper::client::FutureResponse,
>;

#[derive(Fail, Debug)]
#[fail(display = "Syncer was stopped")]
struct StopError;

#[derive(Debug, Clone, Default)]
struct SyncState {
    errored: bool,
    is_live: bool,
    next_batch: Option<String>,
}

pub struct Syncer {
    state: Rc<RefCell<SyncState>>,
    client: Box<ClientService>,
    stop_flag: Flag,
    base_host: String,
    access_token: String,
    timer: Timer,
    logger: Logger,
}

impl Syncer {
    pub fn new<C: hyper::client::Connect>(
        client: hyper::Client<C>,
        base_host: String,
        access_token: String,
        timer: Timer,
        logger: Logger,
        stop_flag: Flag,
    ) -> Syncer {
        Syncer {
            state: Rc::default(),
            client: Box::new(client),
            stop_flag,
            base_host,
            access_token,
            timer,
            logger,
        }
    }

    fn create_request(&self) -> hyper::client::Request<hyper::Body> {
        let url = if let Some(ref nb) = self.state.borrow().next_batch {
            format!(
                "{}/_matrix/client/r0/sync?since={}&timeout=60000",
                self.base_host, nb
            )
        } else {
            format!("{}/_matrix/client/r0/sync", self.base_host)
        };

        trace!(self.logger, "Using url: {}", url);

        let url: hyper::Uri = url.parse().expect("valid sync url");

        let mut request = hyper::Request::new(hyper::Method::Get, url);
        request
            .headers_mut()
            .set(hyper::header::Authorization(hyper::header::Bearer {
                token: self.access_token.clone(),
            }));

        request
    }

    fn do_sync(&mut self) -> Box<Future<Item = SyncStreamItem, Error = Error>> {
        let request = self.create_request();

        // If we've previously errored getting the sync, lets back off
        // a bit
        let sleep_fut = if self.state.borrow().errored {
            Box::new(
                self.timer
                    .sleep(Duration::from_secs(5))
                    .map_err(Error::from),
            ) as Box<Future<Item = _, Error = Error>>
        } else {
            Box::new(future::ok(()))
        };

        let request_future = self.client
            .call(request)
            .then(|res| res.context("Failed to make HTTP sync request"))
            .from_err()
            .with_flag(self.stop_flag.clone(), StopError.into());

        let logger = self.logger.clone();
        let logger2 = self.logger.clone();
        let state = self.state.clone();
        let state2 = self.state.clone();

        let f = sleep_fut
            .with_flag(self.stop_flag.clone(), StopError.into())
            .and_then(move |_| {
                trace!(logger, "Making sync request");
                request_future
            })
            .and_then(|res| {
                if res.status().is_success() {
                    Ok(res)
                } else {
                    Err(format_err!("Got HTTP response: {}", res.status()))
                }
            })
            .and_then(|res| res.body().concat2().from_err())
            .and_then(|body: hyper::Chunk| {
                let body: SyncResponse =
                    serde_json::from_slice(&body).context("Failed to parse sync response")?;
                Ok(body)
            })
            .map(move |sync_response| {
                let is_live = state2.borrow().is_live;

                SyncStreamItem {
                    sync_response,
                    is_live,
                }
            })
            .then(move |res| {
                trace!(logger2, "Got Response");

                if let Err(ref err) = res {
                    if err.downcast_ref::<StopError>().is_none() {
                        debug!(logger2, "Response Error"; "err" => %err);
                    }
                }

                // Set the error state
                state.borrow_mut().errored = res.is_err();

                if let Ok(ref resp) = res {
                    state.borrow_mut().next_batch = Some(resp.sync_response.next_batch.clone());
                    state.borrow_mut().is_live = true;
                }

                res
            });

        Box::new(f)
    }

    pub fn run(mut self) -> Box<Stream<Item = Result<SyncStreamItem, Error>, Error = ()>> {
        let logger = self.logger.clone();

        let stream = stream::repeat(())
            .and_then(move |_| self.do_sync().then(Ok))
            .take_while(move |res| match *res {
                Err(ref error) => match error.downcast_ref::<StopError>() {
                    Some(_) => {
                        info!(logger, "Stopping sync stream");
                        Ok(false)
                    }
                    _ => Ok(true),
                },
                _ => Ok(true),
            });

        Box::new(stream)
    }
}
