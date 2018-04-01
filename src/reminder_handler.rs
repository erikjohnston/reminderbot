use chrono::Utc;
use db::{Reminder, Reminders};
use failure::ResultExt;
use futures::{future, Future};
use slog::Logger;
use tokio_core::reactor::Handle;
use twilio_rust::Client;
use twilio_rust::messages::{MessageFrom, Messages, OutboundMessageBuilder};

use Config;
use db::AddressBook;

pub struct ReminderHandler {
    logger: Logger,
    client: Client,
    config: Config,
    reminders: Reminders,
    address_book: AddressBook,
}

impl ReminderHandler {
    pub fn new(
        logger: Logger,
        client: Client,
        config: Config,
        reminders: Reminders,
        address_book: AddressBook,
    ) -> ReminderHandler {
        ReminderHandler {
            logger,
            client,
            config,
            reminders,
            address_book,
        }
    }

    pub fn do_reminders(&self, handle: &Handle) {
        let now = Utc::now();

        let reminders = self.reminders
            .get_reminders_before(&now)
            .expect("failed to get reminders from database");

        for reminder in reminders {
            let f = self.handle_reminder(&reminder);
            handle.spawn(f);

            self.reminders
                .delete_reminder(&reminder.id)
                .expect("failed to delete from database");
        }
    }

    fn handle_reminder(&self, reminder: &Reminder) -> Box<Future<Item = (), Error = ()>> {
        let logger = self.logger.new(o!("id" => reminder.id.clone()));

        info!(logger, "Sending message");

        let msisdn_res = self.address_book
            .get_msisdn_for_user(&reminder.destination)
            .context("failed to get msisdn from DB");

        let msisdn = match msisdn_res {
            Ok(Some(msisdn)) => msisdn,
            Ok(None) => {
                warn!(logger, "Failed to find msisdn"; "destination" => reminder.destination.clone());
                return Box::new(future::ok(()));
            }
            Err(err) => {
                error!(logger, "Failed to get msisdn"; "destination" => reminder.destination.clone(), "err" => %err);
                return Box::new(future::ok(()));
            }
        };

        let messages = Messages::new(&self.client);

        let outbound_sms = OutboundMessageBuilder::new_sms(
            MessageFrom::From(&self.config.twilio.from_num),
            &msisdn,
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

        Box::new(f)
    }
}
