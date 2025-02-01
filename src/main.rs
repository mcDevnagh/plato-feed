mod args;
mod client;
mod db;
mod feed;
mod html;
mod plato;
mod settings;

use std::{fs, sync::Arc};

use anyhow::{Context, Result};
use args::Args;
use client::Client;
use db::Db;
use feed::{load_feed, program_name};
use futures::future::join_all;
use plato::notify;
use settings::Settings;

async fn run() -> Result<()> {
    let args = Args::new()?;
    let settings = Settings::load().with_context(|| "failed to load settings")?;
    if !args.online {
        if !args.wifi {
            plato::notify("Please enable WiFi to update feeds");
        } else {
            plato::notify("Waiting for the network to come up");
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
    }

    if !args.save_path.exists() {
        fs::create_dir(&args.save_path)?;
    }

    let db = Arc::new(Db::new()?);
    let client = Client::new(program_name(), settings.concurrent_requests)?;
    let library_path = Arc::new(args.library_path);

    let mut tasks = Vec::with_capacity(settings.servers.len());
    for server in settings.flatten_servers(args.save_path) {
        if !server.dir.exists() {
            let res = fs::create_dir_all(&server.dir)
                .with_context(|| format!("creating server directory: {}", server.dir.display()));
            if let Err(err) = res {
                notify(&err.to_string());
                eprintln!("feed: {:?}", err);
                continue;
            }
        }

        let db = Arc::clone(&db);
        let instance = Arc::new(server.instance);
        let client = client.clone();
        let library_path = Arc::clone(&library_path);
        let save_dir = Arc::new(server.dir);
        let server = Arc::new(server.server);
        let task = tokio::spawn(async move {
            load_feed(
                db,
                Arc::clone(&server),
                instance,
                client,
                library_path,
                save_dir,
            )
            .await
            .with_context(|| format!("Server {}", server))
        });
        tasks.push(task);
    }

    let mut errors = 0;
    for result in join_all(tasks).await {
        let err = match result {
            Err(e) => e.into(),
            Ok(Err(e)) => e,
            Ok(Ok(tasks)) => {
                for result in join_all(tasks).await {
                    let err = match result {
                        Err(e) => e.into(),
                        Ok(Err(e)) => e,
                        Ok(Ok(_)) => continue,
                    };

                    eprintln!("feed: {:?}", err);
                    errors += 1;
                }

                continue;
            }
        };

        eprintln!("feed: {:?}", err);
        errors += 1;
    }

    if errors > 0 {
        notify(&format!("Feed downloaded with {errors} errors"));
    } else {
        notify("Feed download successful");
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    log_panics::init();
    if let Err(err) = run().await {
        notify(&err.to_string());
        eprintln!("feed: {:?}", err);
    }
}
