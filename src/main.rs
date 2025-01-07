mod args;
mod feed;
mod plato;
mod settings;

use std::{
    cmp::min,
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Context, Error, Result};
use args::Args;
use feed::{load_feed, program_name};
use futures::future::join_all;
use reqwest::Client;
use settings::Settings;
use tokio::sync::Semaphore;

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

    let sigterm = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm))?;

    // Create directory for each instance name in the save path.
    if settings.use_server_name_directories {
        for name in settings.servers.keys() {
            let instance_path = args.save_path.join(name);
            let sigterm = Arc::clone(&sigterm);
            if sigterm.load(Ordering::Relaxed) {
                return Err(anyhow!("SIGTERM"));
            }

            if !instance_path.exists() {
                fs::create_dir(&instance_path)?;
            }
        }
    }

    let client = Client::builder().user_agent(program_name()).build()?;
    let client = Arc::new(client);

    let library_path = Arc::new(args.library_path);
    let save_path = Arc::new(args.save_path);

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
        let task = tokio::spawn(load_feed(
            server,
            instance,
            client,
            library_path,
            save_path,
            semaphore,
            sigterm,
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
        eprintln!("Error: {:#}", err);
        fs::write("feed_error.txt", format!("{:#}", err)).ok();
        return Err(err);
    }

    Ok(())
}
