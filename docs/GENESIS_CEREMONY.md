# Genesis ceremony

How mainnet (`blockula-1`, prefix `boo`) gets its first validator set and its `genesis.json`. Closes the process side of OPS-1 (key ceremony, operator independence) and OPS-2 (a genesis built from reviewed inputs and reproduced by several parties) in [MAINNET.md](MAINNET.md). The rules come from [MAINNET-DESIGN.md](MAINNET-DESIGN.md): DEC-3 (21–32 independent operators), DEC-7 (chain id and prefix), DEC-10 (no admin keys, no faucet), DEC-14 (hosting limits) and 3.7 (the deterministic builder). Operator requirements are in [OPERATIONS.md](OPERATIONS.md).

The testnet did all of this on one laptop (`deploy/launch.sh`). Mainnet must not: **no private key in this ceremony is ever generated, seen or held by anyone but its owner.**

The genesis builder is `blockula genesis operator` and `blockula genesis build` (P-ECON, `node/src/genesis_builder.rs`); `deploy/mainnet/genesis-spec.example.toml` is a complete example spec (the DEC-5 allocation on a throwaway chain).

## Roles

| Role | Who | Does |
|---|---|---|
| Coordinator | Foundation | Runs the calendar, collects submissions, checks independence, edits the spec, chairs the calls. Holds no key that matters. |
| Operators | 21–32 independent entities | Generate keys offline, submit, run validators. |
| Builders | ≥ 3 parties, at least 2 of them not the foundation | Rebuild the binary and the genesis independently and publish the hashes. |
| Recorder | Someone outside the foundation | Keeps the log and the recordings. |
| Counsel | The foundation's lawyers | Sign off allocations and vesting (LEG-1) before the spec freezes. |

## Calendar

| When | Step |
|---|---|
| T − 10 weeks | Operator applications close. Independence review starts. |
| T − 8 weeks | Selected operators confirmed. Key-generation window opens. |
| T − 6 weeks | Submissions due. The spec PR opens for review. |
| T − 4 weeks | **Dry run** on a throwaway chain id, with every operator. |
| T − 2 weeks | Spec frozen (counsel signed off). Release candidate tagged and reproduced. |
| T − 1 week | Builders publish matching genesis hashes. The release is signed and published with `genesis.json`. |
| T | Genesis time. Operators start their validators in the announced window. |
| T + 1 day | Ceremony record published. |

## 1. Operator independence

Each operator is one legal entity (or one individual) with its own staff, machines and keys. Operators under common control or ownership count as one. When selecting and at every later change, the coordinator checks that:

- No hosting provider, ASN or jurisdiction hosts more than ¼ of the set (8 of 32), measured by the ASN that announces the validator's IP and the country of its data center.
- No facility (data center) hosts more than 15% of the set.
- No entity runs more than one validator. The founder and the foundation run at most 2 between them (DEC-3, DEC-14).
- The distribution table (operator, entity jurisdiction, provider, ASN, data-center country) is published with the spec.

## 2. Offline key generation (each operator, alone)

Each operator generates two keys on a machine that has never been and will never be online:

- the **consensus key**: signs votes; goes to the validator host and holds only a fee float;
- the **owner key**: controls the bond, rewards and commission; stays offline for good.

