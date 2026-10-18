// install_node's checks on a release download, the same as scripts/install.sh (docs/OPERATIONS.md
// "Releases"):
//   0. The version (a vX.Y.Z tag; `latest` is resolved to its tag first) is bound to everything
//      below: checksums.txt must name it on its first line ("# blockula-release vX.Y.Z", covered
//      by the signature), provenance must come from its tag (--source-ref), and install_node checks
//      the extracted binary's `--version` before it installs it. An older, genuinely signed
//      release served as the one asked for is refused (audit supply-chain-B-2).
//   1. SHA-256 against the release's checksums.txt. Always. It only catches a corrupted download:
//      whoever can change the release can change checksums.txt too.
//   2. SLSA build provenance with a logged-in GitHub CLI (`gh attestation verify`): the archive was
//      built by this repo's release workflow. A failed check is fatal; no gh skips it.
//   3. The offline ML-DSA-65 signature on checksums.txt (checksums.txt.sig), verified with the
//      SDK against the release key id. Once a key id is known (pinned in
//      $BLOCKULA_HOME/release-key-id by `blockula release install`, built in, or
//      BLOCKULA_RELEASE_KEY_ID; all that are set must agree), every release must carry a valid
//      signature: a missing or invalid .sig is fatal. Leaving the .sig out is exactly what a
//      compromised GitHub account or release workflow would do, and it can't forge the offline
//      signature (audit F69). With no key id (before the key ceremony, nothing pinned) there's
//      nothing to check it against, so it's skipped.
// install_node installs to a path of its own ($BLOCKULA_HOME/mcp-bin/blockula), never over the
// binary `blockula release install` puts in $BLOCKULA_HOME/bin (audit
// review-r3-integrate-compose-1).
// The checks add up (AND): provenance and the signature each refuse a release they fail. If
// neither ran, the result says so loudly. BLOCKULA_REQUIRE_VERIFY=1 refuses a release it can only
// checksum, and once a key id is known it requires provenance too (both checks).
import { createHash } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { verifyReleaseSignature, type ReleaseSignature } from "@blockula/sdk";

export const REPO = "jackdishman/blockula";
/**
 * BLAKE3 of the release public key (docs/OPERATIONS.md "The release key"). Empty until the key
 * ceremony fills it in, together with scripts/install.sh's and node/src/release_cli.rs's
 * BUILTIN_KEY_ID (a test keeps the three equal); BLOCKULA_RELEASE_KEY_ID sets it meanwhile.
 */
export const RELEASE_KEY_ID = "";
/** The release key id pin `blockula release install` writes (release_cli.rs PIN_FILE), in $BLOCKULA_HOME. */
export const PIN_FILE = "release-key-id";

/** Where install_node puts the node binary, and where `blockula release install` and install.sh put theirs (never overwritten by install_node). */
export function binPaths(home: string): { mcp: string; cli: string } {
  return { mcp: join(home, "mcp-bin", "blockula"), cli: join(home, "bin", "blockula") };
}

/** A node binary the MCP could run, and what its `--version` printed (undefined: it didn't run). */
export interface NodeBinary {
  path: string;
  version?: string;
}

/**
 * The node binary start_dev_node and summon_chain run when BLOCKULA_BIN isn't set (audit
 * review-r4-supply-3). `found` lists the installed ones, `blockula release install`'s first (it was
 * checked against the pinned key), then install_node's. The first whose `--version` is this MCP's
 * wins; else the first that runs; else the first; with none, `blockula` from PATH. `matches` says
 * whether it's this MCP's version, so a stale binary is never run without saying so.
 */
export function pickNodeBinary(found: NodeBinary[], mcpVersion: string): NodeBinary & { matches: boolean } {
  const same = found.find((b) => b.version === `blockula ${mcpVersion}`);
  if (same) return { ...same, matches: true };
  return { ...(found.find((b) => b.version !== undefined) ?? found[0] ?? { path: "blockula" }), matches: false };
}

/**
 * The release key id pinned in `file` (`blockula release pin` / the first `release install`), or
 * null if there's no pin. Same rules as release_cli.rs read_pin: one 64-hex key id (any case,
 * surrounding whitespace ignored), never ignored when it's malformed, and refused when the group
 * or others can write it (they could have swapped the key it vouches for).
 */
