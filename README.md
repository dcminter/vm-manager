# VM Manager

Create and manage QEMU virtual machines from prebuilt OS images, with a command
set modelled on Docker.

```bash
vm images
vm pull debian:trixie
vm inspect debian:trixie --output json
```

## Building and running

Build prerequisites (Debian 13):

```bash
sudo apt install build-essential
```

Then:

```bash
cargo build
cargo run -p vm -- images
```

## Commands

| Command | Purpose |
|---|---|
| `vm images` | List the images in the local catalogue |
| `vm inspect <image>` | Show where an image comes from and how its guest is reached |
| `vm pull <image>` | Fetch an image into the local store |
| `vm update` | Refresh the local catalogue from its remote source |

An image is named `repository:tag`, as in `debian:trixie`. The tag may be
omitted, in which case `latest` is used, and a specific build may be pinned by
appending `@sha512:...`.

## Configuration

`$XDG_CONFIG_HOME/vm/config.toml` is optional. It holds `catalogue_url`, the
archive `vm update` fetches, and `catalogue_path`, the directory within that
archive holding the entries. Point both at an internal server to run a curated
catalogue.

A refreshed catalogue is staged and parsed before it replaces the one in use,
so an unreachable or malformed source leaves the working catalogue untouched.

## Output

Every command writes a table by default and machine-readable output on request:
`--output json` or `--output yaml`, or `VM_OUTPUT` in the environment. In those
formats the document is the only thing on standard output, with progress and
errors on standard error, so piping to `jq` needs no filtering. Errors take the
requested format too, carrying a stable `kind` alongside the message.

Fetched images are verified against the digest the catalogue records and are
stored under `$XDG_DATA_HOME/vm/images`, addressed by that digest, so tags
naming the same build share one file.

## Licence

MIT.
