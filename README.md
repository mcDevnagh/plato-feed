# plato-feed
A fetcher hook for the [Plato](https://github.com/baskerville/plato) document
reader that syncs an e-reader with [RSS](https://en.wikipedia.org/wiki/RSS) and
[Atom](https://en.wikipedia.org/wiki/Atom_(web_standard)) feeds.

## Usage

1. Build a `plato-feed` binary and create a folder in Plato's bin directory for it (usually `/mnt/onboard/.adds/plato/bin/feed`)
2. Edit `Settings.toml` and place it alongside the binary
3. Add a hook to Plato's own `Settings.toml` that looks like the following:
```toml
[[libraries.hooks]]
path = "Feed"
program = "bin/feed/plato-feed"
sort-method = "added"
first-column = "title-and-author"
second-column = "progress"
```
4. Whenever the `Feed` folder is opened, this hook will check if there are any
books that haven't been downloaded and will fetch them if need be.

## Building
The easiest way to build this project is to use
[cross](https://github.com/cross-rs/cross)
to cross-compile with Docker. Once setup it's as simple as running:
```shell
cross build --release --target=arm-unknown-linux-gnueabihf
```

### Building without Docker
After setting up the [Linaro toolchain](https://releases.linaro.org/components/toolchain/binaries/4.9-2017.01/arm-linux-gnueabihf/)
on your system you can compile the project with:
```shell
rustup target add arm-unknown-linux-gnueabihf
cargo build --release --target=arm-unknown-linux-gnueabihf
```

# Acknowledgements
This hook is based on the work done for the following hooks.

[Plato article fetcher](https://github.com/baskerville/plato/blob/master/crates/fetcher/src/main.rs).
License:
```
Plato -- Document reader for the Kobo e-ink devices.
Copyright (C) 2017 Bastien Dejean.

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as
published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this program. If not, see <http://www.gnu.org/licenses/>.
```

[plato-opds](https://github.com/videah/plato-opds). License:
```plato-opds -- OPDS syncing hook for the Plato document reader.
Copyright (C) 2023 videah

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as
published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this program. If not, see <http://www.gnu.org/licenses/>.
```