1. **Prepare the offline machine.** A laptop or small PC you control. Install a fresh OS from install media whose checksum you verified. Remove or disable Wi-Fi and Bluetooth (hardware switch, or remove the card); never plug in a network cable.
2. **Get a verified `blockula` onto it**, as in [OPERATIONS.md "To an offline machine"](OPERATIONS.md#to-an-offline-machine). On an online machine that runs a verified `blockula` with your release key id pinned ([a first install](OPERATIONS.md#a-first-install) if it doesn't yet), `blockula release fetch vX.Y.Z --file blockula-<target>.tar.gz --dir offline-vX.Y.Z` downloads the release candidate's archive for the offline machine's platform (`<target>`, whatever the online machine runs) and keeps it only if it matches the `checksums.txt` signed by the pinned key; write down the SHA-256 it prints. Ideally, also rebuild that archive (`COMMIT=<commit> scripts/reproduce-release.sh vX.Y.Z <target>`) and compare. Copy the directory over on a fresh USB stick, and on the offline machine check the archive against the SHA-256 you wrote down (`sha256sum --strict -c`) before unpacking it and installing the binary. Nothing on the stick vouches for itself. A first install's own archive (`first-vX.Y.Z/blockula-<target>.tar.gz`, checked against the announced SHA-256) can be carried instead when the offline machine has the same platform. An archive downloaded any other way (a browser, `gh release download`) hasn't been checked against anything.
3. **Generate the keys:**

   ```bash
   umask 077
   blockula keygen > consensus.json                   # {"address", "seed"}
   jq -r .seed consensus.json > validator.key         # the validator home's key file
   blockula keygen > owner.json                       # the cold owner account
   jq -r .address consensus.json owner.json           # note both addresses
   ```

4. **Proof of possession.** The consensus key signs `pop_message(chain_id, owner, p2p_addr, http_addr)` (`node/src/types.rs`), which binds it to your owner account and addresses so nobody can register a key they don't hold. For genesis, the builder produces it offline:

   ```bash
   blockula genesis operator --chain-id blockula-1 --key validator.key \
     --owner <owner boo1…> --p2p-addr <public-ip>:30300 --http-addr https://<your sentry> \
     --commission-bps 500 --self-bond 10000 --name "<operator>" \
     --legal-entity "<entity>" --entity-jurisdiction DE --hosting-provider "<provider>" \
     --asn <asn> --datacenter-country FI --datacenter "<facility>" \
     > operator.toml                                  # add --json for JSON
   ```

   It prints the public operator entry (public key, PoP, owner, addresses, the fields above) and nothing secret. Fields left out come out empty for you to fill in; the builder flags empty independence fields.
5. **Back up both keys** before anything else: encrypt them (`age -p -o consensus.json.age consensus.json`, the same for `owner.json`) and store copies in two places you control. Decrypt one copy to prove it works. Write the passphrase down separately.
6. **Carry out only public data:** `operator.toml` and the two addresses, on a fresh USB stick (or as QR codes). Nothing else leaves the machine.
7. **The consensus key moves once** to the validator host, by hand: an encrypted copy on a USB stick, decrypted on the host into `validator.key` (mode 600, owned by the service user). Check the address matches. The owner key never moves.

## 3. Submission

Each operator opens a pull request adding `genesis/blockula-1/operators/<operator>.toml`, from a GitHub account tied to the entity, with a signed commit (SSH or GPG):

```toml
# genesis/blockula-1/operators/<operator>.toml
[operator]
name = "Example Validators GmbH"
contact = ["ops@example.org", "+49 …"]            # two reachable contacts

[keys]                                              # from `blockula genesis operator`
consensus_public_key = "…"                          # hex, 1,952 bytes
consensus_address = "boo1…"
owner = "boo1…"                                     # the cold owner account
pop = "…"                                           # hex, proof of possession

[network]
p2p_addr = "203.0.113.7:30300"                      # dedicated public IPv4
http_addr = "https://rpc.example.org"               # your sentry, never the validator

[staking]                                           # P-STAKE parameters
commission_bps = 500
self_bond = "…"                                     # FANG, from the operator's allocation

[independence]
legal_entity = "Example Validators GmbH"
entity_jurisdiction = "DE"
beneficial_owners = ["…"]
hosting_provider = "…"
asn = 0                                             # the ASN announcing p2p_addr
datacenter_country = "FI"
datacenter = "…"                                    # the facility itself
founder_or_foundation = false                       # true only for the founder's or foundation's seats
affiliations = []                                   # other validators, operators or the foundation you're tied to
statement = "We generated these keys offline on hardware we control. Nobody else has had access to the private keys. We control no other genesis validator."
```

The coordinator checks every submission: the PoP verifies for `blockula-1`; keys, addresses and IPs are unique; the p2p port answers from outside (during the dry run); the independence fields hold up (the ASN really announces the IP; the entity exists in the stated registry); and the provider/ASN/jurisdiction limits hold for the whole set. Problems go back to the operator in the PR.

## 4. The spec

`genesis/blockula-1/spec.toml` (format: `deploy/mainnet/genesis-spec.example.toml`) collects everything the genesis is made from, as reviewed text:

- `chain_id = "blockula-1"`, address prefix `boo`, and a fixed `genesis_time` (UTC milliseconds), never "now";
- protocol parameters (block time, epoch length, `max_validators = 32`, staking, slashing, fees, issuance, governance; MAINNET-DESIGN section 3);
- allocations, as signed off by counsel (TOK-4, DEC-5): each `[[allocation]]` names its DEC-5 bucket, recipient and amount, and the builder gives it the bucket's vesting curve (keyless buckets get their system account); every bucket must hold exactly its share and the total must be exactly `total_supply`;
- the operators: `operators = [...]`, the merged submission files;
- the reserved names (DEC-11) and the Soul ledger (3.5);
- `faucet = false` and `validator_admins = []`; the builder refuses anything else for `blockula-1` (DEC-10).

The spec PR needs two maintainer approvals and stays open for operator comments for at least a week. When it merges, the spec is frozen: its SHA-256 goes into the ceremony log, and any later change restarts step 5.

## 5. Build it several times

Each builder, independently, on their own machine:

```bash
git clone https://github.com/jackdishman/blockula && cd blockula && git checkout <release tag>
COMMIT=<the commit you reviewed> scripts/reproduce-release.sh <release tag>   # the tag is that commit; the binary they'll use matches the release
sha256sum genesis/blockula-1/spec.toml
blockula genesis build --spec genesis/blockula-1/spec.toml --out genesis.json   # prints the genesis hash
sha256sum genesis.json
```

Each builder posts, in the ceremony issue and signed (a signed commit adding `genesis/blockula-1/builds/<builder>.txt`): the release tag and binary SHA-256, the spec SHA-256, the `genesis.json` SHA-256, and the genesis hash. **All of them must match.** Any difference stops the ceremony until it's explained and fixed, and then every builder rebuilds.

## 6. Publish

- `genesis.json` is committed as `genesis/blockula-1/genesis.json` before the release is tagged. The release workflow attaches it as `genesis-blockula-1.json`, attests it and lists it in `checksums.txt`, so the offline ML-DSA release signature covers it.
- The mainnet genesis hash is compiled into the binary as the known hash for `blockula-1` (CRY-2): the launch release sets `genesis_hash` in `node/checkpoints/blockula-1.json`, a placeholder until then, so earlier binaries refuse `blockula-1` altogether. `blockula join` and `blockula run` refuse any other genesis for that chain id, `--genesis-hash` or not.
- The hash is also published on the website, the project's social accounts and in the operator channel, so no single channel can substitute it.
- Each operator checks before starting: `sha256sum genesis.json` against the published value, and the hash the node prints at startup against the genesis hash.

## 7. Dry run

Four weeks before genesis, the whole ceremony runs on a throwaway chain: chain id `blockula-dryrun-1` with the testnet prefix `tboo`, **dry-run keys generated the same offline way** (never the mainnet keys), real operators, real hosts and real monitoring. It passes when:

- every builder reproduces the dry-run genesis hash;
- every operator is online and voting within 30 minutes of the dry-run genesis time;
- the chain finalizes for 72 hours, including one coordinated restart at a height and one rolling upgrade ([OPERATIONS.md "Upgrades"](OPERATIONS.md#upgrades));
- every operator's alerts reach them (each sends a test alert), and a short incident tabletop runs on the operator channel.

Whatever broke gets fixed, and the fix is in the release candidate before the spec freezes.

## 8. Record

The recorder keeps, and publishes in `genesis/blockula-1/ceremony/` the day after genesis:

- a timestamped log (UTC) of every step, with the spec, build and genesis hashes;
- recordings of the spec-freeze call and the genesis call (with everyone's consent);
- each operator's signed statement (the `[independence]` block above) and each builder's signed build file;
- the final independence table;
- anything that went wrong and how it was resolved.

## What the builder does (P-ECON)

- `blockula genesis operator`: offline; consensus key file, chain id, owner, addresses, commission, self-bond and independence fields in; the public operator entry (TOML, or JSON with `--json`) with its proof of possession out.
- `blockula genesis build --spec <toml> [--out genesis.json] [--strict]`: byte-identical output and genesis hash on any machine for the same spec and binary (validators in address order whatever order the files are listed in). It refuses allocations that don't add up to `total_supply`, unknown buckets, addresses on keyless buckets, bad PoPs, mismatched keys and addresses, a p2p host used twice, self-bonds the owner's allocation can't cover, out-of-bounds parameters, and `faucet = true` or `validator_admins` on `blockula-1`.
- Policy findings (a bucket off its DEC-5 share; fewer than 22 operators; more than ⌊n/4⌋ per provider, ASN or country; more than 15% per facility; an entity with two seats; more than 2 founder or foundation seats; empty independence fields) are warnings on a dry run and errors with `strict = true` or `--strict`, always on `blockula-1`.
- Operators stay in separate files (one PR each) with the independence fields; only the keys, addresses, stake and the provider, ASN, country and entity enter genesis.
