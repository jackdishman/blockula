//! `blockula release keyid|sign|verify|install|fetch|pin|selftest`: offline ML-DSA-65 signatures
//! on release checksums, and installing releases checked against them (docs/OPERATIONS.md
//! "Releases"). The signature format is `scripts/release-sig.mjs`'s, and both reproduce the same
//! known-answer vector:
//!
//! * message: `"BLOCKULA_RELEASE_V1"` followed by the exact bytes of the file (`checksums.txt`);
//! * signature: FIPS 204 ML-DSA-65, empty context, deterministic (`crypto::Keys::sign`);
//! * key id: hex BLAKE3 of the public key;
//! * `.sig` file: one JSON line `{"format","key_id","public_key","signature"}` (hex).
//!
//! A release's `checksums.txt` names its tag on its first line, `# blockula-release vX.Y.Z`
//! (`scripts/release-checksums.sh`), so the signature binds the files to that version; `verify
//! --tag` checks the line, and an older release's genuine signature can't pass for a newer one
//! (audit supply-chain-B-2).
//!
//! `install` is how operators install and upgrade (audit F49, round 3): it downloads
//! `checksums.txt` and its signature, checks them against the release key id pinned at onboarding
//! (`~/.blockula/release-key-id`, [`PIN_FILE`]), checks the archive's SHA-256 against them,
//! extracts the binary, checks its `--version` and installs it. It never runs the release's
//! `install.sh`, so nothing the release ships is trusted before the signature is. `fetch` keeps
//! other files of a release (the genesis file) only if they match; `pin` moves the pin to a new
//! key only with a rotation statement signed by the pinned one.
//!
//! `scripts/install.sh` runs `blockula release verify` (from an already-installed binary) to
//! check a new release before installing it. Signing happens only here, with this binary, on the
//! offline machine (docs/OPERATIONS.md "Releases"): `scripts/release-sig.mjs` verifies, it never
//! loads a key (audit supply-chain-B-4).

use crate::crypto::{self, Keys};
use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const FORMAT: &str = "blockula-release-signature-v1";
const DOMAIN: &[u8] = b"BLOCKULA_RELEASE_V1";

/// The repository releases come from by default, and whose release workflow `--provenance` asks
/// GitHub's attestations about.
pub const REPO: &str = "jackdishman/blockula";

/// The release key id this binary trusts when nothing is pinned and no `--key-id` is given (it's
/// then pinned). Empty until the key ceremony fills it in, with `scripts/install.sh`'s
/// `BUILTIN_KEY_ID` (docs/OPERATIONS.md "The release key"). A pin always wins over it.
pub const BUILTIN_KEY_ID: &str = "";

/// The release key id pinned at onboarding, in the blockula directory: `~/.blockula/release-key-id`
/// (`$BLOCKULA_HOME/release-key-id`). One line, 64 hex characters.
pub const PIN_FILE: &str = "release-key-id";

/// The first line of a release key rotation statement: this line, `old <key id>`, `new <key id>`,
/// signed (`blockula release sign`) by the old key.
pub const ROTATION_HEADER: &str = "# blockula-release-key-rotation";

/// Size limits on what a release source may send: a hostile mirror can't fill the disk or memory.
const CHECKSUMS_CAP: u64 = 1 << 20;
const SIG_CAP: u64 = 64 << 10;
const ARCHIVE_CAP: u64 = 512 << 20;
const FILE_CAP: u64 = 64 << 20;

#[derive(Subcommand)]
pub enum ReleaseCmd {
    /// Print the release key id (hex BLAKE3 of the public key) of a key file
    Keyid {
        /// `blockula keygen` JSON, or a bare 32-byte hex seed
        #[arg(long)]
        key: PathBuf,
    },
    /// Sign a file (checksums.txt) on the offline machine; prints the .sig JSON line
    Sign {
        /// `blockula keygen` JSON, or a bare 32-byte hex seed
        #[arg(long)]
        key: PathBuf,
        file: PathBuf,
    },
    /// Verify a file's signature against the published release key id
    Verify {
        /// The release key id to trust (64 hex chars, published in SECURITY.md and OPERATIONS.md)
        /// [default: the pinned one, ~/.blockula/release-key-id]
        #[arg(long = "key-id")]
        key_id: Option<String>,
        /// The pin file to read when no --key-id is given [default: ~/.blockula/release-key-id]
        #[arg(long = "pin-file")]
        pin_file: Option<PathBuf>,
        /// The release (vX.Y.Z) the file must name on its first line, as checksums.txt does
        /// ("# blockula-release vX.Y.Z"), so another release's signed checksums can't pass for it
        #[arg(long)]
        tag: Option<String>,
        /// The signature file (or give it after the file)
        #[arg(long)]
        sig: Option<PathBuf>,
        file: PathBuf,
        sig_file: Option<PathBuf>,
    },
    /// Download a release, check it against the pinned release key id, and install its binary.
    /// Never runs the release's install.sh (docs/OPERATIONS.md "Installing and upgrading")
    Install {
        /// The release to install, vX.Y.Z
        tag: String,
        #[command(flatten)]
        src: SourceArgs,
        /// Where to install the binary [default: $BLOCKULA_INSTALL_DIR/blockula, else
        /// ~/.blockula/bin/blockula]
        #[arg(long)]
        to: Option<PathBuf>,
        /// Also require build provenance from the tag (`gh attestation verify`, a logged-in GitHub
        /// CLI). A patch distributed privately has none: install it without this flag
        #[arg(long)]
        provenance: bool,
    },
    /// Download files of a release into a new directory, keeping them only if they match its
    /// signed checksums.txt (e.g. --file genesis-blockula-1.json)
    Fetch {
        /// The release, vX.Y.Z
        tag: String,
        #[command(flatten)]
        src: SourceArgs,
        /// A new (or empty) directory for the verified files
        #[arg(long)]
        dir: PathBuf,
        /// A file of the release (repeatable) [default: this machine's archive]
        #[arg(long = "file")]
        files: Vec<String>,
    },
    /// Show the pinned release key id, pin one, or move the pin to a new key with a rotation
    /// statement signed by the pinned key
    Pin {
        /// The key id to pin (the first time), or the new one a rotation statement names
        #[arg(long = "key-id")]
        key_id: Option<String>,
        /// A rotation statement ("# blockula-release-key-rotation", "old <id>", "new <id>")
        #[arg(long, requires = "rotation_sig")]
        rotation: Option<PathBuf>,
        /// Its signature by the pinned key (`blockula release sign --key <old key> rotation.txt`)
        #[arg(long = "rotation-sig", requires = "rotation")]
        rotation_sig: Option<PathBuf>,
        /// The pin file [default: ~/.blockula/release-key-id]
        #[arg(long = "pin-file")]
        pin_file: Option<PathBuf>,
    },
    /// Check this binary against the known-answer vector scripts/release-sig.mjs also passes
    Selftest,
}

/// Where a release comes from, and the key it must be signed by.
#[derive(Args, Clone, Default)]
pub struct SourceArgs {
    /// The release key id to trust (64 hex), from SECURITY.md, the website and your operators'
    /// channel, never from the release. Pinned the first time; after that it must be the pinned
    /// one (a new key takes `blockula release pin --rotation`) [default: the pinned key id]
    #[arg(long = "key-id")]
    pub key_id: Option<String>,
    /// Where the release's files are: an https:// URL, a file:// URL or a directory [default:
    /// $BLOCKULA_RELEASE_URL, else https://github.com/jackdishman/blockula/releases/download/<tag>]
    #[arg(long)]
    pub from: Option<String>,
    /// The pin file [default: ~/.blockula/release-key-id]
    #[arg(long = "pin-file")]
    pub pin_file: Option<PathBuf>,
}

/// A `.sig` file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSignature {
    pub format: String,
    pub key_id: String,
    pub public_key: String,
    pub signature: String,
}

fn message(file: &[u8]) -> Vec<u8> {
    [DOMAIN, file].concat()
}

fn key_id(public_key: &[u8]) -> String {
    hex::encode(crypto::hash(public_key))
}

/// A key file: `blockula keygen` JSON (whose `address`, if present, must match the seed) or a
/// bare hex seed.
fn load_key(path: &Path) -> Result<Keys> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let text = text.trim();
    let (seed, address) = if text.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(text).with_context(|| format!("{} isn't valid JSON", path.display()))?;
        let seed = v["seed"].as_str().ok_or_else(|| anyhow!("{} has no \"seed\" field (blockula keygen output)", path.display()))?.to_string();
        (seed, v["address"].as_str().map(String::from))
    } else {
        (text.to_string(), None)
    };
    let seed: [u8; 32] = hex::decode(&seed).ok().and_then(|s| s.try_into().ok()).ok_or_else(|| anyhow!("{} must hold a 32-byte hex seed", path.display()))?;
    let keys = Keys::from_seed(seed);
    if let Some(address) = address {
        let (_, bytes) = crypto::parse_address(&address).ok_or_else(|| anyhow!("{} has an invalid address {address}", path.display()))?;
        ensure!(bytes == crypto::hash(&keys.public_key()), "{}: the seed doesn't match address {address}", path.display());
    }
    Ok(keys)
}

pub fn sign(keys: &Keys, file: &[u8]) -> ReleaseSignature {
    let public_key = keys.public_key();
    ReleaseSignature { format: FORMAT.into(), key_id: key_id(&public_key), public_key: hex::encode(&public_key), signature: hex::encode(keys.sign(&message(file))) }
}

/// `Ok` only if `sig` is a valid signature on `file` by the key whose id is `expected_key_id`.
pub fn verify(sig: &ReleaseSignature, file: &[u8], expected_key_id: &str) -> Result<()> {
    ensure!(sig.format == FORMAT, "unknown signature format {}", sig.format);
    let public_key = hex::decode(&sig.public_key).map_err(|_| anyhow!("public_key isn't hex"))?;
    let signature = hex::decode(&sig.signature).map_err(|_| anyhow!("signature isn't hex"))?;
    ensure!(public_key.len() == crypto::PUBLIC_KEY_LEN && signature.len() == crypto::SIGNATURE_LEN, "malformed ML-DSA-65 key or signature");
    let id = key_id(&public_key);
    ensure!(sig.key_id == id, "key_id doesn't match the public key");
    ensure!(expected_key_id.len() == 64 && crypto::is_lower_hex(expected_key_id), "pass the release key id to trust (--key-id, 64 lowercase hex chars)");
    ensure!(id == expected_key_id, "signed by key {id}, not the release key {expected_key_id}");
    crypto::verify(&public_key, &message(file), &signature).map_err(|_| anyhow!("INVALID signature"))
}

/// The first line of a release's `checksums.txt`, before the tag (`scripts/release-checksums.sh`).
pub const TAG_LINE_PREFIX: &str = "# blockula-release ";

