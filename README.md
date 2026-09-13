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
| `vm images` | List the images in the local catalogue |
| `vm inspect <image>` | Show where an image comes from and how its guest is reached |
| `vm pull <image>` | Fetch an image into the local store |
| `vm update` | Refresh the local catalogue from its remote source |
| `vm run <image>` | Create and start a machine |
| `vm start <name>` | Start a stopped machine, optionally changing its settings |
| `vm ssh <name>` | Open a shell on a machine, or run a command in it |
| `vm cp <from> <to>` | Copy files, naming one side as `name:path` |
| `vm ps` | List machines; `--all` includes stopped ones, `--follow` refreshes |
| `vm stop <name>` | Ask the guest to shut down, then force it |
| `vm pause <name>` | Stop a machine's processors |
| `vm resume <name>` | Resume a paused machine |
| `vm kill <name>` | Stop a machine without telling the guest |
| `vm logs <name>` | Show a machine's console output; `--follow` streams it |
| `vm console <name>` | Attach to a machine's serial console; Ctrl-] detaches |
| `vm screen <name>` | Open a VNC viewer on a machine's screen |
| `vm screenshot <name> [file]` | Save a machine's screen as a PNG |
| `vm clone <name> <image>` | Save a machine's disk as a new image |
| `vm rm <name>` | Delete a machine and its disk |
| `vm rmi <image>` | Delete an image from the local store |

An image is named `repository:tag`, as in `debian:trixie`. The tag defaults to
`latest`, and a build may be pinned by appending `@sha512:...`.

`man vm` describes every command and option.

## Images

The catalogue ships with `debian`, `ubuntu`, `fedora`, `centos`, `almalinux`,
`rocky`, `opensuse`, `alpine`, `arch`, `omnios`, `freebsd`, `netbsd`, `9front`
and `puredarwin`, several releases apiece. Each entry names a dated build and
its publisher's checksum, which `vm pull` verifies.

An entry may declare that its image is compressed (`compression`), its
`format`, and hardware it needs: `firmware`, `cpu`, `machine` and `disk`.

## Machines

`vm run` fetches the image if needed and starts a machine with its own
writable disk. `-p host:guest` forwards a port, `-v /host/path:/guest/path`
shares a directory, `-m` sets memory, `--cpus` processors, and `--name` the
name.

Each machine gets its own key pair and an account named by `--user`, so `vm
ssh` needs no password. `vm ssh` wraps `ssh`, so `~/.ssh/config` and the agent
still apply. `--password` sets a password for console logins.

`--add-ssh-config` lets plain `ssh`, `scp` and `rsync` reach the machine by
name:

```bash
vm run debian:trixie --name mytestvm --add-ssh-config
ssh mytestvm
```

This adds one `Include` line to the top of `~/.ssh/config`. A name already used
by a `Host` there is refused. `vm rm` or `vm start --no-ssh-config` removes the
entry.

`vm start` accepts the same settings as `vm run` and applies them from then on.
`--no-publish`, `--no-volume` and `--no-password` clear them.

Images without cloud-init still run, but take no key and no account.

Machine state lives under `$XDG_STATE_HOME/vm/instances`.

### Ping

`ping` from a guest gets no replies unless the host allows unprivileged pings
for one of the user's groups. Check with
`cat /proc/sys/net/ipv4/ping_group_range`; if it reads `1 0`, run:

```bash
echo 'net.ipv4.ping_group_range = 0 2147483647' | sudo tee /etc/sysctl.d/60-ping.conf
sudo sysctl --system
```

Only echo requests pass, so `traceroute` and `mtr` see nothing past the host.

## Shell completion

The package installs completion for bash, zsh and fish, covering commands,
flags, machine names, image references and machine settings.

Without the package, with `vm` on the `PATH`, use one of:

```bash
source <(COMPLETE=bash vm)
source <(COMPLETE=zsh vm)
COMPLETE=fish vm | source
```

## Configuration

`$XDG_CONFIG_HOME/vm/config.toml` is optional. `man vm-config` describes its
settings: the catalogue source, whether `vm run` fetches missing images, and
whether machines get an SSH config entry by default.

## Output

Commands write text by default, or `--format json` / `--format yaml` (or
`VM_FORMAT`). In those formats standard output holds only the document; errors
carry a stable `kind`.

## Releases

`scripts/release` runs the checks, prompts for the version and release notes,
and pushes the tag. The tag triggers a workflow that attaches Debian Trixie and
Ubuntu 24.04 packages to the release.

`scripts/package <trixie|noble> <version>` builds a package locally.

## Licence

MIT.
