use chrone::{DateTime, Utc};
use tokio_timer::Timer;

use std::mem;

#[derive(Debug, Clone)]
struct Reminder {
    due: DateTime<Utc>,
    text: String,
    owner: String,
}

#[derive(Debug, Clone, Default)]
struct Reminders {
    reminders: Vec<Reminder>,
}

impl Reminders {
    pub fn add_reminder(&mut self, reminder: Reminder) {
        self.reminders.push(reminder);
        self.reminders.sort_by_key(|r| r.due);
    }

    pub fn add_reminders<I>(&mut self, reminders: I) where I: IntoIterator<Item=Reminder> {
        self.reminders.extend(reminders)
        self.reminders.sort_by_key(|r| r.due);
    }

    pub fn take_reminders_before(&mut self, now: DateTime<Utc>) -> Vec<Reminder> {
        let before_reminders = mem::replace(&mut self.reminders, Vec::new());

        let pos = match before_reminders.binary_search_by_key(now, |r| r.due) {
            Ok(pos) => pos,
            Err(pos) => pos,
        };

        // split_off truncates before_reminders to pos and returns a new vec
        // everything after.
        let after_reminders = before_reminders.split_off(pos);

        self.reminders = after_reminders;

        before_reminders
    }

    pub fn get_next_due(&self) -> OptionM<DateTime<Utc>> {
        self.reminders.first().map(|r| r.due)
    }
}