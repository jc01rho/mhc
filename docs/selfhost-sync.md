# Self-hosted memory sync (`mhc-sync-server`)

`mhc` syncs memory between your devices through a small HTTP service. The hosted omo service is
closed, so this repository ships an open server you can run yourself: `crates/maho-sync-server`
(MIT), binary `mhc-sync-server`. It answers every route the shipped sync client
(`memory_core::sync`) calls, and the end-to-end tests in `crates/maho-sync-server/tests/e2e.rs`
drive it with that real client.

## What the server sees

The server is a dumb ciphertext log. Batches are sealed and signed on the device before upload;
keyring records are signed on the device and the wraps inside them are encrypted to each member.
The server stores those bytes as they arrive, puts them in one sequence per account, and hands
them back. It holds no key and cannot read your memory. What it does decide:

- which account and device a request belongs to (from the bearer token, never from the body);
- the order of entries: an append must name the current head as `base_seq`, otherwise it is
  refused `stale_base`, and a repost of an already landed batch id answers its original seq;
- the object cap (8 MiB) and that uploaded bytes match the digest they were reserved with;
- revocation: a revoked device is refused, and its account's batch appends answer `stale_base`
  until a device appends a keyring record (the rotation), which clears the hold.

## Running it

```bash
cargo build --release -p maho-sync-server
./target/release/mhc-sync-server --listen 127.0.0.1:8787 --db sync.db --tokens tokens.json
```

- `--listen` defaults to `127.0.0.1:8787`. The server never binds `0.0.0.0` unless you pass it.
- `--db` is a SQLite file (WAL mode). Back it up like any database; losing it loses the log, not
  your memory, which stays in each device's git repository.
- `--tokens` is read once at start; restart the server after editing it.

## The tokens file

```json
{
  "tokens": [
    {
      "token": "<long random string>",
      "account": "me",
      "device": "<device id>",
      "principal": "device:<device id>",
      "sealing_public_key": "<device sealing key, base64url>",
      "signing_public_key": "<device signing key, base64url>"
    },
    { "token": "<another long random string>", "account": "me", "device": "laptop" },
    { "token": "<admin token>", "account": "admin", "device": "admin", "admin": true }
  ]
}
```

- One entry per device. Devices that share an account share one log; different accounts never
  see each other's entries or objects, and an unknown token is answered 401.
- `principal` is how the device list names the device (default `device:<device>`); the client's
  rotation-on-revoke path compares it with the chain's members.
- Generate tokens with something like `openssl rand -base64 32`. Anyone holding a token can
  append ciphertext to that account and read its ciphertext back, so treat it as a password.

## Revoking a device

```bash
curl -X POST -H "Authorization: Bearer <admin token>" \
  http://127.0.0.1:8787/v1/admin/devices/<device id>/revoke
```

The device's row in `GET /v1/devices` gets a `revokedAt` timestamp, every other request from it
is refused `device_revoked`, and the account is marked `rotation_pending`. The remaining devices
see the revocation, rotate the epoch key with a keyring append, and syncing resumes. Remove the
revoked entry from the tokens file at your next restart.

## Pointing mhc at it

`mhc` reads the service origin from `OMO_SERVICE_API_URL`
(`crates/omo/components/maho-omo-memory/src/device_session/account_source.rs`). It accepts an
`https://` origin, or plain `http://127.0.0.1:<port>` for a server on the same machine:

```bash
export OMO_SERVICE_API_URL=http://127.0.0.1:8787
```

Any other `http://` origin is refused, on purpose: tokens travel in the `Authorization` header.

## Reaching it from other machines

- **Tailscale**: keep `--listen 127.0.0.1:8787` and publish it with `tailscale serve --https=443
  http://127.0.0.1:8787`; devices use `OMO_SERVICE_API_URL=https://<machine>.<tailnet>.ts.net`.
  Only your tailnet reaches it and the certificate is managed for you.
- **Reverse proxy**: terminate TLS in Caddy or nginx in front of `127.0.0.1:8787`. The event
  stream (`GET /v1/me/events`) is server-sent events, so disable response buffering for it.
- Do not expose the server over plain HTTP on a public interface.

## Routes

| Route | Purpose |
|---|---|
| `GET /v1/sync/state` | head (`seq`, `link`, `keyring_link`), `current_epoch`, `rotation_pending` |
| `GET /v1/sync/batches?after=n` | up to 500 rows after `n` (seq, kind, author, digest, object, batch id) |
| `GET /v1/sync/objects/{id}` | object bytes, this account only |
| `POST /v1/sync/uploads`, `PUT /v1/sync/uploads/{id}` | reserve, then upload digest-checked bytes |
| `POST /v1/sync/batches` | append at `base_seq + 1`; idempotent per batch id |
| `GET /v1/sync/entries?after=n` | full rows for the account chain |
| `GET`/`PUT /v1/sync/keys`, `POST /v1/sync/recovery/join` | keyring records |
| `GET /v1/sync/recovery-wraps` | each keyring record's wraps |
| `/v1/sync/pending*` | device pairing rows (open, read, list, answer, reveal) |
| `PUT /v1/account/sync`, `PUT /v1/devices/{id}/sync` | sync on/off |
| `GET /v1/devices` | `syncPrincipal`, `revokedAt`, `current` per device |
| `GET /v1/me/events` | `sync.advanced` frames carrying the new head |
| `POST /v1/admin/devices/{id}/revoke` | revoke a device (admin token) |

The server does not compact, so `oldest_retained_seq` is 0 and there is never a snapshot. The
client's contract headers (`omo-contract-version`, `omo-sync-capabilities`) are accepted and not
required.
