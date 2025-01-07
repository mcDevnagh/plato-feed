use std::{
    env,
    path::PathBuf,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use chrono::Local;
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use feed_rs::parser;
use lazy_static::lazy_static;
use mime_guess::{get_mime_extensions, Mime, MimeGuess};
use regex::Regex;
use reqwest::{header::CONTENT_TYPE, Client};
use serde_json::json;
use sha2::{Digest, Sha224};
use slugify::slugify;
use tokio::{sync::Semaphore, task::JoinHandle};
use url::Url;

use crate::settings::Instance;

lazy_static! {
    static ref CLEAR_REGEX: Regex =
        Regex::new(r"<\s*source[^>]*>(.*</\s*source\s*>)?|<\s*iframe[^>]*>(.*</\s*iframe\s*>)?")
            .unwrap();
    static ref IMG_REGEX: Regex =
        Regex::new(r#"<\s*img [^>]*(src\s*=\s*"([^"]*)")[^>]*>"#).unwrap();
    static ref EXT_REGEX: Regex = Regex::new(r"\.(\S{2,5})$").unwrap();
}

pub fn program_name() -> String {
    format!("plato-feed/{}", env!("CARGO_PKG_VERSION"))
}

pub async fn load_feed(
    server: Arc<String>,
    instance: Instance,
    client: Arc<Client>,
    library_path: Arc<PathBuf>,
    save_path: Arc<PathBuf>,
    semaphore: Arc<Semaphore>,
    sigterm: Arc<AtomicBool>,
) -> Result<Vec<JoinHandle<Result<()>>>> {
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
        let task = tokio::spawn(load_entry(Entry {
            entry,
            base: base.clone(),
            client: Arc::clone(&client),
            library_path: Arc::clone(&library_path),
            publisher: Arc::clone(&publisher),
            save_path: Arc::clone(&save_path),
            semaphore: Arc::clone(&semaphore),
            sigterm: Arc::clone(&sigterm),
        }));
        tasks.push(task);
    }

    let res: Result<Vec<JoinHandle<Result<()>>>> = Ok(tasks);
    res
}

struct Entry {
    entry: feed_rs::model::Entry,
    base: Option<String>,
    client: Arc<Client>,
    library_path: Arc<PathBuf>,
    publisher: Arc<String>,
    save_path: Arc<PathBuf>,
    semaphore: Arc<Semaphore>,
    sigterm: Arc<AtomicBool>,
}

async fn load_entry(entry: Entry) -> Result<()> {
    if entry.sigterm.load(Ordering::Relaxed) {
        return Err(anyhow!("SIGTERM"));
    }

    let mut builder: EpubBuilder<ZipLibrary> =
        EpubBuilder::new(ZipLibrary::new().map_err(|e| anyhow!(e))?).map_err(|e| anyhow!(e))?;

    let authors: Vec<String> = entry.entry.authors.into_iter().map(|a| a.name).collect();
    let author = authors.join(", ");
    builder.set_authors(authors);
    builder.set_generator(program_name());

    let mut filename = Vec::new();
    let year = if let Some(date) = entry.entry.published {
        filename.push(date.format("%Y-%m-%dT%H:%M:%S").to_string());
        let year = date.format("%Y").to_string();
        builder.set_publication_date(date);
        year
    } else {
        String::default()
    };

    let title = if let Some(title) = entry.entry.title {
        filename.push(slugify!(&title.content, max_length = 32));
        builder.set_title(&title.content);
        title.content
    } else {
        entry.entry.id.clone()
    };

    let mut hasher = Sha224::new();
    hasher.update(&entry.entry.id);
    filename.push(format!("{:x}.epub", hasher.finalize()));
    let filename = entry.save_path.join(filename.join("_"));
    if filename.exists() {
        return Ok(());
    }

    let path = filename.strip_prefix(entry.library_path.as_ref())?;

    if let Some(content) = entry.entry.content {
        if let Some(mut body) = content.body {
            let href = if content.content_type.subty() == "html" {
                body = clean_html(
                    body,
                    &mut builder,
                    &entry.base,
                    entry.client,
                    entry.semaphore,
                    entry.sigterm,
                )
                .await;
                "article.html"
            } else {
                "article.txt"
            };

            builder
                .add_content(EpubContent::new(href, body.as_bytes()))
                .map_err(|e| anyhow!(e))?;
        }
    }

    if let Some(content) = entry.entry.summary {
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
            "publisher": entry.publisher.as_ref(),
            "identifier": entry.entry.id,
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
}

async fn clean_html(
    html: String,
    builder: &mut EpubBuilder<ZipLibrary>,
    base_url: &Option<String>,
    client: Arc<Client>,
    semaphore: Arc<Semaphore>,
    sigterm: Arc<AtomicBool>,
) -> String {
    let mut html = CLEAR_REGEX.replace_all(&html, "").to_string();
    let tasks = IMG_REGEX
        .captures_iter(&html)
        .map(|c| {
            let img = c.get(0).unwrap().as_str().to_owned();
            let url = c
                .get(2)
                .ok_or(anyhow!("Failed to match src"))
                .and_then(|s| Ok(Url::parse(s.as_str())?))
                .and_then(|mut u| {
                    if u.has_host() {
                        Ok(u)
                    } else if let Some(base_url) = base_url.clone() {
                        u.set_host(Some(&base_url))?;
                        Ok(u)
                    } else {
                        Err(anyhow!("no host"))
                    }
                });
            let client = Arc::clone(&client);
            let semaphore = Arc::clone(&semaphore);
            let sigterm = Arc::clone(&sigterm);
            let task = tokio::spawn(load_img(url, client, semaphore, sigterm));
            (img, task)
        })
        .collect::<Vec<(String, JoinHandle<Result<Img>>)>>();

    for (i, (original_img, task)) in tasks.into_iter().enumerate() {
        match task.await {
            Err(err) => eprintln!("{err}"),
            Ok(Err(err)) => eprintln!("{err}"),
            Ok(Ok(img)) => {
                let path = format!(
                    "{i}.{}",
                    img.ext
                        .or_else(|| get_mime_extensions(&img.mime)
                            .and_then(|e| e.first())
                            .copied()
                            .map(|x| x.to_owned()))
                        .unwrap_or_default()
                );
                if let Err(err) = builder.add_resource(&path, img.bytes.as_ref(), img.mime.as_ref())
                {
                    eprintln!("{}", err);
                } else {
                    html = html.replace(&original_img, &format!(r#"<img src="{path}"/>"#));
                    continue;
                }
            }
        }

        html = html.replace(&original_img, "");
    }

    html
}

struct Img {
    bytes: Bytes,
    mime: Mime,
    ext: Option<String>,
}

async fn load_img(
    url: Result<Url>,
    client: Arc<Client>,
    semaphore: Arc<Semaphore>,
    sigterm: Arc<AtomicBool>,
) -> Result<Img> {
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
                    ext.clone()
                        .and_then(|ext| MimeGuess::from_ext(&ext).first())
                })
                .ok_or(anyhow!("Failed to get mimetype"))?;

            let bytes = res.bytes().await?;
            drop(permit);
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            Ok(Img { bytes, mime, ext })
        }
    }
}
