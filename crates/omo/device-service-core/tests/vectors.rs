use device_service_core::local_api::chunking::{BlobOutcome, LocalBlobAssembler, blob_events};
use device_service_core::local_api::framing::{
    DecodedLine, LOCAL_LINE_MAX, LineDecoder, encode_line, parse_line,
};
use device_service_core::local_api::limits::{
    LOCAL_CHUNK_MAX, LOCAL_INLINE_MAX, chunk_payload, command_text_fits, line_bytes,
};
use device_service_core::local_api::proof::{
    client_proof, decoded_bytes, mint_id, mint_nonce, proof_matches, server_proof,
};
use serde_json::json;

const TOKEN: [u8; 32] = [0x11; 32];
const NONCE: &str = "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI";

#[test]
fn the_documented_proof_vectors() {
    assert_eq!(
        decoded_bytes(
            "ERERERERERERERERERERERERERERERERERERERERERE",
            32,
            "the token"
        )
        .expect("token"),
        TOKEN.to_vec()
    );
    let vectors = [
        (
            "unix",
            "TLdIwpwhlMugnecRwFIoNTrwIxH9HjwJ98_hpMaqKIQ",
            "sfkHyU_lyf5fLfS9ag1RJI50KPgq4iqBCOXFWn8e0N8",
        ),
        (
            "tcp:49152",
            "4BSzSrSHpBmfLPEkF5-xi39PnUfJO5W3RZKOZNDa0fc",
            "ODo5pWCMHx-GmOQOtLWFbYYNCUqJp-ZChkEqTbVjcAo",
        ),
    ];
    for (endpoint, server, client) in vectors {
        assert_eq!(
            server_proof(&TOKEN, NONCE, endpoint).expect("server"),
            server
        );
        assert_eq!(
            client_proof(&TOKEN, NONCE, endpoint).expect("client"),
            client
        );
    }
    let unix = server_proof(&TOKEN, NONCE, "unix").expect("unix");
    let tcp = server_proof(&TOKEN, NONCE, "tcp:49152").expect("tcp");
    assert!(proof_matches(&unix, &unix));
    assert!(
        !proof_matches(&unix, &tcp),
        "a proof is bound to its endpoint"
    );
    assert!(!proof_matches(&unix, &unix[1..]));
}

#[test]
fn nonces_and_ids_are_canonical_base64url() {
    let nonce = mint_nonce().expect("nonce");
    assert_eq!(nonce.len(), 43);
    assert_eq!(
        decoded_bytes(&nonce, 32, "a nonce").expect("decode").len(),
        32
    );
    assert_eq!(mint_id().expect("id").len(), 22);
    for refused in [
        "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI=",
        "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiJ",
        "IiIi",
        "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIi+",
    ] {
        assert_eq!(
            decoded_bytes(refused, 32, "a nonce")
                .expect_err(refused)
                .code,
            "protocol_violation"
        );
    }
    assert!(server_proof(&TOKEN, "short", "unix").is_err());
}

#[test]
fn an_oversized_line_is_reported_once_and_the_stream_resynchronizes() {
    let mut decoder = LineDecoder::new();
    let first = decoder.push(b"{\"a\":1}\n{\"b\"");
    assert_eq!(first, vec![DecodedLine::Line(b"{\"a\":1}".to_vec())]);
    assert_eq!(
        decoder.push(b":2}\n"),
        vec![DecodedLine::Line(b"{\"b\":2}".to_vec())]
    );

    let big = vec![b'x'; LOCAL_LINE_MAX + 10];
    let mut events = decoder.push(&big[..LOCAL_LINE_MAX]);
    assert!(events.is_empty(), "exactly LOCAL_LINE_MAX bytes still fit");
    events.extend(decoder.push(&big[LOCAL_LINE_MAX..]));
    events.extend(decoder.push(b"more of the same oversized line"));
    assert_eq!(events, vec![DecodedLine::Oversized(LOCAL_LINE_MAX + 10)]);
    assert_eq!(
        decoder.push(b"\n{\"c\":3}\n"),
        vec![DecodedLine::Line(b"{\"c\":3}".to_vec())]
    );
    assert_eq!(parse_line(b"{\"c\":3}").expect("json"), json!({"c": 3}));
    assert_eq!(
        parse_line(b"not json").expect_err("json").code,
        "protocol_violation"
    );
    assert_eq!(
        parse_line(&[0xff, 0xfe]).expect_err("utf8").code,
        "protocol_violation"
    );
}

#[test]
fn a_writer_never_produces_an_oversized_line() {
    let line = encode_line(&json!({"type": "hello"})).expect("small");
    assert_eq!(line.last(), Some(&b'\n'));
    let huge = json!({ "b": "x".repeat(LOCAL_LINE_MAX) });
    assert_eq!(
        encode_line(&huge).expect_err("huge").code,
        "protocol_violation"
    );
}

#[test]
fn chunk_arithmetic_la19() {
    let full = chunk_payload(&vec![7u8; LOCAL_CHUNK_MAX]);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].1.len(), 349_528, "4 x ceil(262144 / 3)");
    let largest_chunk_line = json!({
        "type": "event",
        "kind": "session.blob_chunk",
        "attach": "a".repeat(64),
        "id": "i".repeat(64),
        "off": 999_999_999u64,
        "b": full[0].1,
    });
    assert!(line_bytes(&largest_chunk_line) <= 349_744);
    assert!(line_bytes(&largest_chunk_line) < LOCAL_INLINE_MAX);
    let parts = chunk_payload(&vec![1u8; 3 * LOCAL_CHUNK_MAX + 5]);
    assert_eq!(
        parts.iter().map(|(off, _)| *off).collect::<Vec<_>>(),
        vec![0, LOCAL_CHUNK_MAX, 2 * LOCAL_CHUNK_MAX, 3 * LOCAL_CHUNK_MAX]
    );
    assert!(command_text_fits(&"y".repeat(1_000_000)));
    assert!(!command_text_fits(&"y".repeat(1_000_001)));
}

#[test]
fn a_blob_reassembles_and_a_tampered_one_is_refused() {
    let data: Vec<u8> = (0..(LOCAL_CHUNK_MAX + 100))
        .map(|index| (index % 251) as u8)
        .collect();
    let events = blob_events(
        "att",
        "b1",
        &json!({"of": "state", "position": {"attach_id": "x"}}),
        &data,
    );
    assert_eq!(events.len(), 4);
    let mut assembler = LocalBlobAssembler::default();
    let mut outcome = None;
    for event in &events {
        outcome = assembler.accept(event).expect("in order");
    }
    match outcome {
        Some(BlobOutcome::Blob {
            open,
            data: assembled,
        }) => {
            assert_eq!(assembled, data);
            assert_eq!(open["of"], "state");
        }
        other => panic!("expected a blob, got {other:?}"),
    }
    assert!(!assembler.in_flight());

    let mut tampered = events.clone();
    tampered[0]["sha256"] = json!("0".repeat(64));
    let mut assembler = LocalBlobAssembler::default();
    let mut refused = None;
    for event in &tampered {
        if let Err(error) = assembler.accept(event) {
            refused = Some(error);
        }
    }
    assert_eq!(
        refused.expect("digest mismatch").reason,
        "blob digest mismatch"
    );

    let mut assembler = LocalBlobAssembler::default();
    assembler.accept(&events[0]).expect("open");
    assert!(
        assembler.accept(&events[2]).is_err(),
        "a chunk out of order"
    );
}
