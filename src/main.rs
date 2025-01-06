mod plato;
mod settings;

use std::{
    cmp::min,
    env,
    path::PathBuf,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, format_err, Context, Error, Result};
use bytes::Bytes;
use chrono::Local;
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use feed_rs::parser;
use futures::future::join_all;
use lazy_static::lazy_static;
use mime_guess::{get_mime_extensions, Mime, MimeGuess};
use regex::Regex;
use reqwest::{header::CONTENT_TYPE, Client};
use serde_json::json;
use settings::Settings;
use sha2::{Digest, Sha224};
use slugify::slugify;
use tokio::{fs, sync::Semaphore, task::JoinHandle};
use url::Url;

lazy_static! {
    static ref CLEAR_REGEX: Regex =
        Regex::new(r"<\s*source[^>]*>(.*</\s*source\s*>)?|<\s*iframe[^>]*>(.*</\s*iframe\s*>)?")
            .unwrap();
    static ref IMG_REGEX: Regex =
        Regex::new(r#"<\s*img [^>]*(src\s*=\s*"([^"]*)")[^>]*>"#).unwrap();
    static ref EXT_REGEX: Regex = Regex::new(r"\.(\S{2,5})$").unwrap();
}

async fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let library_path = PathBuf::from(
        args.next()
            .ok_or_else(|| format_err!("missing argument: library path"))?,
    );
    let library_path = Arc::new(library_path);

    let save_path = PathBuf::from(
        args.next()
            .ok_or_else(|| format_err!("missing argument: save path"))?,
    );
    let save_path = Arc::new(save_path);

    let settings = Settings::load().with_context(|| "failed to load settings")?;

    let wifi = args
        .next()
        .ok_or_else(|| format_err!("missing argument: wifi status"))
        .and_then(|v| v.parse::<bool>().map_err(Into::into))?;
    let online = args
        .next()
        .ok_or_else(|| format_err!("missing argument: online status"))
        .and_then(|v| v.parse::<bool>().map_err(Into::into))?;

    if !online {
        if !wifi {
            plato::notify("Establishing a network connection.");
            plato::set_wifi(true);
        } else {
            plato::notify("Waiting for the network to come up.");
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
    }

    if !save_path.exists() {
        fs::create_dir(save_path.as_ref()).await?;
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
        let client = Arc::clone(&client);
        let library_path = Arc::clone(&library_path);
        let save_path = if settings.use_server_name_directories {
            Arc::new(save_path.join(&server))
        } else {
            Arc::clone(&save_path)
        };
        let semaphore = Arc::clone(&semaphore);
        let server = Arc::new(server);
        let sigterm = Arc::clone(&sigterm);
        let task = tokio::spawn(async move {
            let permit = semaphore.acquire().await?;
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            let res = client.get(&instance.url).send().await?;
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            let body = res.bytes().await?;
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            drop(permit);
            let base = Url::parse(&instance.url).ok().and_then(|u| match u.host() {
                Some(url::Host::Domain(host)) => Some(host.to_owned()),
                _ => None,
            });
            let feed = parser::parse(body.as_ref())?;
            let publisher = if let Some(title) = feed.title {
                Arc::new(title.content)
            } else {
                Arc::clone(&server)
            };

            let mut tasks = Vec::new();
            for entry in feed.entries {
                let base = base.clone();
                let client = Arc::clone(&client);
                let library_path = Arc::clone(&library_path);
                let publisher = Arc::clone(&publisher);
                let save_path = Arc::clone(&save_path);
                let semaphore = Arc::clone(&semaphore);
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
                    let filename = save_path.join(filename.join("_"));
                    if filename.exists() {
                        return Ok(());
                    }

                    let path = filename.strip_prefix(library_path.as_ref())?;

                    if let Some(content) = entry.content {
                        if let Some(mut body) = content.body {
                            let href = if content.content_type.subty() == "html" {
                                body = CLEAR_REGEX.replace_all(&body, "").to_string();
                                let tasks = IMG_REGEX
                                    .captures_iter(&body)
                                    .map(|c| {
                                        let img = c.get(0).unwrap().as_str().to_owned();
                                        let url = c
                                            .get(2)
                                            .ok_or(anyhow!("Failed to match src"))
                                            .and_then(|s| Ok(Url::parse(s.as_str())?))
                                            .and_then(|mut u| {
                                                if u.has_host() {
                                                    Ok(u)
                                                } else if let Some(base) = base.clone() {
                                                    u.set_host(Some(&base))?;
                                                    Ok(u)
                                                } else {
                                                    Err(anyhow!("no host"))
                                                }
                                            });
                                        let client = Arc::clone(&client);
                                        let semaphore = Arc::clone(&semaphore);
                                        let sigterm = Arc::clone(&sigterm);
                                        let task = tokio::spawn(async move {
                                            match url {
                                                Err(err) => Err(err),
                                                Ok(url) => {
                                                    let ext = EXT_REGEX
                                                        .captures(url.path())
                                                        .and_then(|c| c.get(1))
                                                        .map(|m| m.as_str().to_owned());

                                                    let permit = semaphore.acquire().await?;
                                                    if sigterm.load(Ordering::Relaxed) {
                                                        return Err(anyhow!("SIGTERM"));
                                                    }

                                                    let res = client.get(url).send().await?;
                                                    if sigterm.load(Ordering::Relaxed) {
                                                        return Err(anyhow!("SIGTERM"));
                                                    }

                                                    let mime = res
                                                        .headers()
                                                        .get(CONTENT_TYPE)
                                                        .and_then(|h| h.to_str().ok())
                                                        .and_then(|mt| Mime::from_str(mt).ok())
                                                        .or_else(|| {
                                                            ext.clone().and_then(|ext| {
                                                                MimeGuess::from_ext(&ext).first()
                                                            })
                                                        })
                                                        .ok_or(anyhow!("Failed to get mimetype"))?;

                                                    let bytes = res.bytes().await?;
                                                    drop(permit);
                                                    if sigterm.load(Ordering::Relaxed) {
                                                        return Err(anyhow!("SIGTERM"));
                                                    }

                                                    Ok((bytes, mime, ext))
                                                }
                                            }
                                        });
                                        (img, task)
                                    })
                                    .collect::<Vec<(
                                        String,
                                        JoinHandle<Result<(Bytes, Mime, Option<String>)>>,
                                    )>>();

                                for (i, (img, task)) in tasks.into_iter().enumerate() {
                                    match task.await {
                                        Err(err) => eprintln!("{err}"),
                                        Ok(Err(err)) => eprintln!("{err}"),
                                        Ok(Ok((bytes, mime, ext))) => {
                                            let path = format!(
                                                "{i}.{}",
                                                ext.or_else(|| get_mime_extensions(&mime)
                                                    .and_then(|e| e.first())
                                                    .copied()
                                                    .map(|x| x.to_owned()))
                                                    .unwrap_or_default()
                                            );
                                            if let Err(err) = builder.add_resource(
                                                &path,
                                                bytes.as_ref(),
                                                mime.as_ref(),
                                            ) {
                                                eprintln!("{}", err);
                                            } else {
                                                body = body.replace(
                                                    &img,
                                                    &format!(r#"<img src="{path}"/>"#),
                                                );
                                                continue;
                                            }
                                        }
                                    }

                                    body = body.replace(&img, "");
                                }

                                "article.html"
                            } else {
                                "article.txt"
                            };

                            builder
                                .add_content(EpubContent::new(href, body.as_bytes()))
                                .map_err(|e| anyhow!(e))?;
                        }
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
                            "publisher": publisher.as_ref(),
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
