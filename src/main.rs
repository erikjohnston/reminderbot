#![feature(conservative_impl_trait)]

extern crate chrono;
#[macro_use]
extern crate failure;
extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate regex;
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
extern crate rusqlite;

use hyper::Client;
use hyper_tls::HttpsConnector;
use futures::{Future, Stream};
use slog::Drain;
use twilio_rust::messages::{MessageFrom, Messages, OutboundMessageBuilder};
use regex::Regex;

use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod date;
mod matrix;
mod reminders;


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
    // Set up logging

    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    let logger = slog::Logger::root(drain, o!());

    info!(logger, "Initialising");

    // Parse config

    let config: Config = {
        let mut f = File::open("config.toml").expect("couldn't find config.toml");
        let mut s = String::new();
        f.read_to_string(&mut s)
            .expect("failed to read config.toml");

        toml::from_str(&s).expect("failed to parse config")
    };

    // Set up tokio

    let mut core = tokio_core::reactor::Core::new().expect("start tokio core");
    let handle = core.handle();

    // Set up reminders handling

    let reminders = Arc::new(Mutex::new(reminders::Reminders::new()));

    spawn_reminder_loop(&config, &logger, &handle, reminders.clone());

    // Set up matrix::Syncer

    let connector = HttpsConnector::new(4, &handle).expect("tls setup");
    let http_client = Client::configure().connector(connector).build(&handle);

    let syncer = matrix::Syncer::new(
        http_client,
        config.matrix.host.clone(),
        config.matrix.access_token.clone(),
        tokio_timer::Timer::default(),
        logger.clone(),
    );

    // Set up graceful shutdown

    let mut sync_stopper = syncer.stopper();
    let ctrl_c = tokio_signal::ctrl_c(&handle)
        .flatten_stream()
        .for_each(move |()| {
            sync_stopper.stop();
            Ok(())
        })
        .map_err(|_| ());
    handle.spawn(ctrl_c);

    // Set up main event handling code

    let mut event_handler = EventHandler {
        logger: logger.clone(),
        reminders: reminders.clone(),
    };

    // Actually start syncing from matrix

    info!(logger, "Starting");

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
    logger: slog::Logger,
    reminders: Arc<Mutex<reminders::Reminders>>,
}

impl EventHandler {
    fn handle_event(
        &mut self,
        _room_id: &str,
        event: &matrix::types::Event,
    ) -> Box<Future<Item = (), Error = ()>> {
        if event.etype != "m.room.message" {
            return Box::new(futures::future::ok(()));
        }

        let body_opt = event.content.get("body")
            .and_then(|value| value.as_str());

        let body = if let Some(body) = body_opt {
            body
        } else {
            return Box::new(futures::future::ok(()));
        };

        if !body.starts_with("testbot:") {
            return Box::new(futures::future::ok(()));
        }

        info!(self.logger, "Got message: {}...", &body[..20]);

        let reminder_regex = Regex::new(r"^\s+testbot:\s+remindme\s+(.*)\s+to\s+(.*)$").expect("invalid regex");
        if let Some(capt) = reminder_regex.captures(body) {
            let at = &capt[1];
            let text = &capt[2];

            let now = chrono::Utc::now();
            let due = match date::parse_human_datetime(at, now) {
                Ok(date) => date,
                Err(_) => {
                    // TODO: Report back error
                    info!(self.logger, "Failed to parse date {}", at);
                    return Box::new(futures::future::ok(()));
                }
            };            

            if due < now {
                // TODO: Report back error
                info!(self.logger, "Due date in past: {}", due);                
                return Box::new(futures::future::ok(()));
            }

            info!(self.logger, "Queuing message to be sent at {}", due);

            self.reminders.lock().expect("lock was poisoned")
                .add_reminder(reminders::Reminder {
                    due,
                    text: String::from(text),
                    owner: event.sender.clone(),
                });

            // TODO: persist.
        } else {
            info!(self.logger, "Unrecognized command");            
        }

        return Box::new(futures::future::ok(()));
    }
}


fn spawn_reminder_loop(
    config: &Config,
    logger: &slog::Logger,
    handle: &tokio_core::reactor::Handle,
    reminders: Arc<Mutex<reminders::Reminders>>,
) {
    let twilio_client = twilio_rust::Client::new(
        &config.twilio.account_sid,
        &config.twilio.auth_token,
        handle,
    ).expect("failed to set up twilio client");

    let from_num = config.twilio.from_num.clone();
    let to_num = config.twilio.to_num.clone();

    let logger = logger.clone();
    let handle2 = handle.clone();

    let s = tokio_timer::Timer::default().interval(Duration::from_millis(200))
    .for_each(move |_| {
        let now = chrono::Utc::now();
        let events = reminders.lock().expect("lock was poisoned")
            .take_reminders_before(&now);

        for event in events {
            let messages = Messages::new(&twilio_client);

            let outbound_sms = OutboundMessageBuilder::new_sms(
                MessageFrom::From(&from_num),
                &to_num,
                &event.text,
            ).build();

            let logger = logger.clone();

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

            handle2.spawn(f);
        }

        Ok(())
    }).map_err(|_| ());

    handle.spawn(s);
}