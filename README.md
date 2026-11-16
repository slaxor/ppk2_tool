[![ci](https://github.com/slaxor/ppk2_tool/actions/workflows/ci.yml/badge.svg)](https://github.com/slaxor/ppk2_tool/actions/workflows/ci.yml)
# ppk2_tool

Host-side tooling for Nordic's Power Profiler Kit II, without nRF Connect.

- `lib/` — driver (CDC protocol, calibration), record folding, recording files, daemon protocol
- `cli/` — `ppk2_tool`: `serve` (daemon), `record` (headless to file), `stream` (stdout),
  `probe` (how a few seconds of samples spread over the five ranges)
- `gui/` — `ppk2_tool-gui`: egui client; live via the daemon, or viewing a recording

## Install

Binaries for every push to master are on the
[`latest` pre-release](https://github.com/slaxor/ppk2_tool/releases/tag/latest)
(x86_64/aarch64 Linux, Apple Silicon/Intel macOS); tagged `v*` pushes become
proper releases. Windows is out: the daemon talks over unix sockets.

NixOS / home-manager, as a flake input:

```nix
inputs.ppk2_tool.url = "github:slaxor/ppk2_tool";

# home-manager
imports = [ inputs.ppk2_tool.homeManagerModules.default ];
programs.ppk2-tool.enable = true;
programs.ppk2-tool.settings = { fold = 10; millivolts = 3300; };   # optional, becomes defaults.toml

# NixOS: dialout access and no USB autosuspend for the PPK2
imports = [ inputs.ppk2_tool.nixosModules.default ];
hardware.ppk2.enable = true;
users.users.<you>.extraGroups = [ "dialout" ];
```

Or just `nix run github:slaxor/ppk2_tool` (GUI) and
`nix run github:slaxor/ppk2_tool#cli -- record --fold 1000 night.ppk2`.
Builds are pushed to the `slaxor` cachix cache by CI.

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

## Calibrating the source voltage

The PPK2's regulator can be a percent or so off (3300 mV set, 3.267 V measured
on one unit). With a multimeter on VOUT:

    ppk2_tool calibrate-voltage            # points at 1800 and 3300 mV
    ppk2_tool calibrate-voltage --high 5000

It asks for the reading at each point (`3.267`, `3,267` or `3267`), fits
output = gain × command + offset and stores it per unit in
`$XDG_CONFIG_HOME/ppk2_tool/calibration/<serial>.toml`. From then on the
voltage you set is the voltage on VOUT. The daemon reads it at start, so
restart the GUI/daemon after calibrating.

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
