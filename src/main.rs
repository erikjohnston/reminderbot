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
#[macro_use]
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

use futures::{Future, Stream};
use hyper::Client;
use hyper_tls::HttpsConnector;
use rusqlite::Connection;
use slog::Drain;
use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

mod date;
mod db;
mod event_handler;
mod futures_flag;
mod matrix;
mod reminder_handler;

use db::{AddressBook, Reminders};
use event_handler::EventHandler;
use reminder_handler::ReminderHandler;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
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
    // to_num: String,
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

    let database = Arc::new(Connection::open(&config.database).expect("failed to open datbase"));

    // Set up reminders handling

    let reminders = Reminders::with_connection(database.clone()).expect("failed to open reminders");

    let address_book = AddressBook::with_connection(database).expect("failed to open address book");

    let twilio_client = twilio_rust::Client::new(
        &config.twilio.account_sid,
        &config.twilio.auth_token,
        &handle,
    ).expect("failed to set up twilio client");

    let reminder_handler = ReminderHandler::new(
        logger.clone(),
        twilio_client,
        config.clone(),
        reminders.clone(),
        address_book,
    );

    let reminder_loop = spawn_reminder_loop(handle.clone(), reminder_handler);
    handle.spawn(reminder_loop);

    // Set up matrix::Syncer

    let connector = HttpsConnector::new(4).expect("tls setup");
    let http_client = Client::builder().build(connector);

    let mut stop_flag = futures_flag::Flag::new();

    let syncer = matrix::Syncer::new(
        http_client.clone(),
        config.matrix.host.clone(),
        config.matrix.access_token.clone(),
        logger.clone(),
        stop_flag.clone(),
    );

    // Set up graceful shutdown

    let ctrl_c = tokio_signal::ctrl_c()
        .flatten_stream()
        .for_each(move |()| {
            // We got a SIGINT, lets stop things gracefully.
            stop_flag.set();
            Ok(())
        })
        .map_err(|_| ());
    handle.spawn(ctrl_c);

    // Set up matrix message sender

    let message_sender = matrix::MessageSenderHyper::new(
        http_client,
        config.matrix.host.clone(),
        config.matrix.access_token.clone(),
        logger.clone(),
    );

    // Set up main event handling code

    let event_handler =
        EventHandler::new(logger.clone(), reminders.clone(), Box::new(message_sender));

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
    handle: tokio_core::reactor::Handle,
    handler: ReminderHandler,
) -> impl Future<Item = (), Error = ()> {
    tokio_timer::Interval::new(std::time::Instant::now(), Duration::from_millis(500))
        .for_each(move |_| {
            handler.do_reminders(&handle);

            Ok(())
        })
        .map_err(|_| ())
}
