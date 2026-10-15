// install_node's checks on a release download, the same as scripts/install.sh (docs/OPERATIONS.md
// "Releases"):
//   1. SHA-256 against the release's checksums.txt. Always. It only catches a corrupted download:
//      whoever can change the release can change checksums.txt too.
//   2. SLSA build provenance with a logged-in GitHub CLI (`gh attestation verify`): the archive was
//      built by this repo's release workflow. A failed check is fatal; no gh skips it.
//   3. The offline ML-DSA-65 signature on checksums.txt (checksums.txt.sig), verified with the
//      SDK against the pinned release key id. A failed check is fatal; no key id or no .sig skips it.
// If neither 2 nor 3 ran, the result says so loudly, and BLOCKULA_REQUIRE_VERIFY=1 refuses.
import { createHash } from "node:crypto";
import { verifyReleaseSignature, type ReleaseSignature } from "@blockula/sdk";

export const REPO = "jackdishman/blockula";
/** BLAKE3 of the release public key (docs/OPERATIONS.md "Releases"). Empty until the key ceremony; BLOCKULA_RELEASE_KEY_ID sets it meanwhile. */
export const RELEASE_KEY_ID = "";

export interface ReleaseDeps {
  /** GET a release file; `null` if it doesn't exist (404). */
  download(url: string): Promise<Buffer | null>;
  /**
   * Runs `gh attestation verify` on the archive's bytes: `"ok"`, `"failed"` (with the reason), or
   * `"unavailable"` (no gh, or not logged in).
   */
  attest(archive: Buffer, name: string): Promise<{ result: "ok" | "failed" | "unavailable"; detail?: string }>;
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

/** Downloads `asset` from `base` (a release download URL) and checks it as described above. Throws instead of returning anything unverified that must not be installed. */
export async function fetchVerifiedRelease(base: string, asset: string, opts: { releaseKeyId: string; requireVerify: boolean }, deps: ReleaseDeps): Promise<VerifiedRelease> {
  const [archive, sums] = await Promise.all([deps.download(`${base}/${asset}`), deps.download(`${base}/checksums.txt`)]);
  if (!archive) throw new Error(`${asset} isn't in this release`);
  if (!sums) throw new Error("this release has no checksums.txt; not installing");
  // 1. Checksum.
  const expected = sums.toString("utf8").split("\n").map((l) => l.trim().split(/\s+/)).find(([, f]) => f === asset)?.[0];
  const actual = sha256(archive);
  if (!expected || expected !== actual) throw new Error(`checksum mismatch for ${asset}; not installing`);
  const verified: string[] = [];
  // 2. Build provenance.
  const att = await deps.attest(archive, asset);
  if (att.result === "failed") throw new Error(`build provenance check FAILED for ${asset}; not installing${att.detail ? ` (${att.detail.trim().split("\n")[0]})` : ""}. Releases up to v0.4.x predate provenance and have no attestation.`);
  if (att.result === "ok") verified.push(`build provenance (GitHub attestation, ${REPO} release workflow)`);
  // 3. Release signature, against the pinned key id (never one taken from the download).
  if (opts.releaseKeyId) {
    const sigFile = await deps.download(`${base}/checksums.txt.sig`);
    if (sigFile) {
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