fn is_release_tag(tag: &str) -> bool {
    tag.strip_prefix('v').is_some_and(|v| {
        let parts: Vec<&str> = v.split('.').collect();
        parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// `Ok` only if `file`'s first line names release `tag` (`# blockula-release vX.Y.Z`).
pub fn check_tag(file: &[u8], tag: &str) -> Result<()> {
    ensure!(is_release_tag(tag), "a release tag is vX.Y.Z, got {tag:?}");
    let first = file.split(|&b| b == b'\n').next().unwrap_or_default();
    match std::str::from_utf8(first).ok().and_then(|l| l.strip_prefix(TAG_LINE_PREFIX)) {
        Some(named) if named == tag => Ok(()),
        Some(named) => bail!("it's for release {named:?}, not {tag}: another release's signed checksums"),
        None => bail!("it names no release (no \"{TAG_LINE_PREFIX}vX.Y.Z\" first line; releases before v0.5.0 have none), so it can't be bound to {tag}"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use commonware_cryptography::Hasher as _;
    hex::encode(commonware_cryptography::Sha256::hash(&[bytes]))
}

/// A release file name: no paths, nothing hidden, so it's safe in a URL and in a directory.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && name.len() <= 255 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// The files a release's `checksums.txt` lists after its tag line (`tag`'s), name → SHA-256 hex.
/// Strict: every other line is `<64 lowercase hex>  <file name>`, each name once.
pub fn parse_checksums(file: &[u8], tag: &str) -> Result<BTreeMap<String, String>> {
    check_tag(file, tag)?;
    let text = std::str::from_utf8(file).map_err(|_| anyhow!("it isn't UTF-8"))?;
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut files = BTreeMap::new();
    for (i, line) in body.split('\n').enumerate().skip(1) {
        let n = i + 1;
        let (hash, name) = line.split_once("  ").ok_or_else(|| anyhow!("line {n} isn't \"<sha256>  <file>\": {line:?}"))?;
        ensure!(hash.len() == 64 && crypto::is_lower_hex(hash), "line {n}: {hash:?} isn't a SHA-256");
        ensure!(is_file_name(name), "line {n}: {name:?} isn't a release file name");
        ensure!(files.insert(name.to_string(), hash.to_string()).is_none(), "line {n}: {name} is listed twice");
    }
    ensure!(!files.is_empty(), "it lists no files");
    Ok(files)
}

fn normalize_key_id(s: &str) -> Result<String> {
    let k = s.trim().to_ascii_lowercase();
    ensure!(k.len() == 64 && crypto::is_lower_hex(&k), "a release key id is 64 hex characters, got {s:?}");
    Ok(k)
}

fn default_pin_file() -> PathBuf {
    crate::cli_net::blockula_dir().join(PIN_FILE)
}

/// The pinned release key id, if any. A pin file that doesn't hold one is an error, never ignored,
/// and so is one that others could have rewritten (writable by group or others, like ssh's keys).
pub fn read_pin(path: &Path) -> Result<Option<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path)?.permissions().mode();
        ensure!(mode & 0o022 == 0, "{} is writable by others (mode {:o}), so it can't vouch for a release key: chmod 644 it once you've checked it holds your key id", path.display(), mode & 0o777);
    }
    normalize_key_id(&text).map(Some).with_context(|| format!("{} isn't a release key id pin: fix it or remove it", path.display()))
}

fn write_pin(path: &Path, key_id: &str) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("new-{}", std::process::id()));
    std::fs::write(&tmp, format!("{key_id}\n")).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// The release key a command trusts: the pinned one; or, with nothing pinned yet, the one given
/// (or built in), which is pinned once the release it checked has verified.
pub struct TrustedKey {
    pub id: String,
    origin: &'static str,
    pin_to: Option<PathBuf>,
}

impl TrustedKey {
    pub fn resolve(given: Option<&str>, pin_file: &Path) -> Result<Self> {
        let pinned = read_pin(pin_file)?;
        let given = given.map(normalize_key_id).transpose()?;
        match (pinned, given) {
            (Some(p), Some(g)) if p != g => bail!(
                "✗ the release key id pinned in {} is {p}, not {g}. A new release key is accepted only with a rotation statement signed by the pinned one: blockula release pin --key-id {g} --rotation rotation.txt --rotation-sig rotation.txt.sig (docs/OPERATIONS.md \"The release key\")",
                pin_file.display()
            ),
            (Some(p), _) => {
                if !BUILTIN_KEY_ID.is_empty() && BUILTIN_KEY_ID != p {
                    eprintln!("note: this blockula was built with release key {BUILTIN_KEY_ID}, but {} pins {p}; the pin is what's checked", pin_file.display());
                }
                Ok(Self { id: p, origin: "pinned", pin_to: None })
            }
            (None, Some(g)) => Ok(Self { id: g, origin: "given; pinned once this release verifies", pin_to: Some(pin_file.into()) }),
            (None, None) if !BUILTIN_KEY_ID.is_empty() => Ok(Self { id: BUILTIN_KEY_ID.into(), origin: "built into this blockula; pinned once this release verifies", pin_to: Some(pin_file.into()) }),
            (None, None) => bail!(
                "no release key id is pinned ({}) and none was given: pass --key-id <release key id>, from SECURITY.md, the website and your operators' channel (never from the release). It's pinned the first time",
                pin_file.display()
            ),
        }
    }

    /// Pins the key if it wasn't pinned yet. Call it only once what it checked has passed.
    fn commit(&self) -> Result<()> {
        if let Some(path) = &self.pin_to {
            write_pin(path, &self.id)?;
            println!("📌 Pinned release key id {} in {}. From now on a release must be signed by it; a new key needs a rotation statement signed by this one.", self.id, path.display());
        }
        Ok(())
    }
}

/// A rotation statement's (old, new) key ids. Exactly three lines: [`ROTATION_HEADER`],
/// `old <key id>`, `new <key id>` (a final newline is optional). Its first line can't be a
/// release's (`# blockula-release vX.Y.Z`) or a checkpoint's, nor theirs its.
pub fn parse_rotation(file: &[u8]) -> Result<(String, String)> {
    let text = std::str::from_utf8(file).map_err(|_| anyhow!("it isn't UTF-8"))?;
    let lines: Vec<&str> = text.strip_suffix('\n').unwrap_or(text).split('\n').collect();
    ensure!(lines.len() == 3 && lines[0] == ROTATION_HEADER, "a rotation statement is three lines: \"{ROTATION_HEADER}\", \"old <key id>\", \"new <key id>\"");
    let id = |line: &str, label: &str| -> Result<String> {
        let k = line.strip_prefix(label).and_then(|k| k.strip_prefix(' ')).ok_or_else(|| anyhow!("expected \"{label} <key id>\", got {line:?}"))?;
        ensure!(k.len() == 64 && crypto::is_lower_hex(k), "{label}: {k:?} isn't a key id (64 lowercase hex)");
        Ok(k.to_string())
    };
    let (old, new) = (id(lines[1], "old")?, id(lines[2], "new")?);
    ensure!(old != new, "it rotates {old} to itself");
    Ok((old, new))
}

/// Where a release's files come from. None of it is trusted: everything is checked against the
/// signed checksums.txt.
enum Source {
    Dir(PathBuf),
    Http { base: String, client: reqwest::Client, rt: tokio::runtime::Runtime },
}

impl Source {
    fn new(from: Option<&str>, tag: &str) -> Result<Self> {
        let from = match from {
            Some(f) => f.to_string(),
            None => std::env::var("BLOCKULA_RELEASE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| format!("https://github.com/{REPO}/releases/download/{tag}")),
        };
        if let Some(path) = from.strip_prefix("file://") {
            ensure!(Path::new(path).is_dir(), "{from} isn't a directory");
            return Ok(Self::Dir(PathBuf::from(path)));
        }
        if from.starts_with("https://") || from.starts_with("http://") {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let client = reqwest::Client::builder()
                .user_agent(concat!("blockula/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(std::time::Duration::from_secs(30))
                .timeout(std::time::Duration::from_secs(30 * 60))
                .build()?;
            return Ok(Self::Http { base: from.trim_end_matches('/').to_string(), client, rt });
        }
        ensure!(Path::new(&from).is_dir(), "--from takes an https:// URL, a file:// URL or a directory, got {from:?}");
        Ok(Self::Dir(PathBuf::from(from)))
    }

    fn describe(&self) -> String {
        match self {
            Self::Dir(d) => d.display().to_string(),
            Self::Http { base, .. } => base.clone(),
        }
    }

    /// The file `name`, at most `cap` bytes; `None` if the source doesn't have it.
    fn get(&self, name: &str, cap: u64) -> Result<Option<Vec<u8>>> {
        ensure!(is_file_name(name), "{name:?} isn't a release file name");
        let too_big = || anyhow!("✗ {name} from {} is larger than {cap} bytes; refusing", self.describe());
        match self {
            Self::Dir(dir) => {
                let path = dir.join(name);
                let file = match std::fs::File::open(&path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    r => r.with_context(|| format!("reading {}", path.display()))?,
                };
                let mut bytes = Vec::new();
                file.take(cap + 1).read_to_end(&mut bytes).with_context(|| format!("reading {}", path.display()))?;
                ensure!(bytes.len() as u64 <= cap, too_big());
                Ok(Some(bytes))
            }
            Self::Http { base, client, rt } => rt.block_on(async {
                let url = format!("{base}/{name}");
                let mut res = client.get(&url).send().await.with_context(|| format!("downloading {url}"))?;
                if res.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                ensure!(res.status().is_success(), "downloading {url}: HTTP {}", res.status());
                ensure!(res.content_length().is_none_or(|n| n <= cap), too_big());
                let mut bytes = Vec::new();
                while let Some(chunk) = res.chunk().await.with_context(|| format!("downloading {url}"))? {
                    ensure!(bytes.len() as u64 + chunk.len() as u64 <= cap, too_big());
                    bytes.extend_from_slice(&chunk);
                }
                Ok(Some(bytes))
            }),
        }
    }
}

/// A release whose `checksums.txt` verified: signed by the trusted key, naming the tag asked for.
struct Release {
    tag: String,
    key: TrustedKey,
    checksums: Vec<u8>,
    sig: Vec<u8>,
    files: BTreeMap<String, String>,
    source: Source,
}

impl Release {
    fn open(tag: &str, args: &SourceArgs) -> Result<Self> {
        ensure!(is_release_tag(tag), "a release tag is vX.Y.Z, got {tag:?}");
        let key = TrustedKey::resolve(args.key_id.as_deref(), &args.pin_file.clone().unwrap_or_else(default_pin_file))?;
        let source = Source::new(args.from.as_deref(), tag)?;
        let checksums = source.get("checksums.txt", CHECKSUMS_CAP)?.ok_or_else(|| anyhow!("✗ {} has no checksums.txt; refusing", source.describe()))?;
        let sig = source.get("checksums.txt.sig", SIG_CAP)?.ok_or_else(|| {
            anyhow!("✗ this release has no checksums.txt.sig, but releases are signed offline with the release key; an unsigned release is what a compromised GitHub account or release workflow would publish. Refusing.")
        })?;
        let parsed: ReleaseSignature = serde_json::from_slice(&sig).map_err(|e| anyhow!("✗ checksums.txt.sig isn't a release signature ({e}); refusing"))?;
        verify(&parsed, &checksums, &key.id).map_err(|e| anyhow!("✗ checksums.txt: {e}; refusing"))?;
        let files = parse_checksums(&checksums, tag).map_err(|e| anyhow!("✗ checksums.txt: {e}; refusing"))?;
        println!("✓ checksums.txt is signed by release key {} ({}) and is release {tag}'s", key.id, key.origin);
        Ok(Self { tag: tag.into(), key, checksums, sig, files, source })
    }

    /// A file the signed checksums.txt lists, downloaded and checked against its SHA-256.
    fn file(&self, name: &str, cap: u64) -> Result<Vec<u8>> {
        let want = self.files.get(name).ok_or_else(|| anyhow!("✗ {name} isn't in release {}'s signed checksums.txt; refusing", self.tag))?;
        let bytes = self.source.get(name, cap)?.ok_or_else(|| anyhow!("✗ {} has no {name}", self.source.describe()))?;
        let got = sha256_hex(&bytes);
        ensure!(&got == want, "✗ {name} doesn't match the signed checksums.txt (SHA-256 {got}, signed {want}); refusing");
        println!("✓ {name} matches its signed SHA-256");
        Ok(bytes)
    }
}

/// This machine's release target (the archive is `blockula-<target>.tar.gz`).
pub fn host_target() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        (os, arch) => bail!("there's no prebuilt blockula for {os} {arch}: build from source (cargo build --release --locked --manifest-path node/Cargo.toml)"),
    })
}

fn default_install_path() -> PathBuf {
    match std::env::var_os("BLOCKULA_INSTALL_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("blockula"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".blockula/bin/blockula"),
    }
}

/// A private scratch directory, removed when dropped.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Result<Self> {
        let mut r = [0u8; 8];
        getrandom::fill(&mut r).map_err(|e| anyhow!("no randomness: {e}"))?;
        let dir = std::env::temp_dir().join(format!("blockula-release-{}", hex::encode(r)));
        std::fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `gh attestation verify`: `file` was built by this repo's release workflow from `tag`.
fn check_provenance(file: &Path, tag: &str) -> Result<()> {
    let out = Command::new("gh")
        .args(["attestation", "verify"])
        .arg(file)
        .args(["--repo", REPO, "--signer-workflow", &format!("{REPO}/.github/workflows/release.yml"), "--source-ref", &format!("refs/tags/{tag}")])
        .output();
    match out {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("✗ --provenance needs the GitHub CLI (gh), logged in (gh auth login); refusing"),
        Err(e) => Err(e).context("running gh attestation verify"),
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => bail!(
            "✗ build provenance check FAILED: {} wasn't built by {REPO}'s release workflow from {tag} ({}). Refusing. A patch distributed privately has no provenance: install it without --provenance (the release signature is its check)",
            file.file_name().unwrap_or_default().to_string_lossy(),
            String::from_utf8_lossy(&o.stderr).trim()
        ),
    }
}

/// Copies `bin` to `dest` atomically (a temporary file beside it, then a rename), mode 755.
fn install_binary(bin: &Path, dest: &Path) -> Result<()> {
    let name = dest.file_name().ok_or_else(|| anyhow!("--to must name a file, got {}", dest.display()))?;
    let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(".{}.new-{}", name.to_string_lossy(), std::process::id()));
    let r = (|| -> Result<()> {
        std::fs::copy(bin, &tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.with_context(|| format!("installing to {}", dest.display()))
}

/// `blockula release install`: the signature (pinned key), the tag line, the archive's hash, the
/// binary's version, then an atomic install. The release's install.sh is never fetched or run.
fn install(tag: &str, args: &SourceArgs, to: Option<PathBuf>, provenance: bool) -> Result<()> {
    let target = host_target()?;
    let release = Release::open(tag, args)?;
    let asset = format!("blockula-{target}.tar.gz");
    let archive = release.file(&asset, ARCHIVE_CAP)?;
    let work = WorkDir::new()?;
    let archive_path = work.0.join(&asset);
    std::fs::write(&archive_path, &archive).with_context(|| format!("writing {}", archive_path.display()))?;
    if provenance {
        check_provenance(&archive_path, tag)?;
        println!("✓ build provenance: {asset} was built by {REPO}'s release workflow from {tag}");
    }
    // The archive matched the signed hash, so this extracts signed bytes. Only the binary.
    let member = format!("blockula-{target}/blockula");
    let status = Command::new("tar").arg("-xzf").arg(&archive_path).arg("-C").arg(&work.0).arg(&member).status().context("running tar")?;
    ensure!(status.success(), "✗ couldn't extract {member} from {asset}");
    let bin = work.0.join(&member);
    ensure!(std::fs::symlink_metadata(&bin).is_ok_and(|m| m.file_type().is_file()), "✗ {member} in {asset} isn't a regular file; refusing");
    let want = format!("blockula {}", &tag[1..]);
    let got = Command::new(&bin).arg("--version").output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    ensure!(got == want, "✗ the binary in {asset} reports {}, not {want}; refusing", if got.is_empty() { "nothing (it doesn't run here)".to_string() } else { format!("{got:?}") });
    println!("✓ the binary reports {got}");
    let dest = to.unwrap_or_else(default_install_path);
    install_binary(&bin, &dest)?;
    release.key.commit()?;
    if !provenance {
        println!("· Not checked: build provenance (add --provenance, with a logged-in gh). The release signature is the check; a patch distributed privately has no provenance.");
    }
    println!("✓ Installed {want} to {} (the release's install.sh wasn't used)", dest.display());
    Ok(())
}

/// `blockula release fetch`: the named files, each matching the signed checksums.txt, into a new
/// directory (with checksums.txt and its signature), so it holds verified files only.
fn fetch(tag: &str, args: &SourceArgs, dir: &Path, files: &[String]) -> Result<()> {
    let mut names: Vec<String> = if files.is_empty() { vec![format!("blockula-{}.tar.gz", host_target()?)] } else { files.to_vec() };
    names.sort();
    names.dedup();
    for n in &names {
        ensure!(n != "checksums.txt" && n != "checksums.txt.sig", "{n} is always fetched (and verified); name the files it lists");
    }
    if dir.exists() {
        ensure!(dir.is_dir() && std::fs::read_dir(dir)?.next().is_none(), "--dir {} must be a new or empty directory, so everything in it is verified", dir.display());
    }
    let release = Release::open(tag, args)?;
    let mut got = Vec::new();
    for n in &names {
        got.push((n, release.file(n, if n.ends_with(".tar.gz") { ARCHIVE_CAP } else { FILE_CAP })?));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(dir.join("checksums.txt"), &release.checksums)?;
    std::fs::write(dir.join("checksums.txt.sig"), &release.sig)?;
    for (n, bytes) in got {
        std::fs::write(dir.join(n), bytes).with_context(|| format!("writing {}", dir.join(n).display()))?;
    }
    release.key.commit()?;
    println!("✓ {} verified in {}", names.join(", "), dir.display());
    Ok(())
}

/// `blockula release pin`.
fn pin(key_id: Option<String>, rotation: Option<(PathBuf, PathBuf)>, pin_file: Option<PathBuf>) -> Result<()> {
    let path = pin_file.unwrap_or_else(default_pin_file);
    let pinned = read_pin(&path)?;
    let key_id = key_id.as_deref().map(normalize_key_id).transpose()?;
    match (key_id, rotation) {
        (None, None) => match pinned {
            Some(p) => {
                println!("{p}");
                eprintln!("(the release key id pinned in {})", path.display());
            }
            None => bail!("no release key id is pinned in {}: blockula release pin --key-id <release key id>", path.display()),
        },
        (Some(k), None) => match pinned {
            None => {
                write_pin(&path, &k)?;
                println!("📌 Pinned release key id {k} in {}", path.display());
            }
            Some(p) if p == k => println!("✓ {k} is already the pinned release key id ({})", path.display()),
            Some(p) => bail!(
                "✗ the release key id pinned in {} is {p}. Moving the pin to {k} needs a rotation statement signed by {p}: --rotation rotation.txt --rotation-sig rotation.txt.sig (docs/OPERATIONS.md \"The release key\")",
                path.display()
            ),
        },
        (Some(k), Some((statement, sig_path))) => {
            let p = pinned.ok_or_else(|| anyhow!("nothing is pinned in {}, so no key can vouch for a rotation: pin a key id with --key-id alone", path.display()))?;
            let bytes = std::fs::read(&statement).with_context(|| format!("reading {}", statement.display()))?;
            let sig: ReleaseSignature = serde_json::from_slice(&std::fs::read(&sig_path).with_context(|| format!("reading {}", sig_path.display()))?).with_context(|| format!("{} isn't a release signature", sig_path.display()))?;
            verify(&sig, &bytes, &p).map_err(|e| anyhow!("✗ {}: {e} (a rotation statement must be signed by the pinned key); the pin is unchanged", statement.display()))?;
            let (old, new) = parse_rotation(&bytes).map_err(|e| anyhow!("✗ {}: {e}; the pin is unchanged", statement.display()))?;
            ensure!(old == p, "✗ the statement rotates away from {old}, not the pinned {p}; the pin is unchanged");
            ensure!(new == k, "✗ the statement names {new} as the new key, not {k}; the pin is unchanged");
            write_pin(&path, &k)?;
            println!("📌 The release key id pinned in {} is now {k}, by a rotation statement signed by {p}", path.display());
        }
        (None, Some(_)) => bail!("--rotation needs --key-id <the new key id the statement names>"),
    }
    Ok(())
}

/// Shared with `scripts/release-sig.mjs` (VECTOR): seed 07×32 signs this file.
pub const VECTOR_SEED: [u8; 32] = [7; 32];
pub const VECTOR_FILE: &str = "0123abcd  blockula-x86_64-unknown-linux-gnu.tar.gz\n";
pub const VECTOR_KEY_ID: &str = "b470b86b94891081659c41ca43129ca4fd136cd247ecbd0b9d3f037f5215fedd";
pub const VECTOR_SIGNATURE_BLAKE3: &str = "390d0f3d0c35684e1f9bf14044eaa58edb00ea8849c6eaecc14d782f2338b579";

/// The known-answer vector, then the rejections `release-sig.mjs selftest` checks.
pub fn selftest() -> Result<()> {
    let keys = Keys::from_seed(VECTOR_SEED);
    let file = VECTOR_FILE.as_bytes();
    let sig = sign(&keys, file);
    let got = hex::encode(crypto::hash(&hex::decode(&sig.signature)?));
    ensure!(sig.key_id == VECTOR_KEY_ID && got == VECTOR_SIGNATURE_BLAKE3, "known-answer mismatch: key id {}, signature BLAKE3 {got}", sig.key_id);
    verify(&sig, file, VECTOR_KEY_ID)?;
    let other = Keys::from_seed([0; 32]);
    let mut flipped = hex::decode(&sig.signature)?;
    flipped[100] ^= 1;
    let rejects: [(&str, Result<()>); 5] = [
        ("a modified file", verify(&sig, VECTOR_FILE.replace("0123", "0124").as_bytes(), VECTOR_KEY_ID)),
        ("another key", verify(&self::sign(&other, file), file, VECTOR_KEY_ID)),
        ("a swapped public key", verify(&ReleaseSignature { public_key: hex::encode(other.public_key()), ..sig.clone() }, file, VECTOR_KEY_ID)),
        ("a modified signature", verify(&ReleaseSignature { signature: hex::encode(&flipped), ..sig.clone() }, file, VECTOR_KEY_ID)),
        ("a tx-domain signature", verify(&ReleaseSignature { signature: hex::encode(keys.sign(file)), ..sig.clone() }, file, VECTOR_KEY_ID)),
    ];
    for (what, r) in rejects {
        if r.is_ok() {
            bail!("accepted {what}");
        }
    }
    Ok(())
}

pub fn run(cmd: ReleaseCmd) -> Result<()> {
    match cmd {
        ReleaseCmd::Keyid { key } => println!("{}", key_id(&load_key(&key)?.public_key())),
        ReleaseCmd::Sign { key, file } => {
            let keys = load_key(&key)?;
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let sig = sign(&keys, &bytes);
            verify(&sig, &bytes, &sig.key_id)?;
            println!("{}", serde_json::to_string(&sig)?);
            eprintln!("signed {} with key {}", file.display(), sig.key_id);
        }
        ReleaseCmd::Verify { key_id, pin_file, tag, sig, file, sig_file } => {
            let key_id = match key_id {
                Some(k) => k,
                None => {
                    let path = pin_file.unwrap_or_else(default_pin_file);
                    read_pin(&path)?.ok_or_else(|| anyhow!("pass --key-id <release key id>: none is pinned in {}", path.display()))?
                }
            };
            let sig_path = match (sig, sig_file) {
                (Some(s), None) | (None, Some(s)) => s,
                _ => bail!("give the signature file once: --sig <file.sig> <file>, or <file> <file.sig>"),
            };
            let sig: ReleaseSignature = serde_json::from_slice(&std::fs::read(&sig_path).with_context(|| format!("reading {}", sig_path.display()))?).with_context(|| format!("{} isn't a release signature", sig_path.display()))?;
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            verify(&sig, &bytes, &key_id).map_err(|e| anyhow!("✗ {}: {e}", file.display()))?;
            if let Some(tag) = &tag {
                check_tag(&bytes, tag).map_err(|e| anyhow!("✗ {}: {e}", file.display()))?;
            }
            match tag {
                Some(tag) => println!("✓ {} is signed by release key {key_id} and is release {tag}'s", file.display()),
                None => println!("✓ {} is signed by release key {key_id}", file.display()),
            }
        }
        ReleaseCmd::Install { tag, src, to, provenance } => install(&tag, &src, to, provenance)?,
        ReleaseCmd::Fetch { tag, src, dir, files } => fetch(&tag, &src, &dir, &files)?,
        ReleaseCmd::Pin { key_id, rotation, rotation_sig, pin_file } => pin(key_id, rotation.zip(rotation_sig), pin_file)?,
        ReleaseCmd::Selftest => {
            selftest()?;
            println!("✓ release signature selftest passed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// The vector `scripts/release-sig.mjs` checks (with @noble/post-quantum): the two
    /// implementations produce byte-identical signatures.
    #[test]
    fn known_answer_vector_and_rejections() {
        selftest().unwrap();
    }

    #[test]
    fn sign_and_verify_through_files() {
        let dir = std::env::temp_dir().join(format!("blockula-release-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let keys = Keys::from_seed([9; 32]);
        let json = dir.join("release-key.json");
        std::fs::write(&json, serde_json::json!({ "address": keys.address(), "seed": keys.seed_hex() }).to_string()).unwrap();
        let bare = dir.join("seed.hex");
        std::fs::write(&bare, keys.seed_hex()).unwrap();
        assert_eq!(load_key(&json).unwrap().public_key(), load_key(&bare).unwrap().public_key());
        // A keygen file whose address doesn't match its seed is refused.
        let wrong = dir.join("wrong.json");
        std::fs::write(&wrong, serde_json::json!({ "address": Keys::from_seed([1; 32]).address(), "seed": keys.seed_hex() }).to_string()).unwrap();
        assert!(load_key(&wrong).err().unwrap().to_string().contains("doesn't match"));

        let file = dir.join("checksums.txt");
        std::fs::write(&file, "abc  blockula.tar.gz\n").unwrap();
        let sig = sign(&keys, &std::fs::read(&file).unwrap());
        let line = serde_json::to_string(&sig).unwrap();
        let back: ReleaseSignature = serde_json::from_str(&line).unwrap();
        let id = key_id(&keys.public_key());
        verify(&back, &std::fs::read(&file).unwrap(), &id).unwrap();
        assert!(verify(&back, b"abc  blockula.tar.gz\nextra\n", &id).is_err());
        assert!(verify(&back, &std::fs::read(&file).unwrap(), &"00".repeat(32)).unwrap_err().to_string().contains("not the release key"));
        assert!(verify(&back, &std::fs::read(&file).unwrap(), "short").unwrap_err().to_string().contains("--key-id"));
        assert!(serde_json::from_str::<ReleaseSignature>(&line.replace("\"format\"", "\"extra\":1,\"format\"")).is_err(), "unknown fields");
        // The CLI paths.
        let sig_path = dir.join("checksums.txt.sig");
        std::fs::write(&sig_path, format!("{line}\n")).unwrap();
        run(ReleaseCmd::Verify { key_id: Some(id.clone()), pin_file: None, tag: None, sig: Some(sig_path.clone()), file: file.clone(), sig_file: None }).unwrap();
        run(ReleaseCmd::Verify { key_id: Some(id.clone()), pin_file: None, tag: None, sig: None, file: file.clone(), sig_file: Some(sig_path.clone()) }).unwrap();
        assert!(run(ReleaseCmd::Verify { key_id: Some(id.clone()), pin_file: None, tag: None, sig: None, file: file.clone(), sig_file: None }).is_err());
        // This checksums.txt names no release, so it can't be bound to one.
        let untagged = run(ReleaseCmd::Verify { key_id: Some(id), pin_file: None, tag: Some("v0.5.0".into()), sig: Some(sig_path), file: file.clone(), sig_file: None });
        assert!(untagged.unwrap_err().to_string().contains("names no release"));
        std::fs::remove_dir_all(dir).ok();
    }

    /// supply-chain-B-2: a release's genuinely signed checksums.txt can't pass for another
    /// release's. The tag line is signed with the rest, so it can't be edited either.
    #[test]
    fn verify_tag_binds_the_signed_checksums_to_a_release() {
        let dir = std::env::temp_dir().join(format!("blockula-release-tag-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let keys = Keys::from_seed([9; 32]);
        let id = key_id(&keys.public_key());
        let file = dir.join("checksums.txt");
        let sig_path = dir.join("checksums.txt.sig");
        let old = "# blockula-release v0.5.0\nabc  blockula-x86_64-unknown-linux-gnu.tar.gz\n";
        std::fs::write(&file, old).unwrap();
        std::fs::write(&sig_path, serde_json::to_string(&sign(&keys, old.as_bytes())).unwrap()).unwrap();
        let verify_as = |tag: &str| run(ReleaseCmd::Verify { key_id: Some(id.clone()), pin_file: None, tag: Some(tag.into()), sig: Some(sig_path.clone()), file: file.clone(), sig_file: None });
        verify_as("v0.5.0").unwrap();
        // The older release, replayed as the one asked for.
        assert!(verify_as("v0.5.1").unwrap_err().to_string().contains("for release \"v0.5.0\", not v0.5.1"));
        assert!(verify_as("0.5.0").unwrap_err().to_string().contains("vX.Y.Z"));
        assert!(verify_as("v0.5").is_err() && verify_as("v0.5.0.1").is_err() && verify_as("v0.5.x").is_err());
        // Relabelling it breaks the signature.
        std::fs::write(&file, old.replace("v0.5.0", "v0.5.1")).unwrap();
        assert!(verify_as("v0.5.1").unwrap_err().to_string().contains("INVALID signature"));
        // The line must be the first one.
        assert!(check_tag(b"abc  x.tar.gz\n# blockula-release v0.5.0\n", "v0.5.0").is_err());
        assert!(check_tag(b"# blockula-release v0.5.0", "v0.5.0").is_ok());
        assert!(check_tag(b"# blockula-release v0.5.0 \n", "v0.5.0").is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blockula-release-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_checksums(dir: &Path, tag: &str) {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with("checksums.txt") && n != "INSTALL_SH_RAN").collect();
        names.sort();
        let mut text = format!("{TAG_LINE_PREFIX}{tag}\n");
        for n in names {
            text += &format!("{}  {n}\n", sha256_hex(&std::fs::read(dir.join(&n)).unwrap()));
        }
        std::fs::write(dir.join("checksums.txt"), text).unwrap();
    }

    fn sign_into(dir: &Path, key: &Keys) {
        let checksums = std::fs::read(dir.join("checksums.txt")).unwrap();
        std::fs::write(dir.join("checksums.txt.sig"), serde_json::to_string(&sign(key, &checksums)).unwrap()).unwrap();
    }

    /// A release in `dir` as release.yml and the offline signer make it: this machine's archive
    /// (a `blockula` that reports `reports`), an install.sh that leaves a marker if anything runs
    /// it, a genesis file, and checksums.txt naming `tag`, signed by `key`. Returns the marker.
    fn fake_release(dir: &Path, tag: &str, key: &Keys, reports: &str) -> PathBuf {
        let target = host_target().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let stage = dir.with_extension("stage");
        let bin_dir = stage.join(format!("blockula-{target}"));
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("blockula"), format!("#!/bin/sh\necho \"{reports}\"\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(bin_dir.join("blockula"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let tar = Command::new("tar").arg("-czf").arg(dir.join(format!("blockula-{target}.tar.gz"))).arg("-C").arg(&stage).arg(format!("blockula-{target}")).status().unwrap();
        assert!(tar.success());
        let marker = dir.join("INSTALL_SH_RAN");
        std::fs::write(dir.join("install.sh"), format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::fs::write(dir.join("genesis-blockula-1.json"), "{\"chain_id\":\"blockula-1\"}\n").unwrap();
        write_checksums(dir, tag);
        sign_into(dir, key);
        marker
    }

    fn from_dir(dir: &Path, key: Option<&str>, pin: &Path) -> SourceArgs {
        SourceArgs { key_id: key.map(String::from), from: Some(format!("file://{}", dir.display())), pin_file: Some(pin.into()) }
    }

    fn reports(bin: &Path) -> Option<String> {
        Command::new(bin).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    /// Audit F49 (round 3): `release install` is the operator's install and upgrade. It trusts the
    /// key pinned at onboarding, not anything the release ships, and never runs install.sh.
    #[test]
    fn install_trusts_only_the_pinned_key_and_never_runs_install_sh() {
        let t = scratch("install");
        let (k, attacker) = (Keys::from_seed([3; 32]), Keys::from_seed([4; 32]));
        let (kid, aid) = (key_id(&k.public_key()), key_id(&attacker.public_key()));
        let pin = t.join("home/release-key-id");
        let dest = t.join("bin/blockula");
        let inst = |tag: &str, a: SourceArgs| install(tag, &a, Some(dest.clone()), false).map_err(|e| e.to_string());
        let asset = format!("blockula-{}.tar.gz", host_target().unwrap());

        let genuine = t.join("genuine");
        let marker = fake_release(&genuine, "v9.9.9", &k, "blockula 9.9.9");
        // Nothing pinned, no key id given (and none built in yet): nothing to check against.
        assert!(inst("v9.9.9", from_dir(&genuine, None, &pin)).unwrap_err().contains("no release key id is pinned"));
        // The first install pins the key id it was given, once the release verified.
        inst("v9.9.9", from_dir(&genuine, Some(&kid.to_uppercase()), &pin)).unwrap();
        assert_eq!(reports(&dest).as_deref(), Some("blockula 9.9.9"));
        assert_eq!(read_pin(&pin).unwrap(), Some(kid.clone()));
        assert!(!marker.exists(), "the release's install.sh ran");
        // Upgrades check against the pin.
        let next = t.join("next");
        fake_release(&next, "v9.9.10", &k, "blockula 9.9.10");
        inst("v9.9.10", from_dir(&next, None, &pin)).unwrap();
        inst("v9.9.10", from_dir(&next, Some(&kid), &pin)).unwrap();
        assert_eq!(reports(&dest).as_deref(), Some("blockula 9.9.10"));

        // A release signed by another key (a compromised GitHub side re-signing it): refused, and
        // passing its key id doesn't help: that's a new key, which takes a rotation statement.
        let evil = t.join("evil");
        let evil_marker = fake_release(&evil, "v9.9.11", &attacker, "blockula 9.9.11");
        assert!(inst("v9.9.11", from_dir(&evil, None, &pin)).unwrap_err().contains(&format!("signed by key {aid}, not the release key {kid}")));
        assert!(inst("v9.9.11", from_dir(&evil, Some(&aid), &pin)).unwrap_err().contains("rotation statement signed by the pinned one"));
        // An archive swapped under the genuine signature.
        let swapped = t.join("swapped");
        fake_release(&swapped, "v9.9.11", &k, "blockula 9.9.11");
        std::fs::OpenOptions::new().append(true).open(swapped.join(&asset)).unwrap().write_all(b"x").unwrap();
        assert!(inst("v9.9.11", from_dir(&swapped, None, &pin)).unwrap_err().contains(&format!("{asset} doesn't match the signed checksums.txt")));
        // An older, genuinely signed release served as the one asked for.
        assert!(inst("v9.9.12", from_dir(&next, None, &pin)).unwrap_err().contains("it's for release \"v9.9.10\", not v9.9.12"));
        // No signature, or a junk one.
        let unsigned = t.join("unsigned");
        fake_release(&unsigned, "v9.9.12", &k, "blockula 9.9.12");
        std::fs::remove_file(unsigned.join("checksums.txt.sig")).unwrap();
        assert!(inst("v9.9.12", from_dir(&unsigned, None, &pin)).unwrap_err().contains("no checksums.txt.sig"));
        std::fs::write(unsigned.join("checksums.txt.sig"), "{}").unwrap();
        assert!(inst("v9.9.12", from_dir(&unsigned, None, &pin)).unwrap_err().contains("isn't a release signature"));
        // A binary that isn't the version asked for.
        let liar = t.join("liar");
        fake_release(&liar, "v9.9.13", &k, "blockula 9.9.99");
        assert!(inst("v9.9.13", from_dir(&liar, None, &pin)).unwrap_err().contains("reports \"blockula 9.9.99\", not blockula 9.9.13"));
        // Not a release tag.
        assert!(inst("9.9.10", from_dir(&next, None, &pin)).unwrap_err().contains("vX.Y.Z"));
        // None of that moved the pin or the installed binary, or ran a release's install.sh.
        assert_eq!(read_pin(&pin).unwrap(), Some(kid.clone()));
        assert_eq!(reports(&dest).as_deref(), Some("blockula 9.9.10"));
        assert!(!evil_marker.exists() && !marker.exists());
        // A pin file others could rewrite vouches for nothing.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(inst("v9.9.10", from_dir(&next, None, &pin)).unwrap_err().contains("is writable by others"));
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        // A pin file that isn't one is an error, never "nothing pinned".
        std::fs::write(&pin, "not a key id\n").unwrap();
        assert!(inst("v9.9.10", from_dir(&next, Some(&kid), &pin)).unwrap_err().contains("isn't a release key id pin"));
        std::fs::remove_dir_all(t).ok();
    }

    /// The pin moves to a new key only with a rotation statement the pinned key signed.
    #[test]
    fn rotation_moves_the_pin_only_with_the_pinned_keys_signature() {
        let t = scratch("rotation");
        let (old, new, other) = (Keys::from_seed([5; 32]), Keys::from_seed([6; 32]), Keys::from_seed([7; 32]));
        let (old_id, new_id, other_id) = (key_id(&old.public_key()), key_id(&new.public_key()), key_id(&other.public_key()));
        let pin_file = t.join("release-key-id");
        let p = |k: &str, r: Option<(&Path, &Path)>| pin(Some(k.into()), r.map(|(a, b)| (a.to_path_buf(), b.to_path_buf())), Some(pin_file.clone())).map_err(|e| e.to_string());
        let statement = |from: &str, to: &str, by: &Keys, name: &str| -> (PathBuf, PathBuf) {
            let text = format!("{ROTATION_HEADER}\nold {from}\nnew {to}\n");
            let (f, s) = (t.join(name), t.join(format!("{name}.sig")));
            std::fs::write(&f, &text).unwrap();
            std::fs::write(&s, serde_json::to_string(&sign(by, text.as_bytes())).unwrap()).unwrap();
            (f, s)
        };
        // Nothing to rotate from yet.
        let (f, s) = statement(&old_id, &new_id, &old, "early.txt");
        assert!(p(&new_id, Some((&f, &s))).unwrap_err().contains("nothing is pinned"));
        p(&old_id, None).unwrap();
        p(&old_id, None).unwrap();
        assert!(p(&new_id, None).unwrap_err().contains("needs a rotation statement signed by"));
        // Signed by the new key itself (or anyone but the pinned key): refused.
        let (f, s) = statement(&old_id, &new_id, &new, "self.txt");
        assert!(p(&new_id, Some((&f, &s))).unwrap_err().contains("not the release key"));
        // Signed by the pinned key, but naming another new key, or rotating from another key.
        let (f, s) = statement(&old_id, &other_id, &old, "other.txt");
        assert!(p(&new_id, Some((&f, &s))).unwrap_err().contains("names"));
        let (f, s) = statement(&other_id, &new_id, &old, "from-other.txt");
        assert!(p(&new_id, Some((&f, &s))).unwrap_err().contains("rotates away from"));
        // A file the pinned key signed for another purpose (a release's checksums.txt) isn't one.
        let checksums = t.join("checksums.txt");
        std::fs::write(&checksums, format!("{TAG_LINE_PREFIX}v1.0.0\n{}  install.sh\n", "a".repeat(64))).unwrap();
        std::fs::write(t.join("checksums.txt.sig"), serde_json::to_string(&sign(&old, &std::fs::read(&checksums).unwrap())).unwrap()).unwrap();
        assert!(p(&new_id, Some((&checksums, &t.join("checksums.txt.sig")))).unwrap_err().contains("three lines"));
        assert_eq!(read_pin(&pin_file).unwrap(), Some(old_id.clone()));
        // The genuine statement.
        let (f, s) = statement(&old_id, &new_id, &old, "rotation.txt");
        p(&new_id, Some((&f, &s))).unwrap();
        assert_eq!(read_pin(&pin_file).unwrap(), Some(new_id.clone()));
        // ...which can't pass for a release.
        assert!(check_tag(&std::fs::read(&f).unwrap(), "v1.0.0").is_err());
        // Replaying it now does nothing: the pinned key is the new one.
        assert!(p(&new_id, Some((&f, &s))).unwrap_err().contains("not the release key"));
        assert!(parse_rotation(format!("{ROTATION_HEADER}\nold {old_id}\nnew {old_id}\n").as_bytes()).is_err());
        assert!(parse_rotation(format!("{ROTATION_HEADER}\nold {old_id}\nnew {new_id}\nextra\n").as_bytes()).is_err());
        assert!(parse_rotation(format!("{ROTATION_HEADER}\nnew {new_id}\nold {old_id}\n").as_bytes()).is_err());
        std::fs::remove_dir_all(t).ok();
    }

    #[test]
    fn checksums_parse_strictly() {
        let h = "ab".repeat(32);
        let ok = format!("{TAG_LINE_PREFIX}v1.2.3\n{h}  blockula-x86_64-unknown-linux-gnu.tar.gz\n{h}  install.sh\n");
        let files = parse_checksums(ok.as_bytes(), "v1.2.3").unwrap();
        assert_eq!(files.len(), 2);
        assert!(parse_checksums(ok.trim_end().as_bytes(), "v1.2.3").is_ok());
        assert!(parse_checksums(ok.as_bytes(), "v1.2.4").is_err());
        for bad in [
            format!("{TAG_LINE_PREFIX}v1.2.3\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h}\tinstall.sh\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h} install.sh\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{}  install.sh\n", h.to_uppercase()),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h}  ../install.sh\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h}  dir/install.sh\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h}  .hidden\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h}  install.sh\n{h}  install.sh\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h}  install.sh\r\n"),
            format!("{TAG_LINE_PREFIX}v1.2.3\n{h}  install.sh\n\n{h}  x.json\n"),
            format!("{h}  install.sh\n"),
        ] {
            assert!(parse_checksums(bad.as_bytes(), "v1.2.3").is_err(), "accepted {bad:?}");
        }
    }

    /// `release fetch` leaves only verified files (the genesis file the validator runbook uses).
    #[test]
    fn fetch_keeps_only_verified_files() {
        let t = scratch("fetch");
        let k = Keys::from_seed([8; 32]);
        let kid = key_id(&k.public_key());
        let pin = t.join("release-key-id");
        let rel = t.join("rel");
        fake_release(&rel, "v9.9.9", &k, "blockula 9.9.9");
        let files = vec!["genesis-blockula-1.json".to_string(), "install.sh".to_string()];
        let out = t.join("verified");
        fetch("v9.9.9", &from_dir(&rel, Some(&kid), &pin), &out, &files).unwrap();
        for f in files.iter().map(String::as_str).chain(["checksums.txt", "checksums.txt.sig"]) {
            assert_eq!(std::fs::read(out.join(f)).unwrap(), std::fs::read(rel.join(f)).unwrap(), "{f}");
        }
        assert_eq!(read_pin(&pin).unwrap(), Some(kid.clone()));
        // Only into a new or empty directory.
        assert!(fetch("v9.9.9", &from_dir(&rel, None, &pin), &out, &files).unwrap_err().to_string().contains("new or empty directory"));
        // A tampered file leaves nothing behind.
        std::fs::write(rel.join("genesis-blockula-1.json"), "{\"chain_id\":\"evil\"}\n").unwrap();
        let out2 = t.join("verified2");
        assert!(fetch("v9.9.9", &from_dir(&rel, None, &pin), &out2, &files).unwrap_err().to_string().contains("doesn't match the signed checksums.txt"));
        assert!(!out2.exists());
        // A file the release doesn't list.
        assert!(fetch("v9.9.9", &from_dir(&rel, None, &pin), &out2, &["genesis-blockula-2.json".into()]).unwrap_err().to_string().contains("isn't in release v9.9.9's signed checksums.txt"));
        assert!(fetch("v9.9.9", &from_dir(&rel, None, &pin), &out2, &["../checksums.txt".into()]).is_err());
        std::fs::remove_dir_all(t).ok();
    }

    /// Over HTTP, as from GitHub (whose download URLs redirect): the same checks, and a source
    /// can't send more than the limit.
    #[test]
    fn install_over_http_follows_redirects_and_caps_sizes() {
        let t = scratch("http");
        let k = Keys::from_seed([9; 32]);
        let kid = key_id(&k.public_key());
        let rel = t.join("rel");
        fake_release(&rel, "v9.9.9", &k, "blockula 9.9.9");
        let root = rel.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                use axum::extract::Path as P;
                let app = axum::Router::new()
                    .route("/download/{name}", axum::routing::get(|P(name): P<String>| async move { axum::response::Redirect::temporary(&format!("/objects/{name}")) }))
                    .route("/objects/{name}", axum::routing::get(move |P(name): P<String>| { let root = root.clone(); async move { std::fs::read(root.join(name)).map_err(|_| axum::http::StatusCode::NOT_FOUND) } }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        let base = format!("http://{}/download", rx.recv().unwrap());
        let pin = t.join("release-key-id");
        let dest = t.join("bin/blockula");
        let args = SourceArgs { key_id: Some(kid.clone()), from: Some(base.clone()), pin_file: Some(pin.clone()) };
        install("v9.9.9", &args, Some(dest.clone()), false).unwrap();
        assert_eq!(reports(&dest).as_deref(), Some("blockula 9.9.9"));
        let source = Source::new(Some(&base), "v9.9.9").unwrap();
        assert!(source.get("install.sh", 8).unwrap_err().to_string().contains("larger than 8 bytes"));
        assert!(source.get("missing.txt", 8).unwrap().is_none());
        std::fs::remove_file(rel.join("checksums.txt.sig")).unwrap();
        assert!(install("v9.9.9", &args, Some(dest.clone()), false).unwrap_err().to_string().contains("no checksums.txt.sig"));
        std::fs::remove_dir_all(t).ok();
    }

    /// The built-in key id (filled in at the key ceremony) is a key id, and the same as
    /// install.sh's.
    #[test]
    fn built_in_key_id_matches_install_sh() {
        assert!(BUILTIN_KEY_ID.is_empty() || normalize_key_id(BUILTIN_KEY_ID).ok().as_deref() == Some(BUILTIN_KEY_ID));
        let script = include_str!("../../scripts/install.sh");
        let line = script.lines().find(|l| l.starts_with("BUILTIN_KEY_ID=\"")).expect("install.sh has a BUILTIN_KEY_ID line");
        assert_eq!(line, format!("BUILTIN_KEY_ID=\"{BUILTIN_KEY_ID}\""), "install.sh and release_cli.rs must build in the same release key id");
    }
}
