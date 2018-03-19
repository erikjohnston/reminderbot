#![feature(conservative_impl_trait)]

extern crate chrono;
#[macro_use]
extern crate failure;
extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate linear_map;
extern crate rand;
extern crate regex;
extern crate rusqlite;
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
use rusqlite::Connection;
use slog::Drain;
use twilio_rust::messages::{MessageFrom, Messages, OutboundMessageBuilder};

use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod date;
mod event_handler;
mod matrix;
mod reminders;
mod futures_flag;

use event_handler::EventHandler;
use reminders::Reminders;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    matrix: MatrixConfig,
    twilio: TwilioConfig,
    database: String,
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

    let logger = setup_logging();

    info!(logger, "Initialising");

    // Parse config

    let config = parse_config();

    // Set up tokio

    let mut core = tokio_core::reactor::Core::new().expect("start tokio core");
    let handle = core.handle();

    // Set up database

    let database = Connection::open(&config.database).expect("failed to open datbase");

    // Set up reminders handling

    let reminders = Arc::new(Mutex::new(
        Reminders::with_connection(database).expect("failed to open reminders"),
    ));

    let reminder_loop =
        spawn_reminder_loop(&config, logger.clone(), handle.clone(), reminders.clone());
    handle.spawn(reminder_loop);

    // Set up matrix::Syncer

    let connector = HttpsConnector::new(4, &handle).expect("tls setup");
    let http_client = Client::configure().connector(connector).build(&handle);

    let mut stop_flag = futures_flag::Flag::new();

    let syncer = matrix::Syncer::new(
        http_client,
        config.matrix.host.clone(),
        config.matrix.access_token.clone(),
        tokio_timer::Timer::default(),
        logger.clone(),
        stop_flag.clone(),
    );

    // Set up graceful shutdown

    let ctrl_c = tokio_signal::ctrl_c(&handle)
        .flatten_stream()
        .for_each(move |()| {
            // We got a SIGINT, lets stop things gracefully.
            stop_flag.set();
            Ok(())
        })
        .map_err(|_| ());
    handle.spawn(ctrl_c);

    // Set up main event handling code

    let event_handler = EventHandler::new(logger.clone(), reminders.clone());

    // Actually start syncing from matrix

    info!(logger, "Starting");

    core.run(event_handler.start_from_sync(handle, syncer))
        .expect("sync stream failed");
}

fn setup_logging() -> slog::Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    slog::Logger::root(drain, o!())
}

fn parse_config() -> Config {
    let mut f = File::open("config.toml").expect("couldn't find config.toml");
    let mut s = String::new();
    f.read_to_string(&mut s)
        .expect("failed to read config.toml");

    toml::from_str(&s).expect("failed to parse config")
}

fn spawn_reminder_loop(
    config: &Config,
    logger: slog::Logger,
    handle: tokio_core::reactor::Handle,
    reminders: Arc<Mutex<reminders::Reminders>>,
) -> impl Future<Item = (), Error = ()> {
    let twilio_client = twilio_rust::Client::new(
        &config.twilio.account_sid,
        &config.twilio.auth_token,
        &handle,
    ).expect("failed to set up twilio client");

    let from_num = config.twilio.from_num.clone();
    let to_num = config.twilio.to_num.clone();

    tokio_timer::Timer::default()
        .interval(Duration::from_millis(200))
        .for_each(move |_| {
            let now = chrono::Utc::now();
            let events = reminders
                .lock()
                .expect("lock was poisoned")
                .get_reminders_before(&now)
                .expect("failed to get reminders from database");

            for event in events {
                let logger = logger.new(o!("id" => event.id.clone()));

                info!(logger, "Sending message");

                let messages = Messages::new(&twilio_client);

                let outbound_sms = OutboundMessageBuilder::new_sms(
                    MessageFrom::From(&from_num),
                    &to_num,
                    &event.text,
                ).build();

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

                handle.spawn(f);

                reminders
                    .lock()
                    .expect("lock was poisoned")
                    .delete_reminder(&event.id)
                    .expect("failed to delete from database");
            }

            Ok(())
        })
        .map_err(|_| ())
}
