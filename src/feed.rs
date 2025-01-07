use std::{collections::HashMap, env, path::PathBuf, str::FromStr, sync::Arc};

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use chrono::Local;
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use feed_rs::{
    model::{Content, Link},
    parser,
};
use futures::future::join_all;
use lazy_static::lazy_static;
use mime_guess::{get_mime_extensions, Mime, MimeGuess};
use regex::{Captures, Regex};
use scraper::{Html, Selector};
use serde_json::json;
use sha2::{Digest, Sha224};
use slugify::slugify;
use tokio::task::JoinHandle;
use url::Url;

use crate::{client::Client, settings::Instance};

lazy_static! {
    static ref CLEAR_SELECTOR: Selector = Selector::parse("iframe, source").unwrap();
    static ref IMG_SELECTOR: Selector = Selector::parse("img").unwrap();
    static ref IMG_REGEX: Regex =
        Regex::new(r#"<\s*img [^>]*(src\s*=\s*"([^"]*)")[^>]*>"#).unwrap();
    static ref EXT_REGEX: Regex = Regex::new(r"\.(\S{2,5})$").unwrap();
}

pub fn program_name() -> String {
    format!("plato-feed/{}", env!("CARGO_PKG_VERSION"))
}

pub async fn load_feed(
    server: Arc<String>,
    instance: Arc<Instance>,
    client: Client,
    library_path: Arc<PathBuf>,
    save_path: Arc<PathBuf>,
) -> Result<Vec<JoinHandle<Result<()>>>> {
    let res = client.get(&instance.url).await?;
    let base = Url::parse(&instance.url).ok().and_then(|u| match u.host() {
        Some(url::Host::Domain(host)) => Some(host.to_owned()),
        _ => None,
    });
    let feed = parser::parse(res.body.as_ref())?;
    let publisher = if let Some(title) = feed.title {
        Arc::new(title.content)
    } else {
        Arc::clone(&server)
    };

    let mut tasks = Vec::new();
    for entry in feed.entries {
        let task = tokio::spawn(load_entry(
            entry,
            base.clone(),
            client.clone(),
            Arc::clone(&library_path),
            Arc::clone(&publisher),
            Arc::clone(&save_path),
            Arc::clone(&instance),
        ));
        tasks.push(task);
    }

    let res: Result<Vec<JoinHandle<Result<()>>>> = Ok(tasks);
    res
}

async fn load_entry(
    entry: feed_rs::model::Entry,
    base: Option<String>,
    client: Client,
    library_path: Arc<PathBuf>,
    publisher: Arc<String>,
    save_path: Arc<PathBuf>,
    server_instance: Arc<Instance>,
) -> Result<()> {
    let mut builder: EpubBuilder<ZipLibrary> =
        EpubBuilder::new(ZipLibrary::new().map_err(|e| anyhow!(e))?).map_err(|e| anyhow!(e))?;

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

    let content = if Some(true) == server_instance.download_full_article {
        download_full_article(entry.links, &mut builder, client)
            .await
            .with_context(|| format!("{} of {}", entry.id, publisher.as_ref()))?
    } else {
        match entry.content {
            Some(Content {
                body: Some(body),
                content_type: _,
                length: _,
                src: _,
            }) => clean_html(body, &mut builder, &base, client).await,
            _ => {
                if Some(false) == server_instance.download_full_article {
                    return Err(anyhow!(
                        "No content for {} of {}",
                        entry.id,
                        publisher.as_ref()
                    ));
                }
                download_full_article(entry.links, &mut builder, client)
                    .await
                    .with_context(|| format!("{} of {}", entry.id, publisher.as_ref()))?
            }
        }
    };

    builder
        .add_content(EpubContent::new("article.html", content.as_ref()))
        .map_err(|e| anyhow!(e))?;

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
}

async fn download_full_article(
    links: Vec<Link>,
    builder: &mut EpubBuilder<ZipLibrary>,
    client: Client,
) -> Result<Bytes> {
    let link = links
        .iter()
        .find(|l| {
            l.media_type
                .as_ref()
                .map(|mt| mt.contains("html"))
                .is_some()
        })
        .or_else(|| links.iter().find(|l| l.media_type.is_none()))
        .or_else(|| links.first())
        .ok_or_else(|| anyhow!("No link to download"))?;

    let res = client.get(link.href.as_str()).await?;
    let html = clean_html(
        String::from_utf8(res.body.to_vec())?,
        builder,
        &Some(link.href.clone()),
        client,
    )
    .await;
    Ok(html)
}

async fn clean_html(
    mut html: String,
    builder: &mut EpubBuilder<ZipLibrary>,
    base_url: &Option<String>,
    client: Client,
) -> Bytes {
    let urls = {
        let mut doc = Html::parse_document(&html);
        let mut elements_to_clear = doc
            .select(&CLEAR_SELECTOR)
            .map(|e| e.id())
            .collect::<Vec<_>>();

        let urls = doc
            .select(&IMG_SELECTOR)
            .map(|elem| {
                elem.attr("src")
                    .ok_or(anyhow!("Failed to match src"))
                    .and_then(|url| {
                        if let Ok(url) = Url::parse(url) {
                            Ok(url)
                        } else if let Some(base_url) = base_url.clone() {
                            let base_url = Url::parse(&base_url)?;
                            Ok(base_url.join(url)?)
                        } else {
                            Err(anyhow!("no host"))
                        }
                    })
                    .map_err(|err| {
                        eprintln!("{err}");
                        elements_to_clear.push(elem.id());
                    })
            })
            .filter_map(|res| res.ok())
            .collect::<Vec<_>>();

        for id in elements_to_clear {
            if let Some(mut m) = doc.tree.get_mut(id) {
                m.detach()
            }
        }

        html = doc.html();
        urls
    };

    let tasks = urls
        .into_iter()
        .map(|url| {
            let client = client.clone();
            tokio::spawn(async move { (url.to_string(), load_img(url, client).await) })
        })
        .collect::<Vec<_>>();
    let tasks = join_all(tasks).await;

    let map = tasks
        .into_iter()
        .enumerate()
        .filter_map(|(i, res)| {
            let (urls, err) = match res {
                Err(err) => (None, Some(anyhow!(err))),
                Ok((url, Err(err))) => (Some((url, None)), Some(err)),
                Ok((url, Ok(img))) => {
                    let path = format!(
                        "{i}.{}",
                        img.ext
                            .or_else(|| get_mime_extensions(&img.mime)
                                .and_then(|e| e.first())
                                .copied()
                                .map(|x| x.to_owned()))
                            .unwrap_or_default()
                    );

                    match builder.add_resource(&path, img.bytes.as_ref(), img.mime.as_ref()) {
                        Err(err) => (Some((url, None)), Some(anyhow!(err))),
                        Ok(_) => (Some((url, Some(path))), None),
                    }
                }
            };

            if let Some(err) = err {
                eprintln!("{err}");
            }

            urls
        })
        .filter_map(|(a, b)| b.map(|b| (a, b)))
        .collect::<HashMap<_, _>>();

    Bytes::copy_from_slice(
        IMG_REGEX
            .replace_all(&html, |caps: &Captures| {
                caps.get(2)
                    .and_then(|src| map.get(src.as_str()))
                    .map(|src| format!(r#"<img src="{src}" />"#))
                    .unwrap_or_default()
            })
            .as_bytes(),
    )
}

struct Img {
    bytes: Bytes,
    mime: Mime,
    ext: Option<String>,
}

async fn load_img(url: Url, client: Client) -> Result<Img> {
    let ext = EXT_REGEX
        .captures(url.path())
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned());

    let res = client.get(url).await?;
    let mime = res
        .content_type
        .and_then(|h| Mime::from_str(h.to_str().ok()?).ok())
        .or_else(|| {
            ext.clone()
                .and_then(|ext| MimeGuess::from_ext(&ext).first())
        })
        .ok_or(anyhow!("Failed to get mimetype"))?;

    Ok(Img {
        bytes: res.body,
        mime,
        ext,
    })
}
