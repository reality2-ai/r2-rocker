/* tslint:disable */
/* eslint-disable */

/**
 * The R2 Hive running in the browser.
 *
 * Contains the EventBus with the Notekeeper sentant and sync plugin.
 * JavaScript interacts with it by sending events and polling for
 * outbound actions.
 */
export class R2Hive {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Drain all outbound events (events the sentant wants to send externally).
     *
     * Returns a JSON array of events: [{"hash":N,"payload":"hex"}, ...]
     * JavaScript processes these (encrypt + relay, UI update, etc).
     */
    drain_outbound(): string;
    /**
     * Create a new hive with the Notekeeper sentant and sync plugin.
     */
    constructor();
    /**
     * Push incoming sync data from another device (already decrypted).
     *
     * `payload` is the CBOR-encoded R2-PLUGIN result envelope.
     */
    push_sync_inbound(payload: Uint8Array): void;
    /**
     * Send an event to the sentant.
     *
     * `event_hash` is the FNV-1a hash of the event name.
     * `payload` is CBOR-encoded event parameters.
     *
     * After calling this, check `drain_outbound()` for events the
     * sentant wants to send (sync, notifications, etc).
     */
    send_event(event_hash: number, payload: Uint8Array): void;
    /**
     * Process one tick of the engine.
     */
    tick(): void;
}

/**
 * Opaque handle to a MemberState (device/joiner side).
 */
export class R2Member {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Restore member state from previously serialized bytes.
     *
     * Use this on page load to restore trust group membership from localStorage.
     */
    static from_bytes(bytes: Uint8Array): R2Member;
    /**
     * Check if the membership certificate is valid at the given time.
     */
    is_valid(now: bigint): boolean;
    /**
     * Sign a relay HELLO message and return the complete JSON string.
     *
     * Produces: `{"type":"hello","version":1,"trust_group":"...","device_id":"...","timestamp":N,"signature":"..."}`
     *
     * The signature is Ed25519 over `"{trust_group}:{device_id}:{timestamp}"`.
     */
    sign_relay_hello(timestamp: bigint): string;
    /**
     * Serialize member state to bytes for persistent storage.
     *
     * Returns 277 bytes containing device key, certificate, DEK, HK.
     * **These bytes contain secret key material — encrypt before storing.**
     */
    to_bytes(): Uint8Array;
    /**
     * Trust group hash for relay HELLO (first 8 bytes of SHA-256 of TG_PK, as 16 hex chars).
     */
    trust_group_hash(): string;
    /**
     * DEK for encryption (32 bytes).
     */
    readonly dek: Uint8Array;
    /**
     * HK for HMAC operations (32 bytes).
     */
    readonly hk: Uint8Array;
    /**
     * Device's public key (32 bytes).
     */
    readonly public_key: Uint8Array;
    /**
     * Trust group public key (32 bytes).
     */
    readonly trust_group_id: Uint8Array;
}

/**
 * Opaque handle to a TrustGroup (key holder side).
 * Stored in WASM memory; JS holds the index.
 */
export class R2TrustGroup {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Restore key holder state from previously serialized bytes.
     *
     * Restores the signing key and derived keys. Member list starts empty —
     * members rejoin via the normal join protocol or are restored separately.
     */
    static from_bytes(bytes: Uint8Array, now: bigint): R2TrustGroup;
    /**
     * Generate a join code. Returns the 16-byte code as hex string.
     *
     * `now` is current Unix timestamp, `ttl_secs` is validity duration.
     */
    generate_join_code(now: bigint, ttl_secs: bigint): string;
    /**
     * List members as JSON array of {name, public_key_hex} objects.
     */
    member_list(): any;
    /**
     * List member names as a JSON array.
     */
    member_names(): any;
    /**
     * Create a new trust group. Returns the key holder's trust group handle.
     *
     * `now` is the current Unix timestamp in seconds.
     */
    constructor(now: bigint);
    /**
     * Process a join request from a device.
     *
     * - `join_code_hex`: the 16-byte join code as hex string
     * - `device_public_key`: the joiner's Ed25519 public key (32 bytes)
     * - `device_name`: human-readable name for the device
     * - `now`: current Unix timestamp
     *
     * Returns the encrypted join response as bytes (to send to the joiner).
     */
    process_join(join_code_hex: string, device_public_key: Uint8Array, device_name: string, now: bigint): Uint8Array;
    /**
     * Revoke a member by their public key hex string. Key holder only.
     */
    revoke_member(public_key_hex: string, now: bigint): void;
    /**
     * Serialize key holder state to bytes for persistent storage.
     *
     * Returns 38 bytes (signing key + sequence + crypto level).
     * **Contains TG_SK — the root secret. Encrypt before storing.**
     */
    to_bytes(): Uint8Array;
    /**
     * DEK (data encryption key), 32 bytes.
     */
    readonly dek: Uint8Array;
    /**
     * HK (HMAC key), 32 bytes.
     */
    readonly hk: Uint8Array;
    /**
     * Number of members (excluding key holder).
     */
    readonly member_count: number;
    /**
     * Trust group public key (32 bytes). This is the trust group ID.
     */
    readonly public_key: Uint8Array;
}

/**
 * Decode a note event CBOR payload: {0: opCode, 1: noteId, 2: timestamp, 3?: encryptedContent}.
 *
 * Returns a JS object with `op_code`, `note_id`, `timestamp`, and optionally `encrypted_content` (Uint8Array).
 */
export function cbor_decode_note_event(payload: Uint8Array): any;

/**
 * Encode a simple CBOR map: { key0: val0, key1: val1, ... }
 *
 * Takes parallel arrays of integer keys and integer values.
 * Returns CBOR-encoded bytes (compact mode).
 */
export function cbor_encode_int_map(keys: Uint8Array, values: Uint32Array): Uint8Array;

/**
 * Encode a note event CBOR payload: {0: opCode, 1: noteId, 2: timestamp, 3?: encryptedContent}.
 *
 * Key 3 (encrypted content) is only included if `encrypted_content` is non-empty.
 * This packs both metadata and content into a single R2-WIRE frame payload.
 */
export function cbor_encode_note_event(op_code: number, note_id: number, timestamp: number, encrypted_content: Uint8Array): Uint8Array;

/**
 * Complete the join handshake (device side).
 *
 * - `device_secret_key`: the device's Ed25519 secret key (32 bytes)
 * - `trust_group_public_key`: the trust group's public key (32 bytes)
 * - `encrypted_response`: the encrypted join response bytes from the key holder
 * - `now`: current Unix timestamp
 *
 * Returns an R2Member handle on success.
 */
export function complete_join(device_secret_key: Uint8Array, trust_group_public_key: Uint8Array, encrypted_response: Uint8Array, now: bigint): R2Member;

/**
 * Compute trust group hash from a public key (first 8 bytes of SHA-256, as 16 hex chars).
 */
export function compute_trust_group_hash(tg_public_key: Uint8Array): string;

/**
 * Decode a compact R2-WIRE frame.
 *
 * Returns a JS object with header fields and payload.
 */
export function decode_compact_frame(data: Uint8Array): any;

/**
 * Decode an extended R2-WIRE frame.
 */
export function decode_extended_frame(data: Uint8Array): any;

/**
 * Decode an invite string → { tg_public_key: [32], join_code_hex: string, trust_group_hash: string }
 */
export function decode_invite(invite: string): any;

/**
 * Decode 3 words back to trust group prefix (3 hex chars) + join code fragment.
 *
 * Input: "word1-word2-word3" or "word1 word2 word3"
 * Returns JS object: { tg_prefix_hex: "abc", join_secret_hex: "123456" }
 */
export function decode_word_code(words: string): any;

/**
 * Decrypt data with the trust group DEK (XChaCha20-Poly1305).
 *
 * Input: [nonce: 24 bytes] [ciphertext + auth tag] (as produced by encrypt_with_dek).
 * Returns the plaintext, or throws if decryption/authentication fails.
 */
export function decrypt_with_dek(dek: Uint8Array, encrypted: Uint8Array): Uint8Array;

/**
 * Derive trust group keys (DEK + HK) from raw secret and public key bytes.
 *
 * Both `tg_secret` and `tg_public` must be 32 bytes (Ed25519 key material).
 * Returns a JS object with `dek` and `hk` as byte arrays.
 */
export function derive_group_keys(tg_secret: Uint8Array, tg_public: Uint8Array): any;

/**
 * Encode a compact R2-WIRE frame.
 *
 * Parameters:
 * - `msg_type`: 0=Event, 2=Reply, 3=Ack, 4=Nack, 5=Heartbeat
 * - `ttl`: time-to-live (0-15)
 * - `k`: relay budget (0-15)
 * - `msg_id`: 16-bit message ID
 * - `event_hash`: 32-bit FNV-1a hash of event name
 * - `target`: 32-bit target hive address (0 = broadcast)
 * - `payload`: CBOR-encoded payload bytes
 *
 * Returns the encoded frame bytes.
 */
