use failure::{Error, ResultExt};
use futures::{future, stream, Future, Stream};
use hyper;
use hyper::client::connect::Connect;
use serde_json;
use slog::Logger;
use tokio_timer::sleep;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use futures_flag::{Flag, FutureExt};

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

pub struct Syncer<C: Connect + 'static> {
    state: Rc<RefCell<SyncState>>,
    client: hyper::Client<C>,
    stop_flag: Flag,
    base_host: String,
    access_token: String,
    logger: Logger,
}

impl<C> Syncer<C>
where
    C: Connect + 'static,
{
    pub fn new(
        client: hyper::Client<C>,
        base_host: String,
        access_token: String,
        logger: Logger,
        stop_flag: Flag,
    ) -> Syncer<C> {
        Syncer {
            state: Rc::default(),
            client,
            stop_flag,
            base_host,
            access_token,
            logger,
        }
    }

    fn create_request(&self) -> hyper::Request<hyper::Body> {
        let url = if let Some(ref nb) = self.state.borrow().next_batch {
            format!(
                "{}/_matrix/client/r0/sync?since={}&timeout=60000",
                self.base_host, nb
            )
        } else {
            format!("{}/_matrix/client/r0/sync", self.base_host)
        };

        trace!(self.logger, "Using url: {}", url);

        hyper::Request::get(url)
            .header(
                "Authorization",
                &format!("Bearer {}", &self.access_token) as &str,
            )
            .body(hyper::Body::empty())
            .expect("valid http request")
    }

    fn do_sync(&mut self) -> Box<Future<Item = SyncStreamItem, Error = Error>> {
        let request = self.create_request();

        // If we've previously errored getting the sync, lets back off
        // a bit
        let sleep_fut = if self.state.borrow().errored {
            Box::new(sleep(Duration::from_secs(5)).map_err(Error::from))
                as Box<Future<Item = _, Error = Error>>
        } else {
            Box::new(future::ok(()))
        };

        let request_future = self
            .client
            .request(request)
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
            .and_then(|res| res.into_body().concat2().from_err())
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

pub trait MessageSender {
    fn send_text_message(&self, room_id: &str, msg: &str) -> Box<Future<Item = (), Error = ()>>;
}

pub struct MessageSenderHyper<C: Connect + 'static> {
    client: hyper::Client<C>,
    base_host: String,
    access_token: String,
    logger: Logger,
}

impl<C> MessageSenderHyper<C>
where
    C: Connect + 'static,
{
    pub fn new(
        client: hyper::Client<C>,
        base_host: String,
        access_token: String,
        logger: Logger,
    ) -> MessageSenderHyper<C> {
        MessageSenderHyper {
            client,
            base_host,
            access_token,
            logger,
        }
    }
}

impl<C> MessageSender for MessageSenderHyper<C>
where
    C: Connect + 'static,
{
    fn send_text_message(&self, room_id: &str, msg: &str) -> Box<Future<Item = (), Error = ()>> {
        let content = serde_json::to_vec(&json!({
            "body": msg,
            "msgtype": "m.text",
        })).expect("valid json");

        let url = format!(
            "{}/_matrix/client/r0/rooms/{}/send/m.room.message",
            self.base_host, room_id
        );

        info!(self.logger, "Sending message"; "url" => &url);

        let request = hyper::Request::post(url)
            .header(
                "Authorization",
                &format!("Bearer {}", &self.access_token) as &str,
            )
            .body(hyper::Body::from(content))
            .expect("valid http request");

        let logger = self.logger.clone();
        let logger2 = self.logger.clone();
        let fut = self
            .client
            .request(request)
            .map(move |_| {
                info!(logger, "Sent message");
            })
            .map_err(move |err| {
                error!(logger2, "Failed to send matrix message"; "error" => %err);
            });

        Box::new(fut)
    }
}
