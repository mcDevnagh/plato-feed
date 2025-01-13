use std::{collections::HashMap, str::FromStr};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use epub_builder::{EpubBuilder, ZipLibrary};
use futures::future::join_all;
use lazy_static::lazy_static;
use mime_guess::{get_mime_extensions, Mime, MimeGuess};
use regex::{Captures, Regex};
use scraper::{selectable::Selectable, Html, Selector};
use url::Url;

use crate::{client::Client, plato::notify};

lazy_static! {
    static ref CLEAR_SELECTOR: Selector = Selector::parse(
        r"
br,
form,
hr,
iframe,
input,
script,
source,
style"
    )
    .unwrap();
    static ref IMG_SELECTOR: Selector = Selector::parse("img").unwrap();
    static ref IMG_REGEX: Regex =
        Regex::new(r#"<\s*img [^>]*(src\s*=\s*"([^"]*)")[^>]*>"#).unwrap();
    static ref EXT_REGEX: Regex = Regex::new(r"\.(\S{2,5})$").unwrap();
    static ref FILTER_ELEMENTS: Vec<Selector> = {
        vec![
            Selector::parse("article").unwrap(),
            Selector::parse("main").unwrap(),
            Selector::parse("div#main").unwrap(),
            Selector::parse("#main-article").unwrap(),
            Selector::parse(".main-content").unwrap(),
            Selector::parse("#body").unwrap(),
            Selector::parse("#content").unwrap(),
            Selector::parse(".content").unwrap(),
            Selector::parse("div#article").unwrap(),
            Selector::parse("div.article").unwrap(),
            Selector::parse("div.post").unwrap(),
            Selector::parse("div.post-outer").unwrap(),
            Selector::parse(".l-root").unwrap(),
            Selector::parse(".content-container").unwrap(),
            Selector::parse(".StandardArticleBody_body").unwrap(),
            Selector::parse("div#article-inner").unwrap(),
            Selector::parse("div#newsstorytext").unwrap(),
            Selector::parse("div.general").unwrap(),
        ]
    };
}

fn get_urls<'a, T: Selectable<'a>>(doc: T, base_url: &Option<String>) -> Vec<Url> {
    doc.select(&IMG_SELECTOR)
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
                    eprintln!("feed: {err}");
                })
        })
        .filter_map(|res| res.ok())
        .collect::<Vec<_>>()
}

pub async fn clean_html(
    mut html: String,
    builder: &mut EpubBuilder<ZipLibrary>,
    base_url: &Option<String>,
    client: Client,
    include_images: bool,
    enable_filter: bool,
    filter_element: &Option<String>,
) -> Bytes {
    let urls = {
        let mut doc = Html::parse_document(&html);
        let elements_to_clear = doc
            .select(&CLEAR_SELECTOR)
            .map(|e| e.id())
            .collect::<Vec<_>>();

        for id in elements_to_clear {
            if let Some(mut m) = doc.tree.get_mut(id) {
                m.detach()
            }
        }

        let mut urls = None;
        if enable_filter {
            for filter in filter_element
                .as_ref()
                .and_then(|e| Selector::parse(e).ok())
                .iter()
                .chain(FILTER_ELEMENTS.iter())
            {
                if let Some(elem) = doc.select(filter).next() {
                    urls = if include_images {
                        Some(get_urls(elem, base_url))
                    } else {
                        Some(Vec::new())
                    };
                    html = elem.html();
                    break;
                }
            }
        }

        urls.unwrap_or_else(|| {
            html = doc.html();
            if include_images {
                get_urls(&doc, base_url)
            } else {
                Vec::new()
            }
        })
    };

    if urls.len() > 1 {
        notify(&format!("loading {} images", urls.len()));
    } else if urls.len() > 0 {
        notify("loading 1 image");
    }

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
                eprintln!("feed: {err}");
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
