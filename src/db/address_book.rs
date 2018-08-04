use std::sync::Arc;

use failure::{Error, ResultExt};
use rusqlite::Connection;

const ADDRESS_BOOK_SCHEMA: &str = r"
    CREATE TABLE IF NOT EXISTS address_book (
        user_id TEXT PRIMARY KEY,
        msisdn TEXT NOT NULL
    );
";

#[derive(Debug, Clone)]
pub struct AddressBook {
    conn: Arc<Connection>,
}

impl AddressBook {
    pub fn with_connection(conn: Arc<Connection>) -> Result<AddressBook, Error> {
        conn.execute_batch(ADDRESS_BOOK_SCHEMA)
            .context("failed to create address book schema")?;

        Ok(AddressBook { conn })
    }

    pub fn get_msisdn_for_user(&self, user_id: &str) -> Result<Option<String>, Error> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT msisdn FROM address_book WHERE user_id = ?")
            .context("failed to create select statement")?;

        let rows = stmt.query_map(&[&user_id], |row| row.get(0))?;

        for row in rows {
            return Ok(Some(row?));
        }

        Ok(None)
    }
}
