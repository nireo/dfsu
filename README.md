# dfsu

`dfsu` is a local-first folder sync tool written in Rust. It is only for a local network for now, however as it's built upon `iroh` it's easy to expand to different peer-to-peer networking models. Currently only very simplistic syncing is implemented and transfers for large files are not streamed, and so on...

## Usage

Start by creating a local identity, then serve a folder:

```sh
cargo run -- init ./Sync
cargo run -- serve /tmp/dfsu-a
```

The server prints an invite. In another terminal, pair it and sync from it:

```sh
cargo run -- pair local <invite>
cargo run -- sync /tmp/dfsu-b local
```

You can also skip pairing and use the invite directly:

```sh
cargo run -- sync /tmp/dfsu-b <invite>
```

## Example

```sh
mkdir -p /tmp/dfsu-a /tmp/dfsu-b
printf hello > /tmp/dfsu-a/a.txt
cargo run -- serve /tmp/dfsu-a
```

Then run:

```sh
cargo run -- sync /tmp/dfsu-b <invite>
cat /tmp/dfsu-b/a.txt
```

## Development

Run tests with `cargo test` and format code with `cargo fmt`. To avoid writing to `~/.config/dfsu` during manual testing, set `DFSU_CONFIG_DIR`:

```sh
DFSU_CONFIG_DIR=/tmp/dfsu-config cargo run -- init ./Sync
```
