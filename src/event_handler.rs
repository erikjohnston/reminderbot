use chrono;
use db::{Reminder, Reminders};
use futures::{future, Future, Stream};
use hyper::client::connect::Connect;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng, ThreadRng};
use regex::Regex;
use slog::Logger;
use tokio_core::reactor::Handle;

use date::parse_human_datetime;
use matrix::types::Event;
use matrix::{MessageSender, Syncer};

pub struct EventHandler {
    logger: Logger,
    reminders: Reminders,
    rng: ThreadRng,
    message_sender: Box<MessageSender>,
}

impl EventHandler {
    pub fn new(
        logger: Logger,
        reminders: Reminders,
        message_sender: Box<MessageSender>,
    ) -> EventHandler {
        EventHandler {
            logger,
            reminders,
            rng: thread_rng(),
            message_sender,
        }
    }

    pub fn start_from_sync<C: Connect + 'static>(
        mut self,
        handle: Handle,
        syncer: Syncer<C>,
    ) -> impl Future<Item = (), Error = ()> {
        syncer.run().for_each(move |res| {
            match res {
                Ok(resp) => {
                    if resp.is_live {
                        for (room_id, event) in resp.sync_response.events() {
                            handle.spawn(self.handle_event(room_id, event))
                        }
                    }
                }
                Err(err) => error!(self.logger, "Error"; "err" => %err),
            }

            Ok(())
        })
    }

    fn handle_event(&mut self, room_id: &str, event: &Event) -> Box<Future<Item = (), Error = ()>> {
        let id: String = self.rng.sample_iter(&Alphanumeric).take(20).collect();

        let logger = self.logger.new(o!("id" => id.clone()));

        info!(logger, "Got event";
            "room" => room_id,
            "sender" => &event.sender,
        );

        if event.etype != "m.room.message" {
            return Box::new(future::ok(()));
        }

        let body_opt = event.content.get("body").and_then(|value| value.as_str());

        let body = if let Some(body) = body_opt {
            body
        } else {
            return Box::new(future::ok(()));
        };

        if !body.starts_with("testbot:") {
            return Box::new(future::ok(()));
        }

        let reminder_regex =
            Regex::new(r"^testbot:\s+remind\s*me\s+(.*)\s+to\s+(.*)$").expect("invalid regex");
        if let Some(capt) = reminder_regex.captures(body) {
            let at = &capt[1];
            let text = &capt[2];

            let now = chrono::Utc::now();
            let due = match parse_human_datetime(at, now) {
                Ok(date) => date,
                Err(_) => {
                    info!(logger, "Failed to parse date {}", at);
                    return self
                        .message_sender
                        .send_text_message(room_id, &format!("Error: Failed to parse date {}", at));
                }
            };

            if due < now {
                info!(logger, "Due date in past: {}", due);
                return self.message_sender.send_text_message(
                    room_id,
                    &format!("Error: Due date in past: {}", due.to_rfc2822()),
                );
            }

            info!(
                logger,
                "Queuing message to be sent at '{}'",
                due.to_rfc2822(),
            );

            let res = self.reminders.add_reminder(&Reminder {
                id,
                due,
                text: String::from(text),
                destination: event.sender.clone(),
            });

            if let Err(err) = res {
                error!(logger, "Failed to handle reminder"; "error" => %err);
                return self.message_sender.send_text_message(
                    room_id,
                    &format!("Error: Failed to persist reminder: {}", err),
                );
            } else {
                return self.message_sender.send_text_message(
                    room_id,
                    &format!("Queuing message to be sent at '{}'", due.to_rfc2822()),
                );
            }

        // TODO: persist.
        } else {
            info!(logger, "Unrecognized command");
        }

        Box::new(future::ok(()))
    }
}
