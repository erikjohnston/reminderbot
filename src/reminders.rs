use chrono::{DateTime, Utc};

use std::mem;

#[derive(Debug, Clone)]
pub struct Reminder {
    pub due: DateTime<Utc>,
    pub text: String,
    pub owner: String,
}

#[derive(Debug, Clone, Default)]
pub struct Reminders {
    reminders: Vec<Reminder>,
}

impl Reminders {
    pub fn new() -> Reminders {
        Reminders {
            reminders: Vec::new(),
        }
    }
    pub fn add_reminder(&mut self, reminder: Reminder) {
        self.reminders.push(reminder);
        self.reminders.sort_by_key(|r| r.due);
    }

    pub fn take_reminders_before(&mut self, now: &DateTime<Utc>) -> Vec<Reminder> {
        let mut before_reminders = mem::replace(&mut self.reminders, Vec::new());

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
}
