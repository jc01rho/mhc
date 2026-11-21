# Self-hosted memory sync

`mhc-sync-server` is an open, self-hostable memory-sync service. It stores only what the devices
produce: sealed objects and signed keyring records. It cannot read a memory, because every batch is
sealed to the account's epoch key before it is uploaded, and every record is signed by the device
that wrote it.

By default nothing in mhc talks to a hosted service: a device that is not signed in opens no sync
session, and an account address only exists when you configure one.

## Build and run

```bash
cargo build --release -p maho-sync-server --bin mhc-sync-server
mhc-sync-server --listen 127.0.0.1:8787 --db ./sync.sqlite --tokens ./tokens
```

- `--listen` defaults to `127.0.0.1:8787`. It is loopback by default on purpose: reach it over an
  SSH tunnel or Tailscale, and put TLS in front of it if it must leave the machine.
- `--db` is the SQLite file holding every account's rows. Omit it for an in-memory store (tests
  only).
- `--tokens` is required.

## Tokens

One line per device, four fields:

```
# token account device-id sync-principal
7c1f...e2 account-0123 device-a device:d-9a1
b30d...77 account-0123 device-b device:d-4c8
```

| field | meaning |
| --- | --- |
| `token` | the bearer token the device sends |
| `account` | the account id every row is filed under; one account never sees another's rows |
| `device-id` | the service-side id of that device, recorded as the author of its rows |
| `sync-principal` | the device principal the account's trust chain names |

Rotating a token is editing this file and restarting the server. There is no registration route.

## Pointing mhc at it

Set the service origin and sign in against that origin (the existing setting already accepts a
loopback `http` origin for local use; a remote origin must be `https`):

```bash
export OMO_SERVICE_API_URL=http://127.0.0.1:8787
```

## Freezing a revoked device

Revoking a device freezes batch appends for the account until a remaining member appends a rotation
that excludes the revoked principal. With the store opened by your own tooling
(`maho_sync_server::Store`):

```rust
store.revoke_device("account-0123")?; // sets rotation_pending
```

A keyring append clears the flag, so the device that rotates (automatically, through the client's
freeze settler) ends the freeze.

## What the server sees

| sees | does not see |
| --- | --- |
| row order per account, epoch numbers, chain links and record signatures | memory text, item keys, or any batch plaintext |
| object sizes, SHA-256 digests, and the ciphertext bytes | key material: wraps and keys are sealed to devices |
| device ids and the times rows arrive | passphrases, recovery phrases, or account secrets |

## Routes

The routes and payload shapes are the ones `crates/omo/memory-core/src/sync` already speaks; the
crate documentation of `crates/maho-sync-server/src/lib.rs` lists them. The short version:

- `GET /v1/sync/state` — head, current epoch, `rotation_pending`, retention
- `GET /v1/sync/batches?after=n`, `GET /v1/sync/entries?after=n`, `GET /v1/sync/keys`
- `POST /v1/sync/uploads`, `PUT /v1/sync/uploads/{id}` — reserve and send one object (8 MiB cap)
- `POST /v1/sync/batches`, `PUT /v1/sync/keys`, `POST /v1/sync/recovery/join`
- `GET /v1/me/events` — a Server-Sent Events stream carrying `sync.advanced` / `sync.keyring.changed`

Refusals use `{ "error": { "code": "..." } }`: `unauthenticated` (401), `stale_base` (409, a moved
base or a frozen account), `unknown_upload`, `digest_mismatch`, `not_found` (404), `too_large` (413).

## Tests

`crates/maho-sync-server/tests/e2e_sync.rs` starts the server in-process and drives it with mhc's
real transport: two devices of one account pair, a batch is uploaded and pulled with a matching
digest, a repost answers its original sequence, a moved base is refused, the event stream announces
the new head, and a token of another account reads nothing.
