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

One line per device, four required fields plus optional ones:

```
# token account device-id sync-principal [admin] [seal=<key>] [sign=<key>]
7c1f...e2 account-0123 device-a device:d-9a1 seal=8Zk... sign=Qm1...
b30d...77 account-0123 device-b device:d-4c8
9f01...ab acct-admin device-admin device:admin admin
```

| field | meaning |
| --- | --- |
| `token` | the bearer token the device sends |
| `account` | the account id every row is filed under; one account never sees another's rows |
| `device-id` | the service-side id of that device, recorded as the author of its rows |
| `sync-principal` | the device principal the account's trust chain names |
| `admin` | optional; the token may call `POST /v1/admin/devices/{id}/revoke` |
| `seal=` / `sign=` | optional; the device's public keys as the account chain states them |

Rotating a token is editing this file and restarting the server. There is no registration route.

## Pointing mhc at it

Set the service origin and sign in against that origin (the existing setting already accepts a
loopback `http` origin for local use; a remote origin must be `https`):

```bash
export OMO_SERVICE_API_URL=http://127.0.0.1:8787
```

## Revoking a device

```bash
curl -X POST -H "Authorization: Bearer <admin token>" \
  http://127.0.0.1:8787/v1/admin/devices/device-b/revoke
```

The device's row in `GET /v1/devices` gets a `revokedAt` timestamp, every other request from it is
refused `device_revoked`, and the account is marked `rotation_pending`. A remaining device sees the
revocation through the client's freeze settler, rotates the epoch key with a keyring append, and
syncing resumes: a landed keyring record is what clears the hold. Remove the revoked entry from the
tokens file at your next restart.

Revoking through the store is still available for an operator's own tooling
(`maho_sync_server::Store::revoke_device(account)`), which sets the same flag for the whole account.

## What the server sees

| sees | does not see |
| --- | --- |
| row order per account, epoch numbers, chain links and record signatures | memory text, item keys, or any batch plaintext |
| object sizes, SHA-256 digests, and the ciphertext bytes | key material: wraps and keys are sealed to devices |
| device ids, revocations and the times rows arrive | passphrases, recovery phrases, or account secrets |

## Routes

The routes and payload shapes are the ones `crates/omo/memory-core/src/sync` already speaks; the
crate documentation of `crates/maho-sync-server/src/lib.rs` lists them. The short version:

- `GET /v1/sync/state` — head (`seq`, `link`, `keyring_link`), current epoch, `rotation_pending`, retention
- `GET /v1/sync/batches?after=n` — the pull page (seq, kind, author, digest, object id, batch id)
- `GET /v1/sync/entries?after=n` — the account chain's rows, in the shape `envelope::parse_entry` accepts
- `GET /v1/sync/keys`, `GET /v1/sync/recovery-wraps`
- `POST /v1/sync/uploads`, `PUT /v1/sync/uploads/{id}` — reserve and send one object (8 MiB cap, digest checked)
- `POST /v1/sync/batches`, `PUT /v1/sync/keys`, `POST /v1/sync/recovery/join`
- `PUT /v1/sync/pending`, `GET /v1/sync/pending`, `GET /v1/sync/pending/{id}`,
  `POST /v1/sync/pending/{id}/answer`, `POST /v1/sync/pending/{id}/reveal` — the pairing ceremony
- `PUT /v1/account/sync`, `POST /v1/confirm`, `PUT /v1/devices/{id}/sync` — sync on/off per account and device
- `GET /v1/devices` — `syncPrincipal`, `revokedAt`, `current` per device
- `GET /v1/me/events` — a Server-Sent Events stream carrying `sync.advanced` / `sync.keyring.changed`
- `POST /v1/admin/devices/{id}/revoke` — revoke one device (admin token)

Refusals use `{ "error": { "code": "..." } }`: `unauthenticated` (401), `device_revoked` (401),
`cross_account` (401), `forbidden` (403), `stale_base` (409, a moved base or a frozen account),
`unknown_upload`, `digest_mismatch`, `object_mismatch` (400), `not_found` (404), `too_large` (413).

## Tests

`crates/maho-sync-server/tests/e2e_sync.rs` starts the server in-process and drives it with mhc's
real client (`HttpTransport`, `AccountHttp`, `PairingHttp`, `read_revocation_view`): two devices of
one account pair, a batch is uploaded and pulled with a matching digest, a repost answers its
original sequence, a moved base is refused, a keyring append lands and unfreezes the account, a
revocation is reported through the device list, the event stream announces the new head, and a token
of another account reads nothing.
