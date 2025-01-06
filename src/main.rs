mod settings;

use std::{
    cmp::min,
    env,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, format_err, Context, Error, Result};
use chrono::Local;
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use feed_rs::parser;
use futures::future::join_all;
use lazy_static::lazy_static;
use regex::Regex;
use reqwest::Client;
use serde_json::json;
use settings::Settings;
use sha2::{Digest, Sha224};
use slugify::slugify;
use tokio::{fs, sync::Semaphore, task::JoinHandle};

lazy_static! {
    static ref CLEAR_REGEX: Regex =
        Regex::new(r"<\s*img[^>]*>|<\s*iframe[^>]*>(.*</\s*iframe\s*>)?").unwrap();
}

async fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let library_path = PathBuf::from(
        args.next()
            .ok_or_else(|| format_err!("missing argument: library path"))?,
    );
    let save_path = PathBuf::from(
        args.next()
            .ok_or_else(|| format_err!("missing argument: save path"))?,
    );

    let settings = Settings::load().with_context(|| "failed to load settings")?;

    if !save_path.exists() {
        fs::create_dir(&save_path).await?;
    }

    let sigterm = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm))?;

    // Create directory for each instance name in the save path.
    if settings.use_server_name_directories {
        let mut tasks = Vec::with_capacity(settings.servers.len());
        for name in settings.servers.keys() {
            let instance_path = save_path.join(name);
            let sigterm = Arc::clone(&sigterm);
            let task = tokio::spawn(async move {
                if sigterm.load(Ordering::Relaxed) {
                    return Err(anyhow!("SIGTERM"));
                }

                if !instance_path.exists() {
                    fs::create_dir(&instance_path).await?;
                }

                Ok::<(), Error>(())
            });
            tasks.push(task);
        }

        let err: Vec<Error> = join_all(tasks)
            .await
            .into_iter()
            .filter_map(|t| {
                match t {
                    Ok(ok) => ok,
                    Err(err) => Err(anyhow!(err)),
                }
                .err()
            })
            .collect();

        if !err.is_empty() {
            return Err(anyhow!(
                "Failed to create server name directories: {:?}",
                err
            ));
        }
    }

    let client = Client::builder().user_agent(program_name()).build()?;
    let client = Arc::new(client);

    let semaphore = Semaphore::new(min(settings.concurrent_requests, Semaphore::MAX_PERMITS));
    let semaphore = Arc::new(semaphore);

    let mut tasks = Vec::with_capacity(settings.servers.len());
    for (server, instance) in settings.servers {
        let library_path = library_path.clone();
        let save_path = if settings.use_server_name_directories {
            save_path.join(&server)
        } else {
            save_path.clone()
        };

        let client = Arc::clone(&client);
        let semaphore = Arc::clone(&semaphore);
        let sigterm = Arc::clone(&sigterm);
        let task = tokio::spawn(async move {
            let permit = semaphore.acquire().await?;
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            let res = client.get(instance.url).send().await?;
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            let body = res.bytes().await?;
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            drop(permit);
            let feed = parser::parse(body.as_ref())?;
            let publisher = feed.title.map_or_else(|| server.clone(), |t| t.content);
            let mut tasks = Vec::new();
            for entry in feed.entries {
                let library_path = library_path.clone();
                let save_path = save_path.clone();
                let publisher = publisher.clone();
                let sigterm = Arc::clone(&sigterm);
                let task = tokio::spawn(async move {
                    if sigterm.load(Ordering::Relaxed) {
                        return Err(anyhow!("SIGTERM"));
                    }

                    let mut builder = EpubBuilder::new(ZipLibrary::new().map_err(|e| anyhow!(e))?)
                        .map_err(|e| anyhow!(e))?;

                    let authors: Vec<String> = entry.authors.into_iter().map(|a| a.name).collect();
                    let author = authors.join(", ");
                    builder.set_authors(authors);
                    builder.set_generator(program_name());

                    let mut filename = Vec::new();
                    let year = if let Some(date) = entry.published {
                        filename.push(date.format("%Y-%m-%dT%H:%M:%S").to_string());
                        let year = date.format("%Y").to_string();
                        builder.set_publication_date(date);
                        year
                    } else {
                        String::default()
                    };

                    let title = if let Some(title) = entry.title {
                        filename.push(slugify!(&title.content, max_length = 32));
                        builder.set_title(&title.content);
                        title.content
                    } else {
                        entry.id.clone()
                    };

                    let mut hasher = Sha224::new();
                    hasher.update(&entry.id);
                    filename.push(format!("{:x}.epub", hasher.finalize()));
                    let filename = save_path.join(&filename.join("_"));
                    if filename.exists() {
                        return Ok(());
                    }

                    let path = filename.strip_prefix(&library_path)?;

                    if let Some(content) = entry.content.and_then(|c| c.body) {
                        let content = CLEAR_REGEX.replace_all(&content, "");
                        builder
                            .add_content(EpubContent::new("article.html", content.as_bytes()))
                            .map_err(|e| anyhow!(e))?;
                    }

                    if let Some(content) = entry.summary {
                        builder.add_description(content.content);
                    }

                    let file = std::fs::File::create(&filename)?;
                    builder.generate(&file).map_err(|e| anyhow!(e))?;
                    let event = json!({
                        "type": "addDocument",
                        "info": {
                            "title": title,
                            "author": author,
                            "year": year,
                            "publisher": publisher,
                            "identifier": entry.id,
                            "added": Local::now().naive_local(),
                            "file": {
                                "path": path,
                                "kind": "epub",
                                "size": file.metadata().ok().map_or(0, |m| m.len()),
                            }
                        },
                    });
                    println!("{event}");
                    Ok(())
                });
                tasks.push(task);
            }

            let res: Result<Vec<JoinHandle<Result<()>>>> = Ok(tasks);
            res
        });
        tasks.push(task);
    }

    for result in join_all(tasks).await {
        match result {
            Err(e) => eprintln!("{}", e),
            Ok(Err(e)) => eprintln!("{}", e),
            Ok(Ok(tasks)) => {
                for result in join_all(tasks).await {
                    match result {
                        Err(e) => eprintln!("{}", e),
                        Ok(Err(e)) => eprintln!("{}", e),
                        Ok(Ok(_)) => (),
                    }
                }
            }
        }
    }

    Ok(())
}

fn program_name() -> String {
    format!("plato-feed/{}", env!("CARGO_PKG_VERSION"))
}

#[tokio::main]
async fn main() -> Result<()> {
    log_panics::init();
    if let Err(err) = run().await {
        eprintln!("Error: {:#}", err);
        fs::write("feed_error.txt", format!("{:#}", err)).await.ok();
        return Err(err);
    }

    Ok(())
}
