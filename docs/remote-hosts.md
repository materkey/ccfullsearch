# Remote hosts

`ccs` can search other machines over ssh next to this one. List them as `[[hosts]]` in `~/.config/ccs/config.toml` (`$XDG_CONFIG_HOME/ccs/config.toml`, the same path on macOS):

```toml
[[hosts]]
name = "Z270XX"

[[hosts]]
name = "devbox"
ccs_path = "/home/me/.cargo/bin/ccs"
```

`name` is an ssh alias: address, user, key and ControlMaster come from `~/.ssh/config`. Each host needs the same `ccs` version installed; `ccs_path` points at it when it is not on the PATH of a non-interactive ssh shell. `ccs` connects with `ssh -o BatchMode=yes`, so it never asks for a password: key-based login must work without a prompt. Hosts are read at startup, so restart `ccs` after changing the list.

- Search and recent sessions cover every host by default. Results from this machine show at once; a host's results join the list when they arrive, and a host that is down never delays the rest. The status line stays silent about a healthy idle host. It names a host only during a search (`Z270XX: 37`, `Z270XX: …` while waiting) or on a problem (`Z270XX: недоступен (reason)`, `Z270XX: обнови ccs` for an older ccs there).
- Remote rows carry the host name before the project. `Enter` opens the session on its host with `ssh -t <host> ccs resume …`, in the project directory there; the tree view, preview and branch-aware resume work as for local sessions.
- `Ctrl+A` matches a remote project by its path relative to that machine's home (`~/projects/x` on both machines is the same project).
- `Ctrl+L` cycles the host filter and reconnects hosts that are down.
- CLI: `ccs search`, `list` and `pick` take `--host` (repeatable: `all` (default), `local`, `remote`, or a host name); rows carry `host`, and the search summary lists how each host answered. `ccs show --host <host>` reads a remote file. The CLI waits up to 5 s for hosts on `search` and 10 s on `list`.
- `CCS_HOSTS=Z270XX,devbox` overrides the file; `CCS_HOSTS=` turns remote hosts off. When `CCFS_SEARCH_PATH` is set, remote hosts are off unless `CCS_HOSTS` is set too.
