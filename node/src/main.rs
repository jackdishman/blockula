// `/api/status` and friends are large `json!` literals.
#![recursion_limit = "256"]

mod api;
mod beacon;
mod chain;
mod checkpoints;
mod cli_net;
mod consensus;
mod contrib_cli;
mod crypto;
mod econ;
mod epochs;
mod genesis_builder;
mod ghost;
mod github;
mod gov_cli;
mod governance;
mod snapshot;
mod staking;
mod state;
mod store;
mod node;
mod params;
mod pq;
mod poc;
mod protection;
mod release_cli;
mod status_cli;
mod types;
mod upgrade;
mod validator_cli;
mod view;

#[cfg(test)]
mod sim;
#[cfg(test)] mod kat;

use anyhow::Result;
use chain::{Chain, SummonOpts};
use clap::{Args, Parser, Subcommand};
use cli_net::blockula_dir;
use node::NetOpts;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "blockula", version, about = "🧛 Blockula — a post-quantum blockchain that never dies")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Net {
    /// Interface to listen on for HTTP and p2p: 127.0.0.1 (default), 0.0.0.0 or :: for servers
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
    /// IP of a proxy or full node whose `X-Blockula-Client-IP` header is trusted for faucet/tx
    /// rate limits (repeatable)
    #[arg(long = "trusted-forwarder")]
    trusted_forwarders: Vec<std::net::IpAddr>,
    /// Reverse-proxy header holding the real client IP (e.g. `fly-client-ip`, `x-real-ip`). Only
    /// set this when every request reaches the node through that proxy; otherwise clients can
    /// spoof it. A request without it is limited as the proxy's own IP
    #[arg(long)]
    client_ip_header: Option<String>,
    /// Proxies that append to --client-ip-header (`x-forwarded-for`): the client is this many
    /// entries from the right, counted over every line of the header in order (entries further
    /// left are the client's own say)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..=16))]
    client_ip_hops: u64,
    /// HTTP peer to sync from and forward txs/faucet requests to (repeatable). Replaces the
    /// genesis validators' http addresses entirely (e.g. http://blockula-v0.internal:7070).
    #[arg(long = "peer-http")]
    peer_http: Vec<String>,
    /// Skip the double-signing guards (missing consensus journal, machine.lock mismatch) and
    /// clear a recorded slashing-protection stop (double-sign-halt.json). Only after you've
    /// proven no other copy of this validator.key is running.
    #[arg(long = "i-know-this-key-is-not-running-elsewhere")]
    force_key: bool,
    /// Replay the whole chain from genesis instead of fast-syncing from a certified snapshot
    #[arg(long)]
    full: bool,
    /// Keep only the last N blocks (and their certificates) on disk; older log segments are
    /// deleted. Validators keep about two days by default; full nodes keep everything.
    #[arg(long, conflicts_with = "archive")]
    prune_blocks: Option<u64>,
    /// Keep every block (an archive node), also on a validator
    #[arg(long)]
    archive: bool,
    /// HTTP reads allowed per client IP per second (0: unlimited); loopback isn't limited.
    /// Behind a reverse proxy, set --client-ip-header too, or every client shares the proxy's budget
    #[arg(long, default_value_t = api::READ_RPS)]
    read_rps: u32,
    /// Burst of HTTP reads a client IP may make at once
    #[arg(long, default_value_t = api::READ_BURST)]
    read_burst: u32,
    /// Longest an HTTP request may take, in seconds
    #[arg(long, default_value_t = api::HTTP_TIMEOUT.as_secs())]
    http_timeout_secs: u64,
    /// Execute this height, then stop: validators exit (code 10) and don't vote past it, full
    /// nodes keep serving reads. For coordinated restarts and incidents (docs/OPERATIONS.md)
    #[arg(long)]
    halt_height: Option<u64>,
    /// The whole network is halted with its last block at this height, and you've checked: while
    /// this validator is at that height and no HTTP peer answers, slashing protection's restart
    /// rule may sign again after its escape time without a peer confirming it (docs/OPERATIONS.md).
    /// Applies at that height only; remove it once the network runs again
    #[arg(long)]
    network_halted_at: Option<u64>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a local single-validator dev chain with a faucet and the built-in explorer
    Dev {
        #[arg(long, default_value = "7070")]
        port: u16,
        /// Wipe the dev chain and start fresh
        #[arg(long)]
        reset: bool,
        #[command(flatten)]
        net: Net,
    },
    /// Create a brand-new chain of your own (genesis + validator keys)
    Summon {
        chain_id: String,
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        no_faucet: bool,
        #[arg(long, default_value = "1000")]
        block_time_ms: u64,
        /// Number of genesis validators. With N > 1, `home/v0 … home/vN-1` are created.
        #[arg(long, default_value = "1")]
        validators: usize,
        /// Season 0 validator admin account (repeatable). Default: the first validator's account.
        #[arg(long = "admin")]
        admins: Vec<String>,
        /// Blocks per epoch; the validator set changes only at epoch boundaries
        #[arg(long, default_value = "600")]
        epoch_blocks: u64,
        /// Minimum validator bond in FANG (default: 50 with a faucet, 10000 without)
        #[arg(long)]
        min_bond: Option<String>,
        /// Séance length in blocks (default: ~7 days)
        #[arg(long)]
        seance_blocks: Option<u64>,
        /// Blocks a verified claim waits before it's judged (default: ~30 days)
        #[arg(long)]
        claim_maturity_blocks: Option<u64>,
        /// Blocks contribution rewards vest over (default: ~90 days)
        #[arg(long)]
        vesting_blocks: Option<u64>,
        /// Address prefix: `boo` (mainnet) or `tboo` (public testnets)
        #[arg(long, default_value = "boo")]
        address_prefix: String,
        /// `blockula dev`'s short durations (`ChainParams::dev_overrides`): unbonding in a few
        /// minutes, votes in a minute, a 20-second timelock, a 10 FANG proposal deposit, spends
        /// open at once. For local multi-validator chains that exercise those flows end to end.
        #[arg(long)]
        dev_durations: bool,
    },
    /// Run a validator (a home with validator.key) or a joined full node
    Run {
        #[arg(long)]
        home: PathBuf,
        /// HTTP port (default: from the advertised http_addr for validators, 7171 for full nodes)
        #[arg(long)]
        port: Option<u16>,
        /// p2p preset: `local` (private IPs allowed) or `recommended` (public networks).
        /// Default: local if this validator's advertised p2p address is private.
        #[arg(long)]
        p2p_preset: Option<String>,
        #[command(flatten)]
        net: Net,
    },
    /// Join an existing chain as a full node: sync finalized blocks from validators and serve the API
    Join {
        /// genesis.json path, or a validator's HTTP URL (e.g. http://127.0.0.1:7070)
        #[arg(long)]
        genesis: String,
        #[arg(long)]
        home: PathBuf,
        #[arg(long, default_value = "7171")]
        port: u16,
        /// Refuse to join unless genesis hashes to this (printed by `summon`, in /api/status).
        /// Required unless this release knows the chain id
        #[arg(long)]
        genesis_hash: Option<String>,
        /// Trusted checkpoint HEIGHT:HASH[:SET_HASH[:TIMESTAMP_MS]] (repeatable): snapshots and
        /// blocks that contradict it are refused, and its age counts from its block's time. Get
        /// one from sources you trust (`blockula checkpoint`)
        #[arg(long = "checkpoint", value_parser = checkpoints::Checkpoint::parse)]
        checkpoints: Vec<checkpoints::Checkpoint>,
        /// Sync even when the newest trusted checkpoint is older than the weak-subjectivity
        /// period (OPERATIONS.md "Weak subjectivity")
        #[arg(long)]
        allow_stale_checkpoint: bool,
        #[command(flatten)]
        net: Net,
    },
    /// Print a checkpoint (HEIGHT:HASH:SET_HASH:TIMESTAMP_MS) that several nodes agree on, for
    /// `join --checkpoint` and node/checkpoints/*.json; or, with --genesis, a genesis file's hash
    Checkpoint {
        /// A node's HTTP URL (repeat it: ask nodes run by independent operators)
        #[arg(long = "peer", required_unless_present = "genesis")]
        peers: Vec<String>,
        /// Height (default: the newest epoch boundary every peer has finalized)
        #[arg(long)]
        height: Option<u64>,
        /// Print the genesis hash of this genesis.json instead
        #[arg(long, conflicts_with_all = ["peers", "height"])]
        genesis: Option<PathBuf>,
        /// Print the JSON entry for node/checkpoints/<chain id>.json
        #[arg(long)]
        json: bool,
        /// Refuse unless the peers serve this genesis (hex): for a node whose genesis you hold
        #[arg(long, conflicts_with = "genesis")]
        genesis_hash: Option<String>,
    },
    /// Print a node's own state as one line of JSON: its /api/status and what its /metrics say
    /// about it (its validator, process start, p2p peers, whether it signs). For scripts on the
    /// node's machine, which has no curl (deploy/restart-gate.sh)
    Status {
        /// The node's HTTP URL
        #[arg(long, default_value = "http://localhost:7070")]
        rpc: String,
    },
    /// A simple command-line wallet (key in ~/.blockula/wallet.json, mode 600)
    Wallet {
        #[command(subcommand)]
        cmd: validator_cli::WalletCmd,
    },
    /// 🏹 Hunts: bounties on GitHub issues (list, create, fund, award, cancel)
    Hunt {
        #[command(subcommand)]
        cmd: contrib_cli::HuntCmd,
    },
    /// 🪦 Proof of Contribution: claim merged PRs, vouch, sit on Séance juries, withdraw rewards
    Contrib {
        #[command(subcommand)]
        cmd: contrib_cli::ContribCmd,
    },
    /// 🏛️ Governance: propose, vote (with conviction), unlock, community distributions
    Gov {
        #[command(subcommand)]
        cmd: gov_cli::GovCmd,
    },
    /// M-of-N accounts: create, propose, approve, show
    Multisig {
        #[command(subcommand)]
        cmd: gov_cli::MultisigCmd,
    },
    /// 🪪 .boo names: show, renew, transfer, release
    Name {
        #[command(subcommand)]
        cmd: contrib_cli::NameCmd,
    },
    /// 🥩 Stake: delegate to validator pools, unbond, withdraw
    Stake {
        #[command(subcommand)]
        cmd: contrib_cli::StakeCmd,
    },
    /// Become (or manage) a validator
    Validator {
        #[command(subcommand)]
        cmd: validator_cli::ValidatorCmd,
    },
    /// The genesis ceremony: an operator's entry (offline), and a reproducible genesis.json
    Genesis {
        #[command(subcommand)]
        cmd: genesis_builder::GenesisCmd,
    },
    /// Generate an ML-DSA-65 keypair
    Keygen {
        /// Address prefix to print the address with: `boo` (mainnet) or `tboo` (public testnets)
        #[arg(long, default_value = "boo")]
        prefix: String,
    },
    /// Sign or verify release checksums with the offline ML-DSA-65 release key
    Release {
        #[command(subcommand)]
        cmd: release_cli::ReleaseCmd,
    },
}