export function readReleasePin(file: string): string | null {
  let text: string;
  try {
    text = readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw new Error(`reading ${file}: ${(e as Error).message}`);
  }
  if (process.platform !== "win32") {
    const mode = statSync(file).mode;
    if (mode & 0o022) throw new Error(`${file} is writable by others (mode ${(mode & 0o777).toString(8)}), so it can't vouch for a release key: chmod 644 it once you've checked it holds your key id; not installing`);
  }
  const k = text.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(k)) throw new Error(`${file} isn't a release key id pin: fix it or remove it; not installing`);
  return k;
}
/** A release tag. Only these are installed: `v*` tags are admin-only (deploy/github/ruleset-tags.json). */
export const RELEASE_TAG = /^v\d+\.\d+\.\d+$/;
/** The first line of a release's checksums.txt, before the tag (scripts/release-checksums.sh). */
export const TAG_LINE_PREFIX = "# blockula-release ";

/**
 * The release key id to trust: the one pinned on this machine (`blockula release install`'s pin,
 * `pinned`, read from `pinFile` by readReleasePin), BLOCKULA_RELEASE_KEY_ID (from an independent
 * channel) or the one built in. Every one that's set must be the same key: a mismatch means one of
 * them isn't genuine (or the pin moved by a rotation this MCP predates: update the MCP).
 */
export function releaseKeyIdFrom(given: string | undefined, builtIn: string, pinned: string | null = null, pinFile = PIN_FILE): string {
  const norm = (k: string | undefined | null, what: string) => {
    const v = (k ?? "").trim().toLowerCase();
    if (v && !/^[0-9a-f]{64}$/.test(v)) throw new Error(`${what} must be 64 hex characters`);
    return v;
  };
  const g = norm(given, "BLOCKULA_RELEASE_KEY_ID");
  const b = norm(builtIn, "the built-in release key id");
  const p = norm(pinned, `the release key id pinned in ${pinFile}`);
  if (p && g && g !== p) throw new Error(`the release key id pinned in ${pinFile} is ${p}, not BLOCKULA_RELEASE_KEY_ID (${g}); not installing. A new release key is accepted only through a rotation statement the pinned key signed (blockula release pin --key-id <new> --rotation …, docs/OPERATIONS.md "The release key").`);
  if (p && b && b !== p) throw new Error(`this MCP was built with release key ${b}, but ${pinFile} pins ${p}; not installing. One of them isn't genuine, or the key was rotated after this MCP was released: check the pin (blockula release pin) against SECURITY.md and your operators' channel, and update the MCP.`);
  if (g && b && g !== b) throw new Error(`this MCP trusts release key ${b}, which isn't BLOCKULA_RELEASE_KEY_ID (${g}); not installing. Check both against SECURITY.md and the website.`);
  return p || g || b;
}

