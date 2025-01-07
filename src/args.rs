use std::{env, path::PathBuf};

use anyhow::{anyhow, Result};

pub struct Args {
    pub library_path: PathBuf,
    pub save_path: PathBuf,
    pub wifi: bool,
    pub online: bool,
}

impl Args {
    pub fn new() -> Result<Args> {
        let mut args = env::args().skip(1);
        let library_path = PathBuf::from(
            args.next()
                .ok_or_else(|| anyhow!("missing argument: library path"))?,
        );

        let save_path = PathBuf::from(
            args.next()
                .ok_or_else(|| anyhow!("missing argument: save path"))?,
        );

        let wifi = args
            .next()
            .ok_or_else(|| anyhow!("missing argument: wifi status"))
            .and_then(|v| v.parse::<bool>().map_err(Into::into))?;

        let online = args
            .next()
            .ok_or_else(|| anyhow!("missing argument: online status"))
            .and_then(|v| v.parse::<bool>().map_err(Into::into))?;

        Ok(Args {
            library_path,
            save_path,
            wifi,
            online,
        })
    }
}
