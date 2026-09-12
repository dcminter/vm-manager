# VM Manager

Create and manage QEMU virtual machines from prebuilt OS images, with a command
set modelled on Docker.

```bash
vm images
vm run --name demo debian:trixie
vm ssh demo
vm stop demo
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
| `vm run <image>` | Create and start a machine |
| `vm start <name>` | Start a machine that is not running |
| `vm ssh <name>` | Open a shell on a machine, or run a command in it |
| `vm cp <from> <to>` | Copy files, naming one side as `name:path` |
| `vm ps` | List machines; `--all` includes those not running |
| `vm stop <name>` | Ask the guest to shut down, then insist |
| `vm kill <name>` | Stop a machine without telling the guest |
| `vm rm <name>` | Delete a machine and its disk |

An image is named `repository:tag`, as in `debian:trixie`. The tag may be
omitted, in which case `latest` is used, and a specific build may be pinned by
appending `@sha512:...`.

## Machines

`vm run` fetches the image if it is not already held, gives the machine its own
writable disk over the stored image, and starts it detached. Use `-p
host:guest` to forward a port, `-m` for memory and `--cpus` for processors, and
`--name` to choose a name rather than take the one it invents.

Each machine gets a key pair of its own and a cloud-init seed that installs it
for the account named by `--user`, so no password is set and none is needed.
A host port is forwarded to its SSH port whether or not one was published, so
`vm ssh demo` works on a machine that was started with no arguments at all. It
wraps `ssh` rather than replacing it, so `~/.ssh/config`, the agent and
`ProxyCommand` all still apply, and each machine keeps its own `known_hosts`
so that rebuilding one never touches yours.
Everything a machine owns — its disk, its key, its console log and its record —
lives in one directory under `$XDG_STATE_HOME/vm/instances`, and `vm rm` takes
the lot.

Images that carry no cloud-init still run; they simply take no key and no user,
which `vm run` says at the time rather than leaving to be discovered.

## Configuration

`$XDG_CONFIG_HOME/vm/config.toml` is optional. It holds `catalogue_url`, the
archive `vm update` fetches, and `catalogue_path`, the directory within that
archive holding the entries. Point both at an internal server to run a curated
catalogue. Setting `auto_pull = false` makes `vm run` refuse an image that is
not already held rather than fetching it.

A refreshed catalogue is staged and parsed before it replaces the one in use,
so an unreachable or malformed source leaves the working catalogue untouched.

## Output

Every command writes a table by default and machine-readable output on request:
`--format json` or `--format yaml`, or `VM_FORMAT` in the environment. In those
formats the document is the only thing on standard output, with progress and
errors on standard error, so piping to `jq` needs no filtering. Errors take the
requested format too, carrying a stable `kind` alongside the message.

Fetched images are verified against the digest the catalogue records and are
stored under `$XDG_DATA_HOME/vm/images`, addressed by that digest, so tags
naming the same build share one file.

## Licence

MIT.
