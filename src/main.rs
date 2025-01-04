mod settings;

use std::{
    env,
    fs::{self},
    path::PathBuf,
};

use anyhow::{format_err, Context, Result};
use settings::Settings;

fn run() -> Result<()> {
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
    println!(
        "Use Server Name Directories: {}",
        settings.use_server_name_directories
    );
    for (server, instance) in settings.servers {
        println!("{}: {{url={}}}", server, instance.url);
    }

    if !save_path.exists() {
        fs::create_dir(&save_path)?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    log_panics::init();
    if let Err(err) = run() {
        eprintln!("Error: {:#}", err);
        fs::write("feed_error.txt", format!("{:#}", err)).ok();
        return Err(err);
    }

    Ok(())
}
