// Release signatures (docs/OPERATIONS.md "Releases"): the offline ML-DSA-65 key signs each
// release's checksums.txt. Same format and checks as scripts/release-sig.mjs, so tools that
// install a release (the MCP's install_node) can verify it with the SDK alone:
//   message   = "BLOCKULA_RELEASE_V1" ‖ file bytes
//   signature = ML-DSA-65 (FIPS 204, empty context)
//   key id    = hex BLAKE3(public key)
//   .sig file = one JSON line: {"format","key_id","public_key","signature"} (hex)
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";
import { PUBLIC_KEY_LEN, SIGNATURE_LEN } from "./tx.js";

export const RELEASE_SIGNATURE_FORMAT = "blockula-release-signature-v1";
const DOMAIN = new TextEncoder().encode("BLOCKULA_RELEASE_V1");

/** The contents of a `checksums.txt.sig` file. */
export interface ReleaseSignature {
  format: string;
  key_id: string;
  public_key: string;
  signature: string;
}

/** What the release key signs: the domain, then the file's exact bytes. */
export function releaseMessage(file: Uint8Array): Uint8Array {
  const m = new Uint8Array(DOMAIN.length + file.length);
  m.set(DOMAIN);
  m.set(file, DOMAIN.length);
  return m;
}

const isHex = (s: unknown, bytes: number) => typeof s === "string" && s.length === bytes * 2 && /^[0-9a-f]*$/.test(s);

/**
 * Throws unless `sig` is a valid release signature on `file` by the key whose id is
 * `trustedKeyId` (the published release key id, never one taken from the signature itself).
 */
export function verifyReleaseSignature(sig: ReleaseSignature, file: Uint8Array, trustedKeyId: string): void {
  if (!sig || sig.format !== RELEASE_SIGNATURE_FORMAT) throw new Error(`unknown release signature format ${String(sig?.format)}`);
  if (!isHex(trustedKeyId, 32)) throw new Error("the trusted release key id must be 64 lowercase hex characters");
  if (!isHex(sig.public_key, PUBLIC_KEY_LEN) || !isHex(sig.signature, SIGNATURE_LEN)) throw new Error("malformed ML-DSA-65 key or signature");
  const publicKey = hexToBytes(sig.public_key);
  const id = bytesToHex(blake3(publicKey));
  if (sig.key_id !== id) throw new Error("the signature's key_id doesn't match its public key");
  if (id !== trustedKeyId) throw new Error(`signed by key ${id}, not the release key ${trustedKeyId}`);
  if (!ml_dsa65.verify(hexToBytes(sig.signature), releaseMessage(file), publicKey)) throw new Error("INVALID release signature");
}
