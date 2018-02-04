#![feature(conservative_impl_trait)]

#[macro_use]
extern crate failure;
extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
#[macro_use]
extern crate slog;
extern crate slog_async;
extern crate slog_term;
extern crate tokio_core;
extern crate tokio_signal;
extern crate tokio_timer;
extern crate toml;
extern crate twilio_rust;

use hyper::Client;
use hyper_tls::HttpsConnector;
use futures::{Future, Stream};
use slog::Drain;
use twilio_rust::messages::{MessageFrom, Messages, OutboundMessageBuilder};

use std::fs::File;
use std::io::Read;

mod matrix;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    matrix: MatrixConfig,
    twilio: TwilioConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct MatrixConfig {
    host: String,
    access_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TwilioConfig {
    account_sid: String,
    auth_token: String,
    from_num: String,
    to_num: String,
}

fn main() {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    let logger = slog::Logger::root(drain, o!());

    info!(logger, "Initialising");

    let config: Config = {
        let mut f = File::open("config.toml").expect("couldn't find config.toml");
        let mut s = String::new();
        f.read_to_string(&mut s)
            .expect("failed to read config.toml");

        toml::from_str(&s).expect("failed to parse config")
    };

    let mut core = tokio_core::reactor::Core::new().expect("start tokio core");

    let handle = core.handle();
    let connector = HttpsConnector::new(4, &handle).expect("tls setup");

    let client = Client::configure().connector(connector).build(&handle);

    let timer = tokio_timer::wheel().build();

    let twilio_client = twilio_rust::Client::new(
        &config.twilio.account_sid,
        &config.twilio.auth_token,
        &core.handle(),
    ).expect("failed to set up twilio client");

    let event_handler = EventHandler {
        client: twilio_client,
        logger: logger.clone(),
        from_num: config.twilio.from_num.clone(),
        to_num: config.twilio.to_num.clone(),
    };

    info!(logger, "Starting");

    let syncer = matrix::Syncer::new(
        client,
        config.matrix.host.clone(),
        config.matrix.access_token.clone(),
        timer,
        logger.clone(),
    );

    let mut sync_stopper = syncer.stopper();

    let ctrl_c = tokio_signal::ctrl_c(&handle)
        .flatten_stream()
        .for_each(move |()| {
            sync_stopper.stop();
            Ok(())
        })
        .map_err(|_| ());
    handle.spawn(ctrl_c);

    core.run(syncer.run().for_each(move |res| {
        match res {
            Ok(resp) => {
                if resp.is_live {
                    for (room_id, event) in resp.sync_response.events() {
                        info!(logger, "Got event";
                            "room" => room_id,
                            "sender" => &event.sender,
                        );
                        handle.spawn(event_handler.handle_event(room_id, event))
                    }
                }
            }
            Err(err) => error!(logger, "Error"; "err" => %err),
        }

        Ok(())
    })).expect("sync stream failed");
}

struct EventHandler {
    client: twilio_rust::Client,
    logger: slog::Logger,
    from_num: String,
    to_num: String,
}

impl EventHandler {
    fn handle_event(
        &self,
        _room_id: &str,
        event: &matrix::types::Event,
    ) -> Box<Future<Item = (), Error = ()>> {
        if event.etype != "m.room.message" {
            return Box::new(futures::future::ok(()));
        }

        let body_opt = event.content.get("body").and_then(|value| value.as_str());

        let body = if let Some(body) = body_opt {
            body
        } else {
            return Box::new(futures::future::ok(()));
        };

        if !body.starts_with("!test") {
            return Box::new(futures::future::ok(()));
        }

        info!(self.logger, "Sending sms");

        let messages = Messages::new(&self.client);

        let outbound_sms = OutboundMessageBuilder::new_sms(
            MessageFrom::From(&self.from_num),
            &self.to_num,
            "Hello from Rust!",
        ).build();

        let logger = self.logger.clone();

        let f = messages.send_message(&outbound_sms).then(move |res| {
            match res {
                Ok(msg) => if let Some(error) = msg.error_message {
                    error!(logger, "Error from twilio"; "error" => error);
                } else {
                    info!(logger, "Message sent"; "status" => ?msg.status)
                },
                Err(err) => error!(logger, "Error sending sms"; "error" => ?err),
            }

            Ok(())
        });

        Box::new(f)
    }
}