export function encode_compact_frame(msg_type: number, ttl: number, k: number, msg_id: number, event_hash: number, target: number, payload: Uint8Array): Uint8Array;

/**
 * Encode an extended R2-WIRE frame.
 */
export function encode_extended_frame(msg_type: number, ttl: number, k: number, msg_id: number, event_hash: number, target_group: number, target_hive: number, payload: Uint8Array): Uint8Array;

/**
 * Encode an invite: TG_PK (32 bytes) + join_code (16 bytes) → base64url string.
 *
 * The invite contains everything a joiner needs to connect and join:
 * the trust group public key (to compute hash and decrypt response)
 * and the join code secret.
 */
export function encode_invite(tg_public_key: Uint8Array, join_code_hex: string): string;

/**
 * Encode a note ID-only payload as CBOR.
 *
 * {0: id, 3: timestamp}
 * Used for delete events.
 */
export function encode_note_id_payload(id: string, timestamp: bigint): Uint8Array;

/**
 * Encode a note event payload as CBOR.
 *
 * {0: id, 1: title, 2: content, 3: timestamp}
 * Used by JavaScript to build payloads for hive.send_event().
 */
export function encode_note_payload(id: string, title: string, content: string, timestamp: bigint): Uint8Array;

/**
 * Encode a trust group hash + join code as 3 words.
 *
 * `tg_hash_hex`: 16-char hex trust group hash
 * `join_code_hex`: 32-char hex join code
 * Returns: "word1-word2-word3"
 */
export function encode_word_code(tg_hash_hex: string, join_code_hex: string): string;

/**
 * Encrypt data with the trust group DEK (XChaCha20-Poly1305).
 *
 * Returns: [nonce: 24 bytes] [ciphertext + auth tag]
 * Used for plugin-to-plugin data exchange (note content, files, etc.)
 * The relay sees only ciphertext.
 */
export function encrypt_with_dek(dek: Uint8Array, plaintext: Uint8Array): Uint8Array;

/**
 * Raw FNV-1a 32-bit hash of pre-canonicalised bytes.
 */
export function fnv1a_32(data: Uint8Array): number;

/**
 * Wrap a frame with a 4-byte big-endian length prefix (TCP framing).
 */
export function frame_with_be_prefix(frame: Uint8Array): Uint8Array;

/**
 * Wrap a frame with a 2-byte little-endian length prefix (BLE/USB framing).
 */
export function frame_with_le_prefix(frame: Uint8Array): Uint8Array;

/**
 * Generate a new Ed25519 device keypair.
 *
 * Returns a JS object with `secret_key` (32 bytes) and `public_key` (32 bytes).
 */
export function generate_device_keypair(): any;

/**
 * Compute an HMAC tag for a compact frame.
 *
 * `hk` must be 32 bytes. Returns the 8-byte truncated HMAC tag.
 * The caller is responsible for appending the tag to the frame and setting
 * the has_hmac flag.
 */
export function hmac_compact_tag(frame_bytes: Uint8Array, hk: Uint8Array): Uint8Array;

/**
 * Compute the HMAC tag for an extended R2-WIRE frame.
 *
 * `frame_bytes` must be a valid extended R2-WIRE frame (without HMAC).
 * `hk` must be 32 bytes (the trust group's HMAC key).
 * Returns the 32-byte HMAC-SHA256 tag.
 */
export function hmac_extended_tag(frame_bytes: Uint8Array, hk: Uint8Array): Uint8Array;

/**
 * Hash an event name to a 32-bit FNV-1a identifier.
 *
 * Canonicalises the input (lowercase, whitespace-stripped) before hashing.
 * Returns the hash, or throws on empty/reserved names.
 */
export function r2_hash(event_name: string): number;

/**
 * Returns the r2-wasm version string.
 */
export function r2_version(): string;

/**
 * Sign a message with an Ed25519 secret key.
 *
 * Used for relay HELLO signing when joining (before we have a full R2Member).
 * `secret_key` must be 32 bytes. Returns 64-byte Ed25519 signature.
 */
export function sign_ed25519(secret_key: Uint8Array, message: Uint8Array): Uint8Array;

/**
 * Transcode an extended frame to compact format.
 */
export function transcode_to_compact(extended_bytes: Uint8Array): Uint8Array;

