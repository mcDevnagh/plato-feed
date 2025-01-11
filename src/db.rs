use std::{
    collections::HashMap,
    fs::File,
    future::Future,
    io::{BufReader, BufWriter},
    path::PathBuf,
};

use ::anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Serializer;
use tokio::sync::Mutex;

const DB_PATH: &str = "db.json";

#[derive(Clone, Deserialize, Default, Serialize)]
struct Entry {
    path: PathBuf,
    last_update: DateTime<Utc>,
}

#[derive(Deserialize, Default, Serialize)]
struct JsonDatabase {
    feeds: HashMap<String, Entry>,
}

struct Inner {
    prev: JsonDatabase,
    new: JsonDatabase,
}

pub struct Db(Mutex<Inner>);

impl Db {
    pub fn new() -> Result<Self> {
        let path = PathBuf::from(DB_PATH);
        let inner = if !path.exists() {
            Inner {
                prev: JsonDatabase::default(),
                new: JsonDatabase::default(),
            }
        } else {
            let f = File::open(path)?;
            let reader = BufReader::new(f);
            Inner {
                prev: serde_json::from_reader(reader)?,
                new: JsonDatabase::default(),
            }
        };

        Ok(Db(Mutex::new(inner)))
    }

    pub async fn update<T: Future<Output = Result<PathBuf, E>>, E>(
        &self,
        id: String,
        updated: Option<DateTime<Utc>>,
        save_file: T,
    ) -> Result<(), E> {
        let mut inner = self.0.lock().await;
        match inner.prev.feeds.remove(&id) {
            // no need to update; just keep the previous entry
            Some(entry) if updated.is_none_or(|u| entry.last_update >= u) => {
                inner.new.feeds.insert(id, entry);
                Ok(())
            }
            // upsert!
            entry => match save_file.await {
                // update succeeded! get new entry!
                Ok(path) => {
                    inner.new.feeds.insert(
                        id,
                        Entry {
                            path,
                            last_update: updated.unwrap_or_else(Utc::now),
                        },
                    );
                    Ok(())
                }
                Err(err) => {
                    if let Some(entry) = entry {
                        // failed to update; just keep the previous entry if it exists
                        inner.new.feeds.insert(id, entry);
                    }

                    Err(err)
                }
            },
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let writer = match File::create(DB_PATH) {
            Ok(f) => BufWriter::new(f),
            Err(err) => {
                eprintln!("feed: {err}");
                return;
            }
        };

        let inner = self.0.get_mut();
        for (feed_name, feed) in inner.prev.feeds.drain() {
            if !inner.new.feeds.contains_key(&feed_name) {
                inner.new.feeds.insert(feed_name, feed);
            }
        }

        let mut serializer = Serializer::pretty(writer);
        if let Err(err) = inner.new.serialize(&mut serializer) {
            eprintln!("feed: {err}");
        }
    }
}