fn net_opts(net: Net, http_port: Option<u16>, p2p_preset: Option<String>, ephemeral_p2p: bool) -> NetOpts {
    let client_ip = api::ClientIpPolicy {
        trusted_forwarders: net.trusted_forwarders,
        forwarder_secret: std::env::var("BLOCKULA_FORWARDER_SECRET").ok().filter(|s| s.len() >= 32),
        header: net.client_ip_header.map(|h| h.to_lowercase()),
        hops: net.client_ip_hops as usize,
    };
    let http = node::HttpOpts { read_rps: net.read_rps, read_burst: net.read_burst, timeout: std::time::Duration::from_secs(net.http_timeout_secs.max(1)) };
    let pruning = match (net.archive, net.prune_blocks) {
        (true, _) => node::Pruning::Archive,
        (_, Some(n)) => node::Pruning::Keep(n),
        _ => node::Pruning::Default,
    };
    NetOpts { bind: net.bind, http_port, p2p_preset, ephemeral_p2p, client_ip, peer_http: net.peer_http, force_key: net.force_key, full: net.full, pruning, http, halt_height: net.halt_height, network_halted_at: net.network_halted_at }
}

fn main() -> Result<()> {
    api::mark_process_start();
    // Outbound HTTPS uses rustls on aws-lc-rs, which offers the hybrid X25519MLKEM768 first.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,blockula=info")))
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().cmd {
        Cmd::Dev { port, reset, net } => {
            let home = blockula_dir().join("dev");
            let stale = home.join("genesis.json").exists() && (chain::Genesis::load(&home.join("genesis.json")).is_err() || node::dev_is_stale(&home));
            if reset || stale {
                if stale {
                    eprintln!("♻️  The dev chain was made by an older Blockula; starting a fresh one.");
                }
                std::fs::remove_dir_all(&home).ok();
            }
            if !home.join("genesis.json").exists() {
                // One-minute epochs: unbonding, jail, Séances and the beacon move in minutes.
                let dev = SummonOpts { epoch_blocks: 60, ..SummonOpts::new("blockula-dev", 1) };
                let params = params::ChainParams::dev_overrides(dev.block_time_ms, dev.epoch_blocks);
                Chain::summon(&home, &SummonOpts { http_port: Some(port), contribution: Some(poc::PocTimes::dev(dev.block_time_ms)), params, ..dev })?;
            }
            node::run_validator(home, net_opts(net, Some(port), Some("local".into()), true))
        }
        Cmd::Summon { chain_id, home, no_faucet, block_time_ms, validators, admins, epoch_blocks, min_bond, seance_blocks, claim_maturity_blocks, vesting_blocks, address_prefix, dev_durations } => {
            let home = home.unwrap_or_else(|| blockula_dir().join(&chain_id));
            if home.join("genesis.json").exists() {
                anyhow::bail!("{} already has a chain", home.display());
            }
            let opts = SummonOpts {
                chain_id: chain_id.clone(),
                faucet: !no_faucet,
                block_time_ms,
                validators,
                // A single-validator chain keeps the classic port; testnets get free ports.
                http_port: (validators == 1).then_some(7070),
                admins,
                epoch_blocks,
                min_bond: min_bond.map(|b| validator_cli::parse_fang(&b)).transpose()?,
                contribution: (seance_blocks.is_some() || claim_maturity_blocks.is_some() || vesting_blocks.is_some()).then(|| {
                    let d = poc::PocTimes::for_block_time(block_time_ms);
                    poc::PocTimes { seance_blocks: seance_blocks.unwrap_or(d.seance_blocks), claim_maturity_blocks: claim_maturity_blocks.unwrap_or(d.claim_maturity_blocks), vesting_blocks: vesting_blocks.unwrap_or(d.vesting_blocks), ..d }
                }),
                address_prefix,
                params: if dev_durations { params::ChainParams::dev_overrides(block_time_ms, epoch_blocks) } else { Default::default() },
            };
            let homes = Chain::summon(&home, &opts)?;
            let g = chain::Genesis::load(&homes[0].join("genesis.json"))?;
            println!("🪦 Summoned chain `{chain_id}` with {validators} validator{} at {}", if validators == 1 { "" } else { "s" }, home.display());
            for (h, v) in homes.iter().zip(&g.validators) {
                println!("   blockula run --home {}    # http://{}  p2p {}", h.display(), v.http_addr, v.p2p_addr);
            }
            println!("   genesis hash: {}  (joiners can pin it with --genesis-hash)", hex::encode(consensus::genesis_block(&g).digest_bytes()));
            println!("   epochs: {} blocks · min self-bond: {} · validator admins: {}", g.epoch_blocks, types::fmt_fang(g.chain_params()?.min_self_bond), g.validator_admins.join(", "));
            if dev_durations {
                let cp = g.chain_params()?;
                println!("   dev durations: unbonding {} blocks · voting {} · timelock {} · deposit {}", cp.unbonding_blocks, cp.voting_blocks, cp.timelock_blocks, types::fmt_fang(cp.proposal_deposit));
            }
            println!("   (Edit validators[].p2p_addr / http_addr in genesis.json before the first start if nodes will be reached at other addresses; every node must use the same file.)");
            Ok(())
        }
        Cmd::Run { home, port, p2p_preset, net } => {
            if home.join("validator.key").exists() {
                node::run_validator(home, net_opts(net, port, p2p_preset, false))
            } else {
                node::run_full_node(home, net_opts(net, Some(port.unwrap_or(7171)), p2p_preset, false))
            }
        }
        Cmd::Wallet { cmd } => validator_cli::wallet(cmd, &blockula_dir()),
        Cmd::Join { genesis, home, port, genesis_hash, checkpoints: pinned, allow_stale_checkpoint, net } => {
            let pins = checkpoints::Pins { genesis_hash, checkpoints: pinned, allow_stale_checkpoint };
            node::join(&genesis, &home, &pins, !net.full)?;
            node::run_full_node(home, net_opts(net, Some(port), None, false))
        }
        Cmd::Checkpoint { peers, height, genesis, json, genesis_hash } => {
            if let Some(path) = genesis {
                let g = chain::Genesis::load(&path)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&checkpoints::genesis_checkpoint(&g))?);
                } else {
                    println!("{}", checkpoints::genesis_hash(&g));
                }
                return Ok(());
            }
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let (g, cp) = rt.block_on(checkpoints::fetch(&peers, height, genesis_hash.as_deref()))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&cp)?);
            } else {
                println!("{}", cp.to_flag());
            }
            eprintln!("`{}` #{} as {} node{} report it: check it against other sources before you trust it", g.chain_id, cp.height, peers.len(), if peers.len() == 1 { "" } else { "s" });
            Ok(())
        }
        Cmd::Status { rpc } => {
            println!("{}", validator_cli::rt()?.block_on(status_cli::read(&rpc))?);
            Ok(())
        }
        Cmd::Validator { cmd } => validator_cli::run(cmd),
        Cmd::Genesis { cmd } => genesis_builder::run(cmd),
        Cmd::Hunt { cmd } => contrib_cli::hunt(cmd, &blockula_dir()),
        Cmd::Contrib { cmd } => contrib_cli::contrib(cmd, &blockula_dir()),
        Cmd::Release { cmd } => release_cli::run(cmd),
        Cmd::Gov { cmd } => gov_cli::gov(cmd, &blockula_dir()),
        Cmd::Multisig { cmd } => gov_cli::multisig(cmd, &blockula_dir()),
        Cmd::Name { cmd } => contrib_cli::name(cmd, &blockula_dir()),
        Cmd::Stake { cmd } => contrib_cli::stake(cmd, &blockula_dir()),
        Cmd::Keygen { prefix } => {
            anyhow::ensure!(crypto::is_prefix(&prefix), "prefix must be one of {:?}", crypto::KNOWN_PREFIXES);
            let k = crypto::Keys::generate();
            println!("{}", serde_json::json!({ "address": k.address_with(&prefix), "seed": k.seed_hex() }));
            Ok(())
        }
    }
}

#[cfg(test)]
mod cli_tests {
    /// clap's own checks of the whole command tree (unique argument ids, groups, defaults), which
    /// otherwise run only for a subcommand being parsed, and only in debug builds: `stake
    /// redelegate` once named a positional `from`, the id of the signer's `--from`.
    #[test]
    fn the_cli_is_well_formed() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
