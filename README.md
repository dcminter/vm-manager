# VM Manager

Create and manage QEMU virtual machines from prebuilt OS images, with a command
set modelled on Docker.

```bash
vm images
vm pull debian:trixie
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

An image is named `repository:tag`, as in `debian:trixie`. The tag may be
omitted, in which case `latest` is used, and a specific build may be pinned by
appending `@sha512:...`.

Fetched images are verified against the digest the catalogue records and are
stored under `$XDG_DATA_HOME/vm/images`, addressed by that digest, so tags
naming the same build share one file.

## Licence

MIT.
