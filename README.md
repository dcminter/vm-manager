# VM Manager

Create and manage QEMU virtual machines from prebuilt OS images, with a command
set modelled on Docker and various cloud clients.

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
| `vm start <name>` | Start a machine that is not running, and change how it is set up |
| `vm ssh <name>` | Open a shell on a machine, or run a command in it |
| `vm cp <from> <to>` | Copy files, naming one side as `name:path` |
| `vm ps` | List machines; `--all` includes those not running |
| `vm stop <name>` | Ask the guest to shut down, then insist |
| `vm kill <name>` | Stop a machine without telling the guest |
| `vm logs <name>` | Show a machine's console; `--follow` writes it as it arrives |
| `vm commit <name> <image>` | Save a machine's disk as a new image |
| `vm rm <name>` | Delete a machine and its disk |
| `vm rmi <image>` | Delete an image from the local store |

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

`-v /host/path:/guest/path` shares a directory over virtiofs. Each share is
served by a `virtiofsd` of its own, started before the machine and reaped with
it. A share is mounted at every boot from the machine's seed rather than
written into the guest's `/etc/fstab`, because a share belongs to the run: an
entry left in the guest would outlive it and fail the next boot that went
without it.

`vm start` takes the same `--memory`, `--cpus`, `--publish`, `--volume`,
`--user` and `--disk-size` as `vm run`, and each changes the machine from then
on; what is not given is left as it was. Forwards and shares are replaced as a
list, and `--no-publish` or `--no-volume` leaves the machine with none. A new
`--user` is an account cloud-init has to create, so the guest is told it is a
machine it has not seen before and generates fresh host keys; the account it
already had is left where it is. None of this touches a running machine, which
is stopped first.

`vm logs` shows what the guest wrote to its console, appended across every boot
since the machine was created, so a machine that failed to boot and was started
again keeps the evidence. `--follow` writes it as it arrives, which is text
only: a document cannot be emitted a line at a time and still be a document.

`vm commit` flattens a machine's disk into a standalone image in the store and
gives it a name. A running guest is paused for the duration, because a disk
taken from under one is crash-consistent at best; `--force` skips the pause and
says so. Committed images are listed by `vm images` alongside the catalogue's
own, and `vm update` does not disturb them.

`vm rmi` takes an image back out of the store. One the catalogue provides is
listed again as unfetched; one made here has nowhere to be fetched from, so its
entry goes with it. A machine is built on its image rather than a copy of it,
so an image still in use is refused unless `--force` says otherwise.

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

## Releases

`scripts/release` walks through cutting one: it runs the checks, asks for the
version and the release notes, asks whether the things a workflow cannot test
have been tested by hand, and pushes the tag. The tag starts a workflow that
builds a package for Debian Trixie and one for Ubuntu 24.04 and attaches both
to the release.

`scripts/package <trixie|noble> <version>` builds one locally. It checks that
every dependency it claims exists in the distribution it is building for, so a
package `apt` would refuse fails the build rather than a user's install.

## Licence

MIT.
