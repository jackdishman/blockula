# Become a validator

A step-by-step guide for someone who has never run one. It takes about an hour the first time. Pick a path:

- **[Path A: in the cloud](#path-a-in-the-cloud)**, about $5 a month. The easiest, and nothing at home is exposed.
- **[Path B: a home server with port forwarding](#path-b-a-home-server-with-port-forwarding)**: free if you have the machine, but your home IP becomes public.
- **[Path C: a home server behind a cheap relay](#path-c-a-home-server-behind-a-relay)**: the machine is at home, and a $4–5 server shows the world its IP instead of yours.

On a machine at home (B or C), **[`blockula validator setup`](#the-quick-way-blockula-validator-setup)** does most of the work in one command. Or skip to **[Let your coding agent do it](#let-your-coding-agent-do-it)** and paste one prompt.

[RUN-A-NODE.md](RUN-A-NODE.md) is the shorter reference; [OPERATIONS.md](OPERATIONS.md) is for professional operators.

## What a validator does

A Blockula chain is a shared ledger kept by a small group of computers, its **validators**. Every couple of seconds one of them proposes the next block (the latest batch of payments, names and so on), and the others check it and vote. When enough of them agree, the block is final: nobody can undo it. More validators run by more boos means no single boo can stop the chain or rewrite it.

Your validator is a program, `blockula`, running on a computer that's always on. It holds one secret, **`validator.key`**, which it signs its votes with. To join, you lock some of the chain's coins as a **bond**, a promise to behave.

## Before you start: costs and risks

- **The coins have no value.** FANG and every chain's coins are for trying things out. Nobody sells or buys them, and validating isn't paid work.
- **It costs a little.** In the cloud, about $5 a month. At home, the electricity and the upload: a validator in a small set sends roughly 150 GB a month ([RUN-A-NODE.md, What it takes](RUN-A-NODE.md#what-it-takes)).
- **It must stay online.** A validator that's off misses its turns. Miss too many and it's **jailed**: it stops voting, loses a little of its bond, and stays out until you run `blockula validator unjail <your address> --key <home>/validator.key --rpc <node>` once the jail time is over. On a set of two or three, your downtime can stop the whole chain, so only join if you mean to keep it up.
- **Never run two copies of one key.** If the same `validator.key` runs in two places, both copies vote, and the chain sees your key sign two different blocks for one turn. That's **double-signing**: it **slashes** (takes) 5% of the bond and bans the validator forever. The usual way it happens is restoring a backup or a cloned disk while the old machine is still running. Before you start a key anywhere, make sure it's stopped everywhere else.
- **Back up the key, once, somewhere safe.** Lose it and the validator is gone (your bond comes back to the account through the 21-day unbonding, if you still hold the key). Leak it and someone else can sign as you.
- **Your addresses are public.** The chain publishes the host names and ports you register, and other validators see your machine's IP. That's why Paths A and C exist.
- **It's experimental software**, under the MIT license, with no warranty and no external audit.

## What you need from the chain

From the chain's listing on [blockula.app](https://blockula.app) (its **Become a validator** button), or from whoever runs it:

| | What it is | Example (the demo chain) |
|---|---|---|
| **Node URL** | One of the chain's nodes, `https://…` | `https://demo.blockula.app` |
| **Genesis hash** | 64 hex characters that pin the exact chain, so nobody can swap in a fake one | `e712d937…d0250f705e` |
| **Contact** | Who to send your validator address to | the listing's `contact`; for the demo chain, the [node form](https://github.com/jackdishman/blockula/issues/new?template=node-operator.yml) |

Check the hash yourself: `curl -s <node>/api/status` shows `genesis_hash`, and it must match what the chain's operator told you. Only chains on the open internet can take outside validators: one that runs on a home network or a tailnet can't.

## Install the `blockula` CLI on your computer

On a Mac or Linux:

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
blockula --version
```

It installs into `~/.blockula/bin`. To check the download came from this project's release pipeline first (recommended), install [GitHub's CLI](https://cli.github.com), run `gh auth login`, and follow [OPERATIONS.md, A first install](OPERATIONS.md#a-first-install). Path A does this check for you.

## The quick way: `blockula validator setup`

On the machine that will be the validator (a Mac or Linux computer at home, or a server you're logged in to):

```bash
blockula validator setup
```

It asks for the chain's node and genesis hash, then:

1. checks the node reports exactly that genesis hash, and stops if it doesn't;
2. works out how the chain's validators reach each other from the addresses they publish, and this machine's address on it: its Tailscale IP on a tailnet chain, its home network IP on a home-network chain, or on an internet chain the DNS name or public IP (or relay) you give it;
3. makes the validator's key in `~/.blockula/<chain id>-validator/validator.key` (mode 600), once;
4. on an internet chain, sets up your full node too (`~/.blockula/<chain id>`, port 7171), which the chain publishes as your validator's HTTP address, while the validator's own API stays on this machine;
5. takes the bond from the chain's faucet; on a chain without one, it prints the command for the owner to send it, and waits;
6. runs the node in the background: a launchd agent on a Mac (it starts when you log in, restarts after a crash, and keeps the Mac from idle sleep while it runs) or a systemd user service on Linux. It never restarts a node that stopped on purpose (a halt, an upgrade it needs, slashing protection);
7. waits for the node to sync, signs `blockula validator register`, and prints the exact `approve` or `admit` command to send the chain's owner, and how to see its logs, stop it and start it. All of that is also in `~/.blockula/<chain id>-validator/README.md`.

Run it again at any time: it picks up where it stopped, and it never makes a second key or starts a second copy of a running node. As flags (for scripts and agents): `--node <node> --genesis-hash <hash>`, `--host <your host>` (needed on an internet chain), `--relay` (Path C), `--foreground` (no service: the node runs in the terminal), `--dry-run` (print the review, do nothing) and `--yes` (no questions). On Linux, `sudo loginctl enable-linger $USER` keeps a user service running while you're logged out.

## Path A: in the cloud

### On Fly.io (the script does it all)

`deploy/host-validator.sh` sets up a validator on [Fly.io](https://fly.io): one small machine with its own public IPv4, a 3 GB disk, about $5 a month. It's how the demo chain got its second validator. Your validator is reachable at `<app>.fly.dev`, so you need no domain.

1. **Make a Fly account** at [fly.io](https://fly.io) and add a card (it bills by use).
2. **Install the tools** on your computer:

   ```bash
   brew install flyctl gh jq          # on a Mac (Homebrew); on Linux: curl -L https://fly.io/install.sh | sh, and gh and jq from your package manager
   fly auth login                     # opens the browser
   gh auth login                      # the script checks the release with it
   ```

3. **Get the script**, from this repository:

   ```bash
   git clone https://github.com/jackdishman/blockula && cd blockula
   ```

4. **Run it**, with the chain's node and genesis hash, and an app name that's yours (lowercase letters, digits and dashes; it becomes `<app>.fly.dev`):

   ```bash
   GENESIS=https://demo.blockula.app \
   GENESIS_HASH=<the chain's genesis hash> \
   APP=<your-app-name> REGION=ord \
     deploy/host-validator.sh
   ```

   It takes 3–5 minutes, and each step is skipped if it's already done, so it's safe to run again:
   - it checks the release's build provenance and pins its SHA-256;
   - `blockula validator init` makes your key in `~/.config/blockula/hosted/<chain id>-<app>/home/validator.key` (mode 600) and prints your **validator address** (`boo1…`);
   - it takes the bond from the chain's faucet, if it has one. If not, it stops and tells you how many coins to ask the operator for; run it again once they've arrived;
   - it creates the Fly app, its disk and a dedicated IPv4, sends the key up once as a temporary secret, deploys, and deletes the secret as soon as the node is up. From then on the key lives only on the Fly disk and in your local backup;
   - it signs `blockula validator register`, which bonds your coins and prints how the chain lets you in.

5. **Ask to be let in** ([below](#getting-into-the-set)), then [check it works](#check-it-works).

`REGION` is a [Fly region](https://fly.io/docs/reference/regions/) code; pick one near the chain's other validators. The script's header lists its other settings (`VERSION`, `BOND`, `VM_MEMORY`, `VOLUME_GB`).

Your Fly validator also serves the chain's API on `http://<app>.fly.dev:7070` and `https://<app>.fly.dev`, because the chain publishes that address for nodes that sync from it. That's fine on a small chain. On a big one, put a separate full node in front instead ([OPERATIONS.md, Sentries](OPERATIONS.md#sentries)).

### On any $5 VPS (Linux, systemd)

Any small Linux server with a public IPv4 works: Hetzner Cloud, DigitalOcean, Vultr, Linode and others have one for about $4–6 a month (1 vCPU, 1 GB RAM, 20 GB disk is plenty for a small set). Check your provider's terms allow blockchain nodes. On a fresh Debian 12 or Ubuntu 24.04 server, logged in over SSH, with `<node>`, `<hash>` and `<id>` (the chain id, from `/api/status`) from the chain, and `<server IP>` the server's public IPv4:

```bash
# A user for the node, and the CLI
sudo useradd --system --create-home --home-dir /var/lib/blockula --shell /usr/sbin/nologin blockula
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
sudo install -m 755 ~/.blockula/bin/blockula /usr/local/bin/blockula

# The validator's home: its key, the genesis and the addresses it advertises. Note the address it prints.
sudo -u blockula blockula validator init --home /var/lib/blockula/validator \
  --genesis <node> --genesis-hash <hash> \
  --public-p2p-addr <server IP>:30300 --public-http-addr http://<server IP>:7171

# Your own full node of the chain, which the chain publishes as your validator's HTTP address.
# Once it logs that it's following the chain, press Ctrl-C: it has joined, and the service below runs it.
sudo -u blockula blockula join --genesis <node> --genesis-hash <hash> --home /var/lib/blockula/full --bind 0.0.0.0
```

Then two services, `/etc/systemd/system/blockula-validator.service`:

```ini
[Unit]
Description=Blockula validator
Wants=network-online.target
After=network-online.target

[Service]
User=blockula
ExecStart=/usr/local/bin/blockula run --home /var/lib/blockula/validator --p2p-bind 0.0.0.0
Restart=on-failure
RestartSec=5
# 10-13 are deliberate stops (a halt height, an upgrade needed, slashing protection, a program
# fault): restarting would only stop again, or risk a double-sign.
RestartPreventExitStatus=10 11 12 13
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

and `/etc/systemd/system/blockula-full.service`, the same with `Description=Blockula full node` and `ExecStart=/usr/local/bin/blockula run --home /var/lib/blockula/full --bind 0.0.0.0`. Then:

```bash
sudo systemctl daemon-reload && sudo systemctl enable --now blockula-full blockula-validator
sudo ufw allow 22/tcp && sudo ufw allow 30300/tcp && sudo ufw allow 7171/tcp && sudo ufw enable

# The bond, from the faucet if the chain has one (else ask its operator to send it)
blockula wallet faucet --to <your validator address> --rpc <node>
sudo -u blockula blockula validator register --home /var/lib/blockula/validator --rpc <node>
```

Keep the validator's own HTTP port (7070) closed: only 30300 (p2p) and 7171 (the full node) are open. Keep the clock right (`timedatectl` should say `System clock synchronized: yes`). Back up `/var/lib/blockula/validator/validator.key` to your own computer once (`scp`), and keep it offline.

## Path B: a home server with port forwarding

Any always-on machine works: a spare laptop, a mini PC, a Raspberry Pi 4 or 5 with 4 GB, a Mac that never sleeps. What to know first:

- **Your home IP becomes public**, published by the chain for good. Anyone can find roughly where you are from it, and could aim traffic at your connection. If that bothers you, use Path C.
- **Carrier-grade NAT doesn't work.** If your router's WAN (internet) address starts with `100.64`–`100.127`, your provider shares one IP between many homes, and nobody can reach your ports. Use Path A or C.
- **A changing IP needs a name.** Most home IPs change now and then. Use a dynamic DNS name (many routers have it built in, or try DuckDNS) as your host, so it follows your IP.
- **Your upload counts.** About 150 GB a month on a small set; check your plan has no low cap.

Steps, with `<your host>` your dynamic DNS name (or public IP):

1. **Give the machine a fixed address on your network** (in your router: "DHCP reservation"), say `192.168.1.50`.
2. **Forward two ports** in your router to that address: TCP **30300** (p2p: other validators dial it) and TCP **7171** (your full node). Never forward 7070, the validator's own HTTP.
3. **Install and set up** with one command ([the quick way](#the-quick-way-blockula-validator-setup)): it makes the key, sets up the full node, takes the bond, starts both in the background and registers:

   ```bash
   blockula validator setup --node <node> --genesis-hash <hash> --host <your host>
   ```

   Or by hand, as on the VPS above with `<your host>` in place of `<server IP>` (on a Mac, as your own user, with homes under `~/.blockula/`, kept running in two terminals or with `launchd`):

   ```bash
   blockula join --genesis <node> --genesis-hash <hash> --home ~/.blockula/<id> --bind 0.0.0.0
   blockula validator init --home ~/.blockula/<id>-validator --genesis <node> --genesis-hash <hash> \
     --public-p2p-addr <your host>:30300 --public-http-addr http://<your host>:7171
   blockula wallet faucet --to <your validator address> --rpc <node>
   blockula run --home ~/.blockula/<id>-validator --p2p-bind 0.0.0.0
   blockula validator register --home ~/.blockula/<id>-validator --rpc <node>
   ```

4. **Check the ports from outside**: from a phone off Wi-Fi, or a site like portchecker.co, port 30300 on `<your host>` should be open once the validator runs.
5. **Turn off sleep** (on a Mac, `setup`'s service already keeps it from idle sleep while the node runs, but not from sleeping when a laptop's lid closes), and plan for power cuts: a cheap UPS keeps a router and a mini PC up through short ones.

## Path C: a home server behind a relay

The validator runs at home, but the chain publishes a small rented server's address, the **relay**. The relay forwards p2p and HTTP over [Tailscale](https://tailscale.com), a free private network between your machines, to the home server. Your home IP stays private, your router needs no open ports, and it works even behind carrier-grade NAT.

```
other validators ──TCP 30300──▶ relay (public IP, $4/month) ──Tailscale──▶ home server: validator
nodes that sync  ──TCP 7171───▶ relay                       ──Tailscale──▶ home server: full node
```

Your validator still dials other validators directly from home, so they see your home IP in their connection logs (not on the chain). The relay carries everything sent to you, about 150 GB a month on a small set: pick a VPS plan that includes at least 1 TB.

1. **Tailscale on both machines.** Make a free account, then on the home server and on the relay:

   ```bash
   curl -fsSL https://tailscale.com/install.sh | sh
   sudo tailscale up
   tailscale ip -4      # on the home server: its tailnet address, say 100.101.102.103
   ```

   In the Tailscale admin console, turn off key expiry for both machines, so the link doesn't drop after six months.

2. **The relay forwards two ports.** On the relay (Debian or Ubuntu), with `HOME_TS` the home server's tailnet address. With socat, as two small services:

   ```bash
   sudo apt-get install -y socat
   for p in 30300 7171; do
   sudo tee /etc/systemd/system/relay-$p.service >/dev/null <<EOF
   [Unit]
   Description=Relay TCP $p to the home validator
   After=network-online.target tailscaled.service
   Wants=network-online.target

   [Service]
   ExecStart=/usr/bin/socat TCP-LISTEN:$p,fork,reuseaddr,keepalive TCP:100.101.102.103:$p,keepalive
   Restart=always
   DynamicUser=yes

   [Install]
   WantedBy=multi-user.target
   EOF
   done
   sudo systemctl daemon-reload && sudo systemctl enable --now relay-30300 relay-7171
   sudo ufw allow 22/tcp && sudo ufw allow 30300/tcp && sudo ufw allow 7171/tcp && sudo ufw enable
   ```

   Or with HAProxy (`sudo apt-get install -y haproxy`), appending to `/etc/haproxy/haproxy.cfg` and then `sudo systemctl restart haproxy`:

   ```
   defaults
       mode tcp
       timeout connect 5s
       timeout client  1h
       timeout server  1h

   frontend p2p
       bind :30300
       default_backend home_p2p
   backend home_p2p
       server home 100.101.102.103:30300 check

   frontend http
       bind :7171
       default_backend home_http
   backend home_http
       server home 100.101.102.103:7171 check
   ```

   A Fly machine can be the relay too, but it needs Tailscale inside the container; a plain VPS is simpler.

3. **The home server publishes the relay's address** and runs the `public-nat` preset. With `<relay host>` the relay's public IP (or a DNS name pointing at it), [the quick way](#the-quick-way-blockula-validator-setup) does both:

   ```bash
   blockula validator setup --node <node> --genesis-hash <hash> --host <relay host> --relay
   ```

   Or by hand:

   ```bash
   blockula join --genesis <node> --genesis-hash <hash> --home ~/.blockula/<id> --bind 0.0.0.0
   blockula validator init --home ~/.blockula/<id>-validator --genesis <node> --genesis-hash <hash> \
     --public-p2p-addr <relay host>:30300 --public-http-addr http://<relay host>:7171
   blockula wallet faucet --to <your validator address> --rpc <node>
   blockula run --home ~/.blockula/<id>-validator --p2p-bind 0.0.0.0 --p2p-preset public-nat
   blockula validator register --home ~/.blockula/<id>-validator --rpc <node>
   ```

   `--p2p-preset public-nat` is for validators behind a NAT or a TCP proxy: every connection reaches your node from the relay's one tailnet address (a private one), which the default settings would refuse or rate-limit as a single peer. It's the preset Fly validators run too. For a service, use the VPS section's systemd unit with `--p2p-preset public-nat` added.

4. **Check it from outside:** `nc -vz <relay host> 30300` from another network should connect, and `curl http://<relay host>:7171/api/status` should show the chain.

## Getting into the set

`register` locks your bond and prints how this chain lets you in. It depends on how many validators the chain has:

- **One to three validators:** every one counts for every block, so the operator lets each newcomer in by hand. Send them your validator address (`boo1…`, which `init` printed) through the listing's contact, and confirm it with them in a second way (a message from an account they know you by). `register` prints the command they run: `blockula validator admit <your address> …` on chains with admission, or `blockula validator approve <your address> …` on others. Your node signals it's ready by itself, and you join at the end of the epoch. For the demo chain, ask with the [node form](https://github.com/jackdishman/blockula/issues/new?template=node-operator.yml): it asks for the chain, your address and your node's p2p and HTTP addresses, and it's public, so send nothing secret. A person reads it, so allow a few days.
- **Four or more:** you join by stake at an epoch boundary, a few newcomers an epoch, ranked by bond plus what others delegate to you.

An epoch is a fixed number of blocks; `curl -s <node>/api/validators | jq .blocks_until_next_epoch` says how long until the next one. Until you're in, your node waits where it caught up to; once you're in, it follows the chain over p2p.

## Check it works

```bash
# Are you in? Your address should be in .active (or .next_epoch, before the boundary).
curl -s <node>/api/validators | jq '[.active[].validator.address]'

# Your own node: 200 when its head is fresh and synced, 503 and a reason when not
curl -s -w '\n%{http_code}\n' http://<your host or relay>:7171/api/health   # Path A: https://<app>.fly.dev/api/health

# Blocks keep coming: run it twice, a few seconds apart, and the height should grow
curl -s <node>/api/status | jq .height
```

**Logs:** `fly logs -a <app>` (Path A on Fly), `journalctl -u blockula-validator -f` (systemd), or the terminal it runs in. Good signs: `entered epoch … voting=true`. The explorer's **Validators** page (`<node>/#/validators`) shows your validator's liveness, misses and jail status.

## Upgrading

When a new release comes out, or the node logs "protocol vN activates at #H … install a release that supports it", upgrade before that height or the node stops there:

- **Fly:** run `deploy/host-validator.sh` again with the same settings (and `VERSION=vX.Y.Z` for a specific release). It reuses your key, verifies the release and redeploys.
- **VPS or home:** install the new release (verified: [OPERATIONS.md, Upgrading](OPERATIONS.md#upgrading)), then `sudo systemctl restart blockula-full blockula-validator`, or restart the two terminals. Your key and data stay where they are.

Restart one machine at a time, and check `/api/health` before the next.

## Moving to another machine

The one rule: **the old one must be stopped before the new one starts.**

1. Set up the new machine (install, `join` the full node) but don't start a validator.
2. **Stop the old validator, and make sure it stays stopped**: `fly machine stop` and `fly scale count 0 -a <app>` (Fly), `sudo systemctl disable --now blockula-validator` (systemd), or shut the machine down. Check it really stopped (no `blockula` process, no new log lines).
3. Copy the validator home (`validator.key`, `genesis.json`, `node.json` and the newest `slashing-protection.json`) to the new machine, mode 600 for the key. Then delete it from the old one.
4. If the new machine has another host name or IP, publish the new addresses: `blockula validator set-addr --key <home>/validator.key --rpc <node> --p2p <new host>:30300 --http http://<new host>:7171 --home <home>`.
5. Start the validator on the new machine and [check it works](#check-it-works).

Missing a few minutes of turns is harmless; two copies running for a second can cost 5% and the validator. [OPERATIONS.md, Moving a validator](OPERATIONS.md#moving-a-validator) has the full procedure.

## Let your coding agent do it

Paste this into Claude Code (or another coding agent) on the computer you'll set up from, after filling in the three `<…>` lines. blockula.app's **Become a validator** has a copy button with the chain's node and hash already in.

```text
Set me up as a validator for a Blockula chain, following docs/BECOME-A-VALIDATOR.md in
https://github.com/jackdishman/blockula (clone it if it isn't here).

  Node:          <the chain's node URL, https://…>
  Genesis hash:  <its 64-hex genesis hash>
  Path:          <A (Fly.io, or a VPS I'll give you SSH to), B (this home machine, port forwarding) or C (this home machine behind a relay VPS over Tailscale)>

Rules:
- First check that the node's /api/status reports exactly that genesis hash. Stop if it doesn't.
- Install only a verified release: check its build provenance with `gh attestation verify` and its
  line in checksums.txt (deploy/host-validator.sh does this for Path A on Fly).
- Never print, paste, upload or move validator.key, and never put it in a commit, a log or a chat.
  Never start the same key in two places. Tell me where the key's backup is and ask me to keep it
  offline.
- Ask me before anything that costs money (a Fly app, a VPS) or changes my network (router port
  forwards, firewall rules, Tailscale), and wait for my yes.
- For Path B or C, use `blockula validator setup` (with --node, --genesis-hash, --host, and --relay
  for C): it does the key, the full node, the bond, the background service and register. Run it
  with --dry-run first and show me its review; run it with --yes once I say yes.
- Take the bond from the chain's faucet if it has one; if it doesn't, tell me how much to ask the
  chain's operator for and wait.
- Run `blockula validator register`, then STOP: give me my validator address (boo1…) and the exact
  admit or approve command register printed, for me to send to the chain's operator. Don't try to
  admit or approve yourself.
- Afterwards, show me how to check it: /api/validators, /api/health and the logs.
```

It does the rest of the steps on this page, and asks you first for each one that spends money or opens a port.
