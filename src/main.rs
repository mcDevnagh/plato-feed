mod args;
mod client;
mod feed;
mod html;
mod plato;
mod settings;

use std::{fs, sync::Arc};

use anyhow::{Context, Result};
use args::Args;
use client::Client;
use feed::{load_feed, program_name};
use futures::future::join_all;
use plato::notify;
use settings::Settings;

async fn run(args: Args, settings: Settings) -> Result<()> {
    if !args.online {
        if !args.wifi {
            plato::notify("Establishing a network connection.");
            plato::set_wifi(true);
        } else {
            plato::notify("Waiting for the network to come up.");
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
    }

    if !args.save_path.exists() {
        fs::create_dir(&args.save_path)?;
    }

    // Create directory for each instance name in the save path.
    if settings.use_server_name_directories {
        for name in settings.servers.keys() {
            let instance_path = args.save_path.join(name);
            if !instance_path.exists() {
                fs::create_dir(&instance_path)
                    .with_context(|| format!("creating server directory: {}", name))?;
            }
        }
    }

    let client = Client::new(program_name(), settings.concurrent_requests)?;
    let library_path = Arc::new(args.library_path);
    let save_path = Arc::new(args.save_path);

    let mut tasks = Vec::with_capacity(settings.servers.len());
    for (server, instance) in settings.servers {
        let instance = Arc::new(instance);
        let library_path = Arc::clone(&library_path);
        let save_path = if settings.use_server_name_directories {
            Arc::new(save_path.join(&server))
        } else {
            Arc::clone(&save_path)
        };
        let server = Arc::new(server);
        let task = tokio::spawn(load_feed(
            server,
            instance,
            client.clone(),
            library_path,
            save_path,
        ));
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
    let args = Args::new()?;
    let settings = Settings::load().with_context(|| "failed to load settings")?;
    if let Err(err) = run(args, settings).await {
        notify(&format!("Error: {}", err));
        return Err(err).with_context(|| "feed");
    }

    Ok(())
}
