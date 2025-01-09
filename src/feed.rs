use std::{path::PathBuf, sync::Arc};

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use chrono::{Local, Utc};
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use feed_rs::{
    model::{Content, Link},
    parser,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use url::Url;

use crate::{client::Client, html::clean_html, settings::Instance};

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

    Ok(tasks)
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

    let mut authors: Vec<String> = entry
        .authors
        .into_iter()
        .map(|a| a.name)
        .filter(|a| !a.is_empty())
        .collect();
    if authors.is_empty() {
        let author = server_instance
            .default_author
            .as_ref()
            .unwrap_or(&publisher);
        if !author.is_empty() {
            authors.push(author.to_owned());
        }
    }
    let author = authors.join(", ");
    builder.set_authors(authors);

    builder.set_generator(program_name());

    let date = if let Some(date) = entry.published {
        date
    } else if let Some(date) = entry.updated {
        date
    } else {
        Utc::now()
    };
    builder.set_publication_date(date);
    let date = date.format("%Y-%m-%dT%H:%M:%S").to_string();

    let title = if let Some(title) = entry.title {
        builder.set_title(&title.content);
        title.content
    } else {
        entry.id.clone()
    };

    let mut hasher = Sha256::new();
    hasher.update(&entry.id);
    let filename = format!("{:x}.epub", hasher.finalize());
    let filename = save_path.join(filename);
    if filename.exists() {
        return Ok(());
    }

    let path = filename.strip_prefix(library_path.as_ref())?;

    let content = if Some(true) == server_instance.download_full_article {
        download_full_article(entry.links, &mut builder, client, server_instance)
            .await
            .with_context(|| format!("{} of {}", entry.id, publisher.as_ref()))?
    } else {
        match entry.content {
            Some(Content {
                body: Some(body),
                content_type: _,
                length: _,
                src: _,
            }) => {
                clean_html(
                    body,
                    &mut builder,
                    &base,
                    client,
                    server_instance.include_images,
                    false,
                    &None,
                )
                .await
            }
            _ => {
                if Some(false) == server_instance.download_full_article {
                    return Err(anyhow!(
                        "No content for {} of {}",
                        entry.id,
                        publisher.as_ref()
                    ));
                }
                download_full_article(entry.links, &mut builder, client, server_instance)
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
            "year": date,
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
    server_instance: Arc<Instance>,
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
        server_instance.include_images,
        server_instance.enable_filter,
        &server_instance.filter_element,
    )
    .await;
    Ok(html)
}
