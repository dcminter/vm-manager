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

You also need Rust (2024 edition) from [Rustup.sh](https://rustup.rs/) or elsewhere.

Then:

```bash
cargo build
cargo run -p vm -- images
```

## Commands

| Command | Purpose |
|---|---|
| `vm images` | List the images in the local catalogue and what each takes |
| `vm inspect <image>` | Show where an image comes from and how its guest is reached |
| `vm pull <image>` | Fetch an image into the local store |
| `vm update` | Refresh the local catalogue from its remote source |
| `vm run <image>` | Create and start a machine |
| `vm start <name>` | Start a machine that is not running, and change how it is set up |
| `vm ssh <name>` | Open a shell on a machine, or run a command in it |
| `vm cp <from> <to>` | Copy files, naming one side as `name:path` |
| `vm ps` | List machines; `--all` includes those not running, `--follow` keeps looking |
| `vm stop <name>` | Ask the guest to shut down, then insist |
| `vm pause <name>` | Stop a machine's processors without telling the guest |
| `vm resume <name>` | Let a paused machine carry on |
| `vm kill <name>` | Stop a machine without telling the guest |
| `vm logs <name>` | Show a machine's console; `--follow` writes it as it arrives |
| `vm console <name>` | Attach the terminal to a machine's serial console; Ctrl-] detaches |
| `vm screen <name>` | Open a VNC viewer on a machine's screen |
| `vm screenshot <name> [file]` | Save a machine's screen as a PNG |
| `vm clone <name> <image>` | Save a machine's disk as a new image |
| `vm rm <name>` | Delete a machine and its disk |
| `vm rmi <image>` | Delete an image from the local store |

An image is named `repository:tag`, as in `debian:trixie`. The tag may be
omitted, in which case `latest` is used, and a specific build may be pinned by
appending `@sha512:...`.

## Images

The catalogue ships with `debian`, `ubuntu`, `fedora`, `centos`, `almalinux`,
`rocky`, `opensuse`, `alpine`, `arch`, `omnios`, `freebsd`, `netbsd`, `9front`
and `puredarwin`, several releases apiece. Run `vm images` for the list. Every entry
names a dated build rather than a moving `latest`, and carries the checksum its
publisher issued, which is what `vm pull` verifies what it fetched against.

An entry may be published compressed, which several projects do because the
saving is large: an entry that says `compression = "xz"`, `"gzip"` or `"zstd"`
is expanded on the way into the store, once, rather than on every boot. The
digest is still the publisher's own, so it covers the file as published rather
than what it expands to. An entry also states the `format` of the image itself,
`qcow2` or `raw`, which is what the instance's overlay records as its backing
format.

An entry that needs other hardware says so: `firmware = "uefi"` in place of
BIOS, `cpu` for a QEMU CPU model in place of `max`, `machine = "pc"` in place of
`q35`, and `disk = "ide"` or `"sata"` in place of `virtio` for a guest with no
virtio driver. PureDarwin needs a CPU model, the `pc` machine and an IDE disk,
and its shell is on the screen rather than the serial console.
Desktop spins stay out, because they publish installer ISOs and no bootable
disk.

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

`--firmware`, `--cpu`, `--machine` and `--disk` override what the image asks
for; an IDE disk needs the `pc` machine and a SATA disk needs `q35`. `--password` asks
for a password the account can log in with at the console; it is read from the
terminal, or as one line of standard input, and never taken as an argument. SSH
still takes only the machine's key.

`vm start` takes the same `--memory`, `--cpus`, `--publish`, `--volume`,
`--user`, `--disk-size`, `--firmware`, `--cpu`, `--machine`, `--disk` and
`--password` as `vm run`,
and each changes the machine from then on; what is not given is left as it was.
Forwards and shares are replaced as a list, `--no-publish` or `--no-volume`
leaves the machine with none, and `--no-password` takes the password away. A
disk prepared for only one firmware does not boot under the other; nothing on it
is changed by trying, and changing the firmware back restores the machine.
FreeBSD reads a new account or password only on a machine's first boot, so
there they belong on `vm run`. A new
`--user` is an account cloud-init has to create, so the guest is told it is a
machine it has not seen before and generates fresh host keys; the account it
already had is left where it is. None of this touches a running machine, which
is stopped first.

`vm logs` shows what the guest wrote to its console, appended across every boot
since the machine was created, so a machine that failed to boot and was started
again keeps the evidence. `--follow` writes it as it arrives, which is text
only: a document cannot be emitted a line at a time and still be a document.

`vm console` attaches the terminal to the machine's serial console, for images
that take no key and for logging in with `--password`. Ctrl-] detaches and
leaves the machine running. Every machine also has a screen, served over VNC on
a private socket: `vm screen` opens `xtigervncviewer` on it, or, where there is
no display, prints the socket and the `ssh -L` command that forwards it to a
machine with one. `vm screenshot` saves the screen as a PNG.

`vm ps` gives each machine's memory and disk as what it costs the host against
what it was promised: the memory the hypervisor holds now out of what the
machine was started with, and the bytes its disk occupies out of the size the
guest sees. A machine that is not running holds no memory, so only the figure
it was given is shown. `--follow` lists them again every second; on a terminal
each listing replaces the last, and in a document format one follows another.

`vm clone` flattens a machine's disk into a standalone image in the store and
gives it a name. It runs in two passes and shows both: the disk is flattened,
and the result is read back and verified, because an image is addressed by its
digest and the digest cannot be known before the bytes exist. A running guest
is paused for the duration, because a disk taken from under one is
crash-consistent at best; `--force` skips the pause and says so. Cloned
images are listed by `vm images` alongside the catalogue's own, and `vm update`
does not disturb them.

`vm pause` stops a machine's processors. The guest takes no part: its memory,
its disk and everything it holds open stay as they are, held by a hypervisor
that is still running, and `vm resume` continues from the instruction it
stopped at. This is not suspending — nothing is written anywhere, so it
survives neither a host reboot nor `vm kill`. A paused guest answers nothing,
so `vm ssh` and `vm cp` say so rather than wait, and `vm stop` lets it carry on
first so that it can take the power button.

`vm rmi` takes a name back out of the store. One the catalogue provides is
listed again as unfetched; one made here has nowhere to be fetched from, so its
entry goes with it. Images are held by digest, so two clones of an unchanged
disk are one file under two names: the file goes with the last name for it, and
until then `vm rmi` says which names are keeping it. A machine is built on its
image rather than a copy of it, so an image still in use is refused unless
`--force` says otherwise.

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
naming the same build share one file. A compressed entry is verified as it
arrives and expanded through `xz`, `gzip` or `zstd`, so the digest checked is
the published one and the file kept is the usable one.

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
