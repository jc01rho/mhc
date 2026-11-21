# Remote sessions

`mhc remote` reaches the sessions on your other devices: list them, send them a message, switch
their model, interrupt or close them. It is the port of omo's `omo remote` (omo v5.1.55 to v5.1.79),
without omo's hosted pieces: no OmO account, no hosted relay, no closed device service download.

## How it fits together

```text
mhc remote / thread_send / /remote
        |  local API v1 (private Unix socket + per-boot token, mutual HMAC proof)
        v
device service on this device          (omo's own, or mhc's open mhc-relayd)
        |  peer link (TCP over loopback or a tailnet, Ed25519 mutual proof)
        v
mhc-relayd on the other device  -->  its mhc host (`mhc host ensure`)  -->  the session
```

* The **device service** owns this device's connection to your other devices. mhc speaks the
  same local API v1 that omo's device service serves, so if omo's service already runs here, mhc
  uses it. Otherwise run mhc's open one, **mhc-relayd** (`mhc remote daemon`).
* The **peer link** between two mhc-relayd is plain TCP and is only allowed to loopback and
  tailnet addresses (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`, `*.ts.net`): the tailnet encrypts it,
  the link authenticates both ends with their Ed25519 keys. An ordinary LAN or internet address is
  refused. Protocol: [specs/mhc-relay-v1.md](specs/mhc-relay-v1.md).
* An **account** is the set of devices that list each other's public key, under the same account
  label, in `relayd.json`. There is no hosted sign-in.

## Set up a device

```sh
mhc login            # make this device's key; prints its id and public key
mhc remote bootstrap # relayd.json, and the always-on mhc-relayd + session host units
```

`bootstrap` installs two user units (systemd `--user` on Linux, LaunchAgents on macOS), both
written mode 0644 whatever the umask (omo v5.1.79):

| Unit | Runs | Why |
| --- | --- | --- |
| `mhc-relayd.service` / `ai.mhc.relayd` | `mhc remote daemon` | the device service, kept running |
| `mhc-session-host.service` / `ai.mhc.session-host` | `mhc host ensure` (oneshot) | this device's mhc host is up from login, so other devices can reach its sessions |

On a platform without a user service manager, run `mhc remote daemon` yourself.

Then list the other devices in `~/.omo/device-service/relayd.json` on every device (each device
lists the others, with the key `mhc login` or `mhc whoami` printed there):

```json
{
  "account": "home",
  "listen": "100.101.102.103:7810",
  "peers": [
    { "name": "laptop", "address": "laptop.tail1234.ts.net:7810", "public_key": "<laptop's public key>" }
  ]
}
```

`listen` is this device's tailnet address (no peer link is served without it). Restart the
device service after editing (`systemctl --user restart mhc-relayd`).

## Use it

```sh
mhc remote status                       # the device service's state
mhc remote devices                      # this device first, then the others, online or last seen
mhc remote sessions [--device laptop]   # sessions across devices
mhc remote send laptop/<session> "run the tests"   # --steer into a running turn, --follow-up after it
mhc remote model laptop/<session> openai/gpt-5
mhc remote interrupt laptop/<session>
mhc remote close laptop/<session>       # refused while a turn runs; --force ends it
mhc remote control off                  # other devices may not view or command this one
```

Every verb takes `--json`. Exit codes: 0 ok, 1 failed, 2 usage, 3 the device service is not there
or cannot do it. In the terminal UI, `/remote` walks from a device to a session to an action, and
`/remote <verb> ...` runs a verb. With a device service on this device, the `thread_send` tool
reaches `<device>/<session>` addresses too.

## What is not there yet

* `mhc remote attach` and `mhc remote open`: mhc-relayd carries no attach stream and no delegated
  engine commands yet. `send` returns once the session took the message; it does not stream the
  answer.
* Waking a session that is not running from another device: `wake` reads and stores the switch,
  and mhc-relayd reports it `overridden` (it cannot carry the wake).
* Windows: mhc-relayd serves its local API on a Unix socket only. An mhc client on Windows can
  still use omo's device service there (loopback TCP endpoint).

## Hosted endpoints

mhc assumes no hosted service (docs/divergences.md SD-21). `MAHO_SERVICE_API_URL` (or omo's
`OMO_SERVICE_API_URL`) names an account/intake origin when you want one; it must be HTTPS, or plain
HTTP to a loopback or tailnet host. Without it, `/feedback` saves reports locally and memory sync
sees a signed-out device.
