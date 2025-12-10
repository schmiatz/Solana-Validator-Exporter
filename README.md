# Solana Validator Exporter

A Prometheus exporter for Solana validators. Supports monitoring multiple validators across mainnet, testnet, and devnet.

## Quick Start

```bash
# Build
cargo build --release

# Configure (see config.example.yaml for all options)
cp config.example.yaml config.yaml
nano config.yaml

# Run
./target/release/solana-validator-exporter --config-file config.yaml
```

Metrics available at `http://localhost:9090/metrics`

## Metrics

All metrics include labels: `network` (mainnet/testnet/devnet) and `vote_account`.

### Slot & Epoch

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_slot` | Current slot | `getSlot` RPC, updated every 100ms |
| `solana_epoch` | Current epoch number | `getEpochInfo` RPC |
| `solana_epoch_progress` | Epoch progress (0-10000 = 0-100%) | `slotIndex / slotsInEpoch × 10000` |

### Block Production

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_blocks{block_type="total"}` | Total leader slots for entire epoch | `getLeaderSchedule` RPC |
| `solana_blocks{block_type="produced"}` | Blocks successfully produced | `getBlockProduction` RPC |
| `solana_blocks{block_type="skipped"}` | Leader slots where no block was produced | `slots_processed - blocks_produced` |
| `solana_blocks{block_type="remaining"}` | Future leader slots not yet reached | `total - slots_processed` |

### Vote Credits (Timely Vote Credits - SIMD-0033)

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_vote_credits_earned` | Credits earned in current epoch | `getVoteAccounts` → `epochCredits` |
| `solana_vote_credits_max` | Max possible credits | `total_blocks_produced × 16` |
| `solana_vote_credits_efficiency` | Efficiency ratio × 10000 | `earned / max × 10000` (divide by 100 for %) |

**Credit System**: 1-2 slot latency = 16 credits (max), 3+ slots = 16-(latency-2), minimum 1 credit.

### Vote Latency

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_vote_latency_slots` | Latest vote latency in slots | Parsed from vote transactions, updated every 100ms |
| `solana_vote_latency_slots_max` | Max vote latency in last 30 seconds | Tracks highest latency, resets every 30s |

### Stake

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_stake{stake_type="activated"}` | Active stake (lamports) | `getProgramAccounts` on stake program |
| `solana_stake{stake_type="activating"}` | Stake becoming active | Filtered by vote account |
| `solana_stake{stake_type="deactivating"}` | Stake being withdrawn | |
| `solana_stake{stake_type="locked"}` | Stake with lockup | |
| `solana_stake{stake_type="*_accounts"}` | Count of stake accounts | |

### Balances

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_identity_balance` | Identity account balance (lamports) | `getBalance` RPC |
| `solana_vote_account_balance` | Vote account balance (lamports) | `getBalance` RPC |

### Rewards

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_epoch_block_rewards` | Sum of block rewards this epoch (lamports) | Fetched per leader slot, updated every 5s |
| `solana_last_block_rewards` | Average of last 4 non-zero block rewards | Rolling average |
| `solana_jito_tips` | Jito MEV tips this epoch (lamports) | Balance of Jito tip distribution PDA |

### Network & Performance

| Metric | Description | Source |
|--------|-------------|--------|
| `solana_vote_credit_rank` | Validator rank by vote credits | Sorted from `getVoteAccounts` |
| `solana_ms_to_next_slot` | Milliseconds until next leader slot | `(next_slot - current_slot) × avg_slot_time` |
| `solana_usd_price` | SOL/USD price × 100000 | Kraken API |

## Update Intervals

| Interval | Metrics |
|----------|---------|
| 100ms | `slot`, `vote_latency_slots` |
| 5s | `epoch_block_rewards`, `last_block_rewards` |
| 30s | All other metrics |

## Systemd Service

See `systemd/` folder for service file and install script:

```bash
sudo ./systemd/install.sh
sudo systemctl enable solana-validator-exporter
sudo systemctl start solana-validator-exporter
```
