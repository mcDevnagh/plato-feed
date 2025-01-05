mod settings;

use std::{cmp::min, env, path::PathBuf, sync::Arc};

use anyhow::{anyhow, format_err, Context, Error, Result};
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use feed_rs::parser;
use futures::future::join_all;
use reqwest::Client;
use settings::Settings;
use sha2::{Digest, Sha224};
use slugify::slugify;
use tokio::{fs, sync::Semaphore, task::JoinHandle};

async fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let _library_path = PathBuf::from(
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

    // Create directory for each instance name in the save path.
    if settings.use_server_name_directories {
        let mut tasks = Vec::with_capacity(settings.servers.len());
        for name in settings.servers.keys() {
            let instance_path = save_path.join(name);
            let task = tokio::spawn(async move {
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

    let client = Client::builder()
        .user_agent(format!("plato-feed/{}", env!("CARGO_PKG_VERSION")))
        .build()?;
    let client = Arc::new(client);

    let semaphore = Semaphore::new(min(settings.concurrent_requests, Semaphore::MAX_PERMITS));
    let semaphore = Arc::new(semaphore);

    let mut tasks = Vec::with_capacity(settings.servers.len());
    for (server, instance) in settings.servers {
        let save_path = if settings.use_server_name_directories {
            save_path.join(server)
        } else {
            save_path.clone()
        };

        let client = Arc::clone(&client);
        let semaphore = Arc::clone(&semaphore);
        let task = tokio::spawn(async move {
            let permit = semaphore.acquire().await?;
            let res = client.get(instance.url).send().await?;
            let body = res.bytes().await?;
            drop(permit);

            let feed = parser::parse(body.as_ref())?;
            let mut tasks = Vec::new();
            for entry in feed.entries {
                let mut builder = EpubBuilder::new(ZipLibrary::new().map_err(|e| anyhow!(e))?)
                    .map_err(|e| anyhow!(e))?;
                builder.set_authors(entry.authors.into_iter().map(|a| a.name).collect());

                let mut filename = Vec::new();
                if let Some(date) = entry.published {
                    filename.push(date.format("%Y-%m-%dT%H-%M-%S").to_string());
                    builder.set_publication_date(date);
                }

                if let Some(title) = entry.title {
                    builder.set_title(&title.content);
                    filename.push(title.content);
                }

                let mut hasher = Sha224::new();
                hasher.update(&entry.id);
                filename.push(format!("{:x}", hasher.finalize()));
                let filename = format!("{}.epub", slugify!(&filename.join("-"), max_length = 250));
                let filename = save_path.join(filename);

                if let Some(content) = entry.content.and_then(|c| c.body) {
                    builder
                        .add_content(EpubContent::new("article.html", content.as_bytes()))
                        .map_err(|e| anyhow!(e))?;
                } else if let Some(content) = entry.summary {
                    builder
                        .add_content(EpubContent::new("article.html", content.content.as_bytes()))
                        .map_err(|e| anyhow!(e))?;
                }

                let file = std::fs::File::create(filename)?;
                builder.generate(file).map_err(|e| anyhow!(e))?;
                let task = tokio::spawn(async move { Ok(()) });
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
