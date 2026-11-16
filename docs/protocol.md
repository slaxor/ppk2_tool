# Daemon protocol

`ppk2_tool serve` listens on a unix socket, by default
`$XDG_RUNTIME_DIR/ppk2_tool.sock`. Permissions on that path are the only
access control; for a remote daemon, forward the socket with `ssh -L`.

## Framing

Every message in either direction is one frame:

    kind: u8, len: u32 little endian, payload[len]

| kind | payload                                               |
|------|-------------------------------------------------------|
| 0    | one JSON object (UTF-8)                               |
| 1    | `len / 16` records, packed as in [format.md](format.md) |

## Control connection

Clients send kind-0 requests and get one kind-0 reply each, in order.

| request                                            | reply                       |
|----------------------------------------------------|-----------------------------|
| `{"cmd":"status"}`                                 | `{"reply":"status", ...}`   |
| `{"cmd":"set","mode":"source"|"ampere","millivolts":N,"fold":N}` (each key optional) | `ok` |
| `{"cmd":"power","on":true}`                        | `ok`                        |
| `{"cmd":"start"}` / `{"cmd":"stop"}`               | `ok`                        |
| `{"cmd":"clear"}`                                  | `ok` — buffer starts over   |
| `{"cmd":"save","path":"/abs/or/rel.ppk2"}`         | `ok` — copies the buffer    |
| `{"cmd":"quit"}`                                   | `ok` — daemon exits         |

Replies are `{"reply":"ok"}`, `{"reply":"error","message":"..."}` or

```json
{"reply":"status","port":"/dev/...","hw":24455,"mode":"source",
 "millivolts":3300,"fold":10,"power":false,"measuring":false,
 "records":0,"dropped":0,"started_unix_ms":1763240000000}
```

`set` persists `mode`, `millivolts` and `fold` to
`$XDG_CONFIG_HOME/ppk2_tool/settings.toml`. Changing `fold` clears the buffer.

## Feed connection

A connection that sends `{"cmd":"subscribe","from":N}` turns into a one-way
stream: the daemon sends kind-1 record frames starting at record `N`, then
everything new as it arrives, until the client closes. If the buffer is
cleared, the daemon sends `{"reply":"cleared"}` on the feed and continues from
record 0 of the new buffer. The GUI uses one control and one feed connection.

Poking it by hand:

    printf '\x00\x10\x00\x00\x00{"cmd":"status"}' | socat - UNIX:$XDG_RUNTIME_DIR/ppk2_tool.sock | tail -c +6
