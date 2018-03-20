use std::sync::{Arc, Mutex};

use chrono::Utc;
use futures::Future;
use reminders::{Reminder, Reminders};
use slog::Logger;
use tokio_core::reactor::Handle;
use twilio_rust::Client;
use twilio_rust::messages::{MessageFrom, Messages, OutboundMessageBuilder};

use Config;

pub struct ReminderHandler {
    logger: Logger,
    client: Client,
    config: Config,
    reminders: Arc<Mutex<Reminders>>,
}

impl ReminderHandler {
    pub fn new(
        logger: Logger,
        client: Client,
        config: Config,
        reminders: Arc<Mutex<Reminders>>,
    ) -> ReminderHandler {
        ReminderHandler {
            logger,
            client,
            config,
            reminders,
        }
    }

    pub fn do_reminders(&self, handle: &Handle) {
        let now = Utc::now();

        let reminders = self.reminders
            .lock()
            .expect("lock was poisoned")
            .get_reminders_before(&now)
            .expect("failed to get reminders from database");

        for reminder in reminders {
            let f = self.handle_reminder(&reminder);
            handle.spawn(f);
        }
    }

    fn handle_reminder(&self, reminder: &Reminder) -> impl Future<Item = (), Error = ()> {
        let logger = self.logger.new(o!("id" => reminder.id.clone()));

        info!(logger, "Sending message");

        let messages = Messages::new(&self.client);

        let outbound_sms = OutboundMessageBuilder::new_sms(
            MessageFrom::From(&self.config.twilio.from_num),
            &self.config.twilio.to_num,
            &reminder.text,
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

        self.reminders
            .lock()
            .expect("lock was poisoned")
            .delete_reminder(&reminder.id)
            .expect("failed to delete from database");

        f
    }
}