/**
 * Transcode a compact frame to extended format.
 */
export function transcode_to_extended(compact_bytes: Uint8Array): Uint8Array;

/**
 * Verify a compact frame's HMAC tag.
 *
 * The frame must include the HMAC tag (has_hmac flag set).
 * `hk` must be 32 bytes. Returns true if valid.
 */
export function verify_compact_hmac(signed_frame: Uint8Array, hk: Uint8Array): boolean;

/**
 * Verify an extended frame's HMAC tag.
 *
 * The frame must include the HMAC tag (has_hmac flag set).
 * `hk` must be 32 bytes. Returns true if valid.
 */
export function verify_extended_hmac(signed_frame: Uint8Array, hk: Uint8Array): boolean;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_r2member_free: (a: number, b: number) => void;
    readonly __wbg_r2trustgroup_free: (a: number, b: number) => void;
    readonly cbor_decode_note_event: (a: number, b: number, c: number) => void;
    readonly cbor_encode_int_map: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly cbor_encode_note_event: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly complete_join: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: bigint) => void;
    readonly compute_trust_group_hash: (a: number, b: number, c: number) => void;
    readonly decode_compact_frame: (a: number, b: number, c: number) => void;
    readonly decode_extended_frame: (a: number, b: number, c: number) => void;
    readonly decode_invite: (a: number, b: number, c: number) => void;
    readonly decode_word_code: (a: number, b: number, c: number) => void;
    readonly decrypt_with_dek: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly derive_group_keys: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly encode_compact_frame: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly encode_extended_frame: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly encode_invite: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly encode_note_id_payload: (a: number, b: number, c: number, d: bigint) => void;
    readonly encode_note_payload: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: bigint) => void;
    readonly encode_word_code: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly encrypt_with_dek: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly fnv1a_32: (a: number, b: number) => number;
    readonly frame_with_be_prefix: (a: number, b: number, c: number) => void;
    readonly frame_with_le_prefix: (a: number, b: number, c: number) => void;
    readonly generate_device_keypair: (a: number) => void;
    readonly hmac_compact_tag: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly hmac_extended_tag: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly r2_hash: (a: number, b: number, c: number) => void;
    readonly r2_version: (a: number) => void;
    readonly r2member_dek: (a: number, b: number) => void;
    readonly r2member_from_bytes: (a: number, b: number, c: number) => void;
    readonly r2member_hk: (a: number, b: number) => void;
    readonly r2member_is_valid: (a: number, b: bigint) => number;
    readonly r2member_public_key: (a: number, b: number) => void;
    readonly r2member_sign_relay_hello: (a: number, b: number, c: bigint) => void;
    readonly r2member_to_bytes: (a: number, b: number) => void;
    readonly r2member_trust_group_hash: (a: number, b: number) => void;
    readonly r2member_trust_group_id: (a: number, b: number) => void;
    readonly r2trustgroup_dek: (a: number, b: number) => void;
    readonly r2trustgroup_from_bytes: (a: number, b: number, c: number, d: bigint) => void;
    readonly r2trustgroup_generate_join_code: (a: number, b: number, c: bigint, d: bigint) => void;
    readonly r2trustgroup_hk: (a: number, b: number) => void;
    readonly r2trustgroup_member_count: (a: number) => number;
    readonly r2trustgroup_member_list: (a: number) => number;
    readonly r2trustgroup_member_names: (a: number) => number;
    readonly r2trustgroup_new: (a: number, b: bigint) => void;
    readonly r2trustgroup_process_join: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: bigint) => void;
    readonly r2trustgroup_public_key: (a: number, b: number) => void;
    readonly r2trustgroup_revoke_member: (a: number, b: number, c: number, d: number, e: bigint) => void;
    readonly r2trustgroup_to_bytes: (a: number, b: number) => void;
    readonly sign_ed25519: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly transcode_to_compact: (a: number, b: number, c: number) => void;
    readonly transcode_to_extended: (a: number, b: number, c: number) => void;
    readonly verify_compact_hmac: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly verify_extended_hmac: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly __wbg_r2hive_free: (a: number, b: number) => void;
    readonly r2hive_drain_outbound: (a: number, b: number) => void;
    readonly r2hive_new: () => number;
    readonly r2hive_push_sync_inbound: (a: number, b: number, c: number) => void;
    readonly r2hive_send_event: (a: number, b: number, c: number, d: number) => void;
    readonly r2hive_tick: (a: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
