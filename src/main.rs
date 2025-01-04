mod settings;

use std::{
    env,
    fs::{self},
    path::PathBuf,
};

use anyhow::{format_err, Context, Result};
use feed_rs::parser;
use futures::{stream, StreamExt};
use reqwest::Client;
use settings::Settings;

struct Feed {
    server: String,
    feed: feed_rs::model::Feed,
}

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

    // Create directory for each instance name in the save path.
    if settings.use_server_name_directories {
        for name in settings.servers.keys() {
            let instance_path = save_path.join(name);
            if !instance_path.exists() {
                fs::create_dir(&instance_path)?;
            }
        }
    }

    let client = Client::builder()
        .user_agent(format!("plato-feed/{}", env!("CARGO_PKG_VERSION")))
        .build()?;

    stream::iter(settings.servers.into_iter())
        .map(|(server, instance)| {
            let client = &client;
            async move {
                let res = client.get(instance.url).send().await?;
                let body = res.bytes().await?;

                let feed = parser::parse(body.as_ref())?;
                Ok(Feed { server, feed })
            }
        })
        .buffered(settings.concurrent_requests)
        .for_each(|r: Result<Feed>| async {
            match r {
                Err(err) => eprintln!("{}", err),
                Ok(feed) => {
                    for entry in feed.feed.entries {
                        println!(
                            "{}\n{}\n",
                            entry.title.map(|t| t.content).unwrap_or_default(),
                            entry.content.and_then(|c| c.body).unwrap_or_default(),
                        );
                    }
                }
            };
        })
        .await;

    if !save_path.exists() {
        fs::create_dir(&save_path)?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    log_panics::init();
    if let Err(err) = run().await {
        eprintln!("Error: {:#}", err);
        fs::write("feed_error.txt", format!("{:#}", err)).ok();
        return Err(err);
    }

    Ok(())
}