/** The tag GitHub's /releases/latest redirected to (the final URL, …/releases/tag/<tag>). */
export function latestTagFrom(finalUrl: string): string {
  const m = /\/releases\/tag\/([^/?#]+)$/.exec(finalUrl);
  const tag = m ? decodeURIComponent(m[1]) : "";
  if (!RELEASE_TAG.test(tag)) throw new Error(`the latest release (${finalUrl}) isn't a release tag (vX.Y.Z); pass a version`);
  return tag;
}

/** Throws unless `blockula --version` printed release `tag`'s version. */
export function checkBinaryVersion(versionOutput: string, tag: string): void {
  const got = versionOutput.trim();
  if (got !== `blockula ${tag.slice(1)}`) throw new Error(`the downloaded binary is ${got || "unrunnable here"}, not ${tag.slice(1)}; not installing`);
}

/** Throws unless checksums.txt names release `tag` on its first line. */
export function checkTagLine(sums: Buffer, tag: string): void {
  const first = sums.toString("utf8").split("\n", 1)[0];
  if (first === TAG_LINE_PREFIX + tag) return;
  if (first.startsWith(TAG_LINE_PREFIX)) throw new Error(`this checksums.txt is for ${first.slice(TAG_LINE_PREFIX.length)}, not ${tag}: an older or other release served as the one asked for; not installing`);
  throw new Error(`this checksums.txt names no release (releases before v0.5.0 don't), so it can't be bound to ${tag}; not installing`);
}

export interface ReleaseDeps {
  /** GET a release file; `null` if it doesn't exist (404). */
  download(url: string): Promise<Buffer | null>;
  /**
   * Runs `gh attestation verify` on the archive's bytes, requiring provenance from `refs/tags/<tag>`:
   * `"ok"`, `"failed"` (with the reason), or `"unavailable"` (no gh, or not logged in).
   */
  attest(archive: Buffer, name: string, tag: string): Promise<{ result: "ok" | "failed" | "unavailable"; detail?: string }>;
}

export interface VerifiedRelease {
  archive: Buffer;
  sha256: string;
  /** What proved the release genuine (empty: only the checksum). */
  verified: string[];
  /** Shown to the user as-is. */
  warning?: string;
}

const sha256 = (b: Buffer) => createHash("sha256").update(b).digest("hex");

/** Downloads `asset` from `base` (release `tag`'s download URL) and checks it as described above. Throws instead of returning anything unverified that must not be installed. The caller still checks the binary's `--version` (checkBinaryVersion). */
export async function fetchVerifiedRelease(base: string, asset: string, opts: { releaseKeyId: string; requireVerify: boolean; tag: string }, deps: ReleaseDeps): Promise<VerifiedRelease> {
  if (!RELEASE_TAG.test(opts.tag)) throw new Error(`a release is a vX.Y.Z tag, got ${opts.tag}`);
  const [archive, sums] = await Promise.all([deps.download(`${base}/${asset}`), deps.download(`${base}/checksums.txt`)]);
  if (!archive) throw new Error(`${asset} isn't in this release`);
  if (!sums) throw new Error("this release has no checksums.txt; not installing");
  // 0. The release checksums.txt names (signed with it, below).
  checkTagLine(sums, opts.tag);
  // 1. Checksum.
  const expected = sums.toString("utf8").split("\n").map((l) => l.trim().split(/\s+/)).find(([, f]) => f === asset)?.[0];
  const actual = sha256(archive);
  if (!expected || expected !== actual) throw new Error(`checksum mismatch for ${asset}; not installing`);
  const verified: string[] = [];
  // 2. Build provenance.
  const att = await deps.attest(archive, asset, opts.tag);
  if (att.result === "failed") throw new Error(`build provenance check FAILED for ${asset} (not built by the release workflow from tag ${opts.tag}); not installing${att.detail ? ` (${att.detail.trim().split("\n")[0]})` : ""}. Releases up to v0.4.x predate provenance and have no attestation.`);
  if (att.result === "ok") verified.push(`build provenance (GitHub attestation, ${REPO} release workflow, tag ${opts.tag})`);
  // 3. Release signature, against the pinned key id (never one taken from the download). Required
  // once a key id is known.
  if (opts.releaseKeyId) {
    const sigFile = await deps.download(`${base}/checksums.txt.sig`);
    if (!sigFile) {
      throw new Error(`this release has no checksums.txt.sig, but releases are signed offline with the release key ${opts.releaseKeyId.slice(0, 16)}…; not installing. An unsigned release is what a compromised GitHub account or release workflow would publish.`);
    }
    let sig: ReleaseSignature;
    try {
      sig = JSON.parse(sigFile.toString("utf8"));
    } catch {
      throw new Error("checksums.txt.sig isn't valid JSON; not installing");
    }
    try {
      verifyReleaseSignature(sig, new Uint8Array(sums), opts.releaseKeyId);
    } catch (e) {
      throw new Error(`checksums.txt signature check FAILED: ${(e as Error).message}; not installing`);
    }
    verified.push(`release signature (ML-DSA-65 key ${opts.releaseKeyId.slice(0, 16)}…)`);
    if (opts.requireVerify && att.result !== "ok") {
      throw new Error("BLOCKULA_REQUIRE_VERIFY=1 requires build provenance as well as the release signature, and provenance couldn't be checked (log in to the GitHub CLI with `gh auth login`); not installing");
    }
  }
  if (verified.length) return { archive, sha256: actual, verified };
  if (opts.requireVerify) throw new Error("BLOCKULA_REQUIRE_VERIFY=1 and only the checksum could be verified (log in to the GitHub CLI with `gh auth login`, or set the release key id); not installing");
  return {
    archive,
    sha256: actual,
    verified,
    warning:
      "ONLY THE CHECKSUM WAS VERIFIED. That proves the download wasn't corrupted, not that this release is genuine: anyone who can change the release can change checksums.txt too. " +
      "To verify it, log in to the GitHub CLI (gh auth login) and install again, or check it by hand (docs/OPERATIONS.md \"Releases\"). Set BLOCKULA_REQUIRE_VERIFY=1 to refuse unverified installs. Don't run a validator on an unverified binary.",
  };
}
