use hyper;
use serde_json;
use futures::{self, future, stream, Future, Stream};
use failure::{Error, ResultExt};
use slog::Logger;
use tokio_timer::Timer;

use std::time::Duration;
use std::sync::{Arc, Mutex};
use std::rc::Rc;
use std::cell::RefCell;

pub mod types;

use self::types::{SyncResponse, SyncStreamItem};

#[derive(Fail, Debug)]
#[fail(display = "Syncer was stopped")]
struct StopError;

#[derive(Debug, Clone, Default)]
struct SyncState {
    errored: bool,
    is_live: bool,
    next_batch: Option<String>,
}

struct StopInner {
    stopped: bool,
    sender: Option<futures::sync::oneshot::Sender<()>>,
}

impl StopInner {
    fn new() -> StopInner {
        StopInner {
            stopped: false,
            sender: None,
        }
    }
}

pub struct StopSyncer {
    inner: Arc<Mutex<StopInner>>,
}

impl StopSyncer {
    pub fn stop(&mut self) {
        let mut stop = self.inner.lock().unwrap();

        stop.stopped = true;

        if let Some(sender) = stop.sender.take() {
            sender.send(()).ok();
        }
    }
}

pub struct Syncer<C: Sized + Clone> {
    state: Rc<RefCell<SyncState>>,
    client: hyper::Client<C>,
    stop: Arc<Mutex<StopInner>>,
    base_host: String,
    access_token: String,
    timer: Timer,
    logger: Logger,
}

impl<C> Syncer<C>
where
    C: hyper::client::Connect + Clone + 'static,
{
    pub fn new(
        client: hyper::Client<C>,
        base_host: String,
        access_token: String,
        timer: Timer,
        logger: Logger,
    ) -> Syncer<C> {
        Syncer {
            state: Rc::default(),
            stop: Arc::new(Mutex::new(StopInner::new())),
            client,
            base_host,
            access_token,
            timer,
            logger,
        }
    }

    pub fn stopper(&self) -> StopSyncer {
        StopSyncer {
            inner: self.stop.clone(),
        }
    }

    fn do_sync(&mut self) -> Box<Future<Item = SyncStreamItem, Error = Error>> {
        let state = self.state.clone();

        let url = if let Some(ref nb) = state.borrow().next_batch {
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

        // If we've previously errored getting the sync, lets back off
        // a bit
        let sleep_fut = if state.borrow().errored {
            Box::new(
                self.timer
                    .sleep(Duration::from_secs(5))
                    .map_err(Error::from),
            ) as Box<Future<Item = _, Error = Error>>
        } else {
            Box::new(future::ok(()))
        };

        let stop_fut = {
            let mut stop = self.stop.lock().unwrap();

            if stop.stopped {
                future::Either::A(future::err(StopError))
            } else {
                let (sender, receiver) = futures::sync::oneshot::channel();

                stop.sender = Some(sender);

                future::Either::B(receiver.then(|_| Err(StopError)))
            }
        };

        let http_client = self.client.clone();

        let logger = self.logger.clone();
        let logger2 = self.logger.clone();
        let state2 = state.clone();

        let f = sleep_fut
            .and_then(move |_| {
                trace!(logger, "Making sync request");
                http_client
                    .request(request)
                    .then(|res| res.context("Failed to make HTTP sync request"))
                    .from_err()
                    .and_then(|res| {
                        if res.status().is_success() {
                            future::Either::A(res.body().concat2().from_err().and_then(
                                |body: hyper::Chunk| {
                                    let body: SyncResponse = serde_json::from_slice(&body)
                                        .context("Failed to parse sync response")?;
                                    Ok(body)
                                },
                            ))
                        } else {
                            future::Either::B(future::err(format_err!(
                                "Got HTTP response: {}",
                                res.status()
                            )))
                        }
                    })
                    .select(stop_fut.from_err())
                    .map(|(r, _)| r)
                    .map_err(|(e, _)| e)
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
                    debug!(logger2, "Response Error"; "err" => %err);
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
        let stop = self.stop.clone();
        let logger = self.logger.clone();

        let stream = stream::repeat(())
            .take_while(move |_| {
                // Check if we should stop or not
                let stop = stop.lock().unwrap();
                if stop.stopped {
                    info!(logger, "Stopping");
                    Ok(false)
                } else {
                    Ok(true)
                }
            })
            .and_then(move |_| self.do_sync().then(|res| Ok(res)))
            .filter_map(|res| {
                // We might get a StopError, which we can just discard since we'll stop
                // due to the take_while above.
                match res {
                    Ok(val) => Some(Ok(val)),
                    Err(error) => match error.downcast::<StopError>() {
                        Ok(_) => return None,
                        Err(err) => Some(Err(err)),
                    },
                }
            });

        Box::new(stream)
    }
}
