use chrono::{DateTime, TimeZone, Utc};
use failure::{Error, ResultExt};
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct Reminder {
    pub id: String,
    pub due: DateTime<Utc>,
    pub destination: String,
    pub text: String,
}

#[derive(Debug)]
pub struct Reminders {
    conn: Connection,
}

impl Reminders {
    pub fn with_connection(conn: Connection) -> Result<Reminders, Error> {
        conn.execute_batch(REMINDERS_SCHEMA)
            .context("failed to create reminders schema")?;

        Ok(Reminders { conn })
    }
    pub fn add_reminder(&mut self, reminder: &Reminder) -> Result<(), Error> {
        self.conn
            .prepare_cached(
                "INSERT INTO reminders (id, due_ts, destination, text, sent) VALUES (?,?,?,?,?)",
            )
            .context("failed to create insert statement")?
            .execute(&[
                &reminder.id,
                &reminder.due.timestamp(),
                &reminder.destination,
                &reminder.text,
                &false,
            ])
            .context("failed to insert query")?;

        Ok(())
    }

    pub fn get_reminders_before(&mut self, now: &DateTime<Utc>) -> Result<Vec<Reminder>, Error> {
        let mut stmt = self.conn
            .prepare_cached("SELECT id, due_ts, destination, text FROM reminders WHERE due_ts <= ? AND NOT sent")
            .context("failed to create select statement")?;

        let vec = stmt.query_map(&[&now.timestamp()], |row| Reminder {
            id: row.get(0),
            due: Utc.timestamp(row.get(1), 0),
            destination: row.get(2),
            text: row.get(3),
        }).context("failed to execute select query")?
            .collect::<Result<_, _>>()
            .context("failed to read results of query")?;

        Ok(vec)
    }

    pub fn delete_reminder(&mut self, id: &str) -> Result<(), Error> {
        self.conn
            .prepare_cached("UPDATE reminders SET sent = ? WHERE id = ?")
            .context("failed to create delete statement")?
            .execute(&[&true, &id])?;

        Ok(())
    }
}

const REMINDERS_SCHEMA: &str = r"
    CREATE TABLE IF NOT EXISTS reminders (
        id TEXT PRIMARY KEY,
        due_ts BIGINT NOT NULL,
        destination TEXT NOT NULL,
        text NOT NULL,
        sent BOOL NOT NULL
    );

    CREATE INDEX IF NOT EXISTS reminders_ts ON reminders (due_ts, sent);
";
