# mhc relay v1: the peer link between two mhc-relayd

mhc-relayd stands in for the closed omo-relay-daemon. Toward the processes on its own device it
serves omo's local API v1 unchanged (see `crates/omo/device-service-core/src/local_api/`). Toward
the account's other devices it keeps this peer link.

## Transport

* LF-delimited UTF-8 JSON, one object per line, at most 1,048,576 bytes per line (the local API
  framing). One connection per request: the dialer opens it, proves itself, sends one request,
  reads one answer and closes.
* Plain TCP, allowed only toward a loopback or tailnet host (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`,
  `*.ts.net`); the tailnet provides confidentiality. A listener on any other address is refused at
  start, and a peer at any other address is never dialed.

## Identity and admission

* Each device holds an Ed25519 key (`<state dir>/device.key`, mode 0600). Its device id is `dev_`
  and the first 20 base64url characters of the SHA-256 of the public key.
* An account is a label (`relayd.json` `account`) plus the peers each device lists by public key.
  A side admits only a key its own `relayd.json` lists, under the same account label.

## Handshake

Both sides send a fresh 32-byte nonce (base64url) on every connection.

```text
dialer   -> {"type":"peer.hello","v":1,"account":A,"device":{id,name,platform},"public_key":Kd,"nonce":Nd}
listener -> {"type":"peer.welcome","v":1,"device":{...},"public_key":Kl,"nonce":Nl,"signature":Sl}
            | {"type":"peer.refused","reason":R}
dialer   -> {"type":"peer.prove","signature":Sd}
listener -> {"type":"peer.ready"} | {"type":"peer.refused","reason":"bad_signature"}
```

* `Sl` is the listener's signature over the lines `mhc-relay-v1/server`, `A`, `Nd`, `Nl`, `Kd`, `Kl`
  (joined with `\n`). The dialer checks that `Kl` is the key it lists for that address.
* `Sd` is the dialer's signature over `mhc-relay-v1/client`, `A`, `Nl`, `Nd`, `Kl`, `Kd`.
* Both nonces are in both transcripts, so a recorded handshake never opens another connection.
  Signatures are checked with `verify_strict`.
* Refusal reasons: `malformed`, `version`, `service_disabled` (the listener is off or signed out),
  `account_mismatch`, `unknown_device` (key not listed), `bad_signature`.

## Requests

`{"id","type":"peer.request","op":...}` answered by `{"id","type":"peer.response","ok":true,"data":{...}}`
or `{"id","type":"peer.response","ok":false,"error":{"code","reason"}}`.

| op | fields | data |
| --- | --- | --- |
| `describe` | | `{device: {id, name, platform}, status, control_attached}` |
| `sessions` | | `{sessions: [{session_id, durable_id?, title?, project?, path?, state, updated_ms?, kind?, model?}]}`; refused `forbidden` while remote control is off |
| `command` | `session_id`, `command_id` (22 base64url), `rpc` (one engine record) | the terminal command state: `{state: engine_applied, receipt}`, `{state: engine_refused, reason}` (`peer_unauthorized` while remote control is off, `route_unlisted` when the host no longer lists the session) or `{state: uncertain, reason}` |

A `command` is deduplicated per sending device and `command_id`: the same id with the same target
and payload answers the first outcome and runs nothing; another payload is `command_id_conflict`.
The engine records carried are `prompt`, `steer`, `follow_up`, `abort`, `set_model`, `get_state` and
`close_session`; the local API refuses any other method before anything leaves the device.
