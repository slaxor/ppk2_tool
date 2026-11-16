# ppk2_tool

Host-side tooling for Nordic's Power Profiler Kit II, without nRF Connect.

- `lib/` — driver (CDC protocol, calibration), record folding, recording files, daemon protocol
- `cli/` — `ppk2_tool`: `serve` (daemon), `record` (headless to file), `stream` (stdout)
- `gui/` — `ppk2_tool-gui`: egui client; live via the daemon, or viewing a recording

## Everyday use

    ppk2_tool-gui

Connects to a running daemon or starts `ppk2_tool serve` itself. Controls:
source/ampere, voltage, fold (samples per stored record), power, measure,
clear, save-to-file. Closing the GUI stops a daemon it started unless
"keep daemon on exit" is ticked. Shift+drag selects a range (mean/min/max/charge);
normal drag pans, scroll zooms; `fit all` shows the whole buffer.

The daemon keeps the current measurement in `$XDG_RUNTIME_DIR/ppk2_tool/`
(tmpfs, so RAM; `--spool DIR` for disk). Nothing is written elsewhere until
you press save.

## Long runs on a small machine

    ppk2_tool record --fold 1000 --seconds 86400 night.ppk2    # on the Pi, in tmux
    ppk2_tool-gui /mnt/pi/night.ppk2                           # on the laptop, any time

or run `ppk2_tool serve` there and `ssh -L $XDG_RUNTIME_DIR/ppk2_tool.sock:/run/user/1000/ppk2_tool.sock pi`
to use the GUI against the remote daemon.

Sizes: 16 bytes per record. fold 10 → 1.4 GB/day, fold 100 → 138 MB/day,
fold 1000 → 14 MB/day. Min/max are kept per record, so a short burst stays
visible at any fold.

## Files and protocol

- [docs/format.md](docs/format.md) — the `.ppk2` recording format
- [docs/protocol.md](docs/protocol.md) — talking to the daemon

## Dev

    nix develop     # or direnv
    just test
    just lint
    cargo run -p ppk2_tool-gui

The GUI needs the host's GPU drivers; the devShell therefore tracks the same
nixpkgs as the system (glibc must match for Mesa's `dlopen` to work).

Your user must be in `dialout` (NixOS: `users.users.<you>.extraGroups = [ "dialout" ];`).

## License

GPLv3, see [LICENSE](./LICENSE).
