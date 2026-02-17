use std::str::FromStr;

use log::debug;
use serde::Deserialize;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcBlockConfig, RpcGetVoteAccountsConfig, RpcProgramAccountsConfig,
};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use solana_sdk::account_utils::StateMut;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::stake;
use solana_sdk::stake::state::StakeStateV2;
use solana_sdk::sysvar;
use solana_sdk::sysvar::clock::Clock;
use solana_sdk::sysvar::stake_history;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use solana_transaction_status::{UiTransactionEncoding, EncodedTransaction, UiInstruction, UiMessage, TransactionDetails};
use serde_json;

pub struct SolanaClient {
    client: RpcClient,
    vote_account: String,
    identity_account: String,
}

pub struct StakeState {
    pub activated_stake: u64,
    pub activating_stake: u64,
    pub deactivating_stake: u64,
    pub locked_stake: u64,
    pub activated_stake_accounts: u64,
    pub activating_stake_accounts: u64,
    pub deactivating_stake_accounts: u64,
}

pub struct SlotBasedMetrics {
    pub last_checked_slot: u64,
    pub current_epoch: u64,
    pub leader_slots_cache: HashMap<u64, Vec<u64>>,
    pub epoch_schedule: solana_sdk::epoch_schedule::EpochSchedule,
    pub client: SolanaClient,
    pub on_vote_latency: Option<Box<dyn Fn(u64) + Send + Sync>>,
    pub on_slot_update: Option<Box<dyn Fn(u64) + Send + Sync>>,
    // Max vote latency tracking
    pub max_vote_latency: u64,
    pub max_vote_latency_last_reset: std::time::Instant,
    pub on_vote_latency_max: Option<Box<dyn Fn(u64) + Send + Sync>>,
}

pub struct EpochBasedBlockRewards {
    pub client: SolanaClient,
    pub current_epoch: u64,
    pub last_processed_slot: u64,
    pub epoch_schedule: solana_sdk::epoch_schedule::EpochSchedule,
    pub leader_slots: Vec<u64>,
    pub total_block_rewards: i64,
    pub on_block_rewards_update: Option<Box<dyn Fn(i64) + Send + Sync>>,
    // Track recent non-zero block rewards for averaging
    pub recent_block_rewards: Vec<i64>,
    pub on_last_block_rewards_update: Option<Box<dyn Fn(i64) + Send + Sync>>,
}

#[derive(Debug, Deserialize)]
struct KrakenResponse {
    result: ResultData,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct ResultData {
    SOLUSD: TickerData,
}

#[derive(Debug, Deserialize)]
struct TickerData {
    c: [String; 2], // Array for the closing price and volume
}

impl SolanaClient {
    pub fn new(url: &str, identity_account: &str, vote_account: &str) -> SolanaClient {
        let client = RpcClient::new_with_timeout(url.to_string(), Duration::from_secs(30));
        SolanaClient {
            client,
            vote_account: vote_account.to_string(),
            identity_account: identity_account.to_string(),
        }
    }

    pub async fn get_slot(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let slot = self.client.get_slot().await?;
        Ok(slot)
    }

    /// Returns (epoch, epoch_progress) from a single getEpochInfo call
    pub async fn get_epoch(&self) -> Result<(u64, i64), Box<dyn std::error::Error + Send + Sync>> {
        let epoch = self.client.get_epoch_info().await?;
        let progress = ((epoch.slot_index as f64 / epoch.slots_in_epoch as f64) * 10000.0) as i64;
        Ok((epoch.epoch, progress))
    }

    pub async fn get_stake_details(&self) -> Result<StakeState, Box<dyn std::error::Error + Send + Sync>> {
        let program_accounts_config = RpcProgramAccountsConfig {
            account_config: RpcAccountInfoConfig {
                encoding: Some(solana_account_decoder::UiAccountEncoding::Base64),
                ..RpcAccountInfoConfig::default()
            },
            filters: Some(vec![
                // Filter by `StakeStateV2::Stake(_, _)`
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, &[2, 0, 0, 0])),
                // Filter by `Delegation::voter_pubkey`, which begins at byte offset 124
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    124,
                    &Pubkey::from_str(&self.vote_account)?.to_bytes(),
                )),
            ]),
            ..RpcProgramAccountsConfig::default()
        };
        let mut stake_details = StakeState {
            activated_stake: 0,
            activating_stake: 0,
            deactivating_stake: 0,
            locked_stake: 0,
            activated_stake_accounts: 0,
            activating_stake_accounts: 0,
            deactivating_stake_accounts: 0,
        };
        let stake_accounts = self
            .client
            .get_program_accounts_with_config(&stake::program::id(), program_accounts_config)
            .await?;
        let clock_account = self.client.get_account(&sysvar::clock::id()).await?;
        let stake_history_account = self.client.get_account(&stake_history::id()).await?;
        let clock: Clock = solana_sdk::account::from_account(&clock_account)
            .ok_or_else(|| "Error parsing clock account")?;
        let stake_history: solana_sdk::stake_history::StakeHistory =
            solana_sdk::account::from_account(&stake_history_account)
                .ok_or_else(|| "Error parsing stake_history_account")?;
        let start = SystemTime::now();
        let since_the_epoch = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let unix_timestamp = since_the_epoch.as_secs() as i64;
        for (stake_pubkey, stake_account) in stake_accounts {
            let stake_state: StakeStateV2 = stake_account.state()?;
            match stake_state {
                StakeStateV2::Initialized(_) => {
                    println!("pubkey: {:?}", stake_pubkey);
                }
                StakeStateV2::Stake(_, stake, _) => {
                    let account = stake.delegation.stake_activating_and_deactivating(
                        clock.epoch,
                        &stake_history,
                        Some(520), // Todo: hack but it's not relevant anymore
                    );
                    let meta = stake_state
                        .meta()
                        .ok_or_else(|| "Error parsing stake account meta info")?;
                    if meta.lockup.custodian
                        != Pubkey::from_str("11111111111111111111111111111111")?
                        && meta.lockup.unix_timestamp > unix_timestamp
                        && account.effective > 0
                    {
                        stake_details.locked_stake += account.effective;
                    }
                    stake_details.activated_stake += account.effective;
                    stake_details.activating_stake += account.activating;
                    stake_details.deactivating_stake += account.deactivating;

                    if account.effective > 0 {
                        stake_details.activated_stake_accounts += 1;
                    } else if account.activating > 0 {
                        stake_details.activating_stake_accounts += 1;
                    }
                    if account.deactivating > 0 {
                        stake_details.deactivating_stake_accounts += 1;
                    }
                }
                _ => {}
            }
        }
        Ok(stake_details)
    }
    pub async fn get_identity_balance(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let balance = self
            .client
            .get_balance(&Pubkey::from_str(&self.identity_account)?)
            .await?;
        Ok(balance)
    }
    pub async fn get_vote_balance(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let balance = self
            .client
            .get_balance(&Pubkey::from_str(&self.vote_account)?)
            .await?;
        Ok(balance)
    }
    pub async fn get_leader_info(&self, epoch: u64) -> Result<Vec<u64>, Box<dyn std::error::Error + Send + Sync>> {
        let epoch_schedule = self.client.get_epoch_schedule().await?;
        let first_slot_in_epoch = epoch_schedule.get_first_slot_in_epoch(epoch);
        let leader_schedule = self
            .client
            .get_leader_schedule(Some(first_slot_in_epoch))
            .await?
            .ok_or_else(|| "Error parsing leader schedule")?;

        let my_leader_slots = leader_schedule
            .iter()
            .filter(|(pubkey, _)| **pubkey == self.identity_account)
            .next();

        let mut leader_slots: Vec<u64> = Vec::new();
        for (_, slots) in my_leader_slots.iter() {
            for slot_index in slots.iter() {
                leader_slots.push(*slot_index as u64 + first_slot_in_epoch)
            }
        }
        Ok(leader_slots)
    }

    /// Returns (my_leader_slots, my_blocks_produced, total_blocks_produced_all_validators)
    pub async fn get_block_production(&self) -> Result<(usize, usize, u64), Box<dyn std::error::Error + Send + Sync>> {
        let blocks = self.client.get_block_production().await?;

        let mut bp: (usize, usize) = (0, 0);
        let mut total_blocks: u64 = 0;

        for (identity, &(leader_slots, blocks_produced)) in blocks.value.by_identity.iter() {
            total_blocks += blocks_produced as u64;
            if *identity == self.identity_account {
                bp = (leader_slots, blocks_produced);
            }
        }

        Ok((bp.0, bp.1, total_blocks))
    }

    pub async fn get_block_rewards(&self, slot: u64) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        match self
            .client
            .get_block_with_config(
                slot,
                RpcBlockConfig {
                    transaction_details: None,
                    rewards: Some(true),
                    max_supported_transaction_version: Some(0),
                    ..RpcBlockConfig::default()
                },
            )
            .await
        {
            Ok(block) => {
                let rewards = block.rewards.ok_or_else(|| "Error fetching rewards")?;
                if rewards.is_empty() {
                    Ok(0)
                } else {
                    Ok(rewards[0].lamports)
                }
            },
            Err(e) => {
                let error = e.to_string();
                if error.contains("skipped") {
                    Ok(0)
                } else {
                    Err(Box::new(e))
                }
            }
        }
    }

    pub async fn get_jito_tips(&self, epoch: i64) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let (addr, _) = Pubkey::find_program_address(
            &[
                b"TIP_DISTRIBUTION_ACCOUNT",
                Pubkey::from_str(&self.vote_account.clone())?
                    .to_bytes()
                    .as_ref(),
                epoch.to_le_bytes().as_ref(),
            ],
            &Pubkey::from_str("4R3gSG8BpU4t19KYj8CfnbtRpnT8gtk4dvTHxVRwc2r7")?,
        );
        let balance = self.client.get_balance(&addr).await?;
        Ok(balance)
    }

    /// Returns (vote_credit_rank, credits_earned_this_epoch) from a single getVoteAccounts call
    pub async fn get_vote_account_info(&self) -> Result<(u32, u64), Box<dyn std::error::Error + Send + Sync>> {
        let vote_accounts = self
            .client
            .get_vote_accounts_with_config(RpcGetVoteAccountsConfig {
                keep_unstaked_delinquents: Some(false),
                delinquent_slot_distance: Some(125),
                ..RpcGetVoteAccountsConfig::default()
            })
            .await?;

        // Calculate credits per validator and determine rank
        let mut validator_credits: Vec<_> = vote_accounts
            .current
            .iter()
            .map(|vote_account| {
                let current_epoch = &vote_account.epoch_credits.last();
                if let Some((_, credits, prev_credits)) = current_epoch {
                    (
                        vote_account.vote_pubkey.clone(),
                        credits.saturating_sub(*prev_credits),
                    )
                } else {
                    (vote_account.vote_pubkey.clone(), 0)
                }
            })
            .collect();
        validator_credits.sort_by(|a, b| b.1.cmp(&a.1));

        let mut rank = 0u32;
        let mut our_credits = 0u64;
        for (index, (vote_pubkey, credits)) in validator_credits.iter().enumerate() {
            if *vote_pubkey == *self.vote_account {
                rank = (index + 1) as u32;
                our_credits = *credits;
                break;
            }
        }

        Ok((rank, our_credits))
    }

    pub async fn get_sol_usd_price(&self) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        debug!("Fetching sol price from kraken");
        let resp = reqwest::get("https://api.kraken.com/0/public/Ticker?pair=SOLUSD").await?;
        let data: KrakenResponse = resp.json().await?;
        debug!("Kraken response: {:?}", data);
        let price_float: f64 = data.result.SOLUSD.c[0].parse()?;
        Ok((price_float * 1e5) as i64)
    }

    pub async fn get_ms_to_next_slot(
        &self,
        current_slot: u64,
        leader_slots: Vec<u64>,
    ) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        if leader_slots.is_empty() {
            return Ok(-1);
        }
        
        // Sort leader slots to ensure we find the actual next slot
        let mut sorted_slots = leader_slots;
        sorted_slots.sort();
        
        // Find the next leader slot (first one greater than current slot)
        let next_slot = sorted_slots.iter().find(|&&slot| slot > current_slot);
        
        match next_slot {
            Some(&slot) => {
                // Get average slot time from recent performance samples
                let samples = self.client.get_recent_performance_samples(Some(60)).await?;
                let (total_slots, total_secs) = samples.iter().fold(
                    (0u64, 0u64),
                    |(slots, secs), sample| {
                        (
                            slots.saturating_add(sample.num_slots),
                            secs.saturating_add(sample.sample_period_secs as u64),
                        )
                    },
                );
                let average_slot_time_ms = total_secs
                    .saturating_mul(1000)
                    .checked_div(total_slots)
                    .unwrap_or(400); // Default to 400ms if no samples
                
                let slots_until_next = slot.saturating_sub(current_slot);
                Ok((slots_until_next * average_slot_time_ms) as i64)
            }
            None => {
                // No more leader slots in this epoch
                Ok(-1)
            }
        }
    }

    /// Checks only the current slot for vote latency. This is much faster and provides real-time detection.
    /// Returns the vote latency if a vote transaction from our validator is found in the current slot.
    pub async fn get_current_slot_vote_latency(&self, current_slot: u64) -> Result<Option<u64>, Box<dyn std::error::Error + Send + Sync>> {
        match self.client.get_block_with_config(
            current_slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::JsonParsed),
                transaction_details: Some(TransactionDetails::Full),
                rewards: Some(false), // We don't need rewards for vote latency
                commitment: None,
                max_supported_transaction_version: Some(0),
            },
        ).await {
            Ok(block) => {
                if let Some(transactions) = block.transactions {
                    for tx_with_meta in transactions.iter() {
                        if let EncodedTransaction::Json(tx_json) = &tx_with_meta.transaction {
                            // Check if this transaction is signed by our validator's identity key
                            let message = &tx_json.message;
                            let account_keys = match message {
                                UiMessage::Parsed(msg) => &msg.account_keys,
                                _ => continue,
                            };

                            // Check if our identity key is in the account keys (as a signer)
                            let is_signed_by_us = account_keys.iter().any(|key| key.pubkey == self.identity_account);
                            if !is_signed_by_us {
                                continue;
                            }

                            // Check if our vote account is in the account keys
                            let has_our_vote_account = account_keys.iter().any(|key| key.pubkey == self.vote_account);
                            if !has_our_vote_account {
                                continue;
                            }

                            let instructions = match message {
                                UiMessage::Parsed(msg) => &msg.instructions,
                                _ => continue,
                            };

                            for instr in instructions.iter() {
                                if let UiInstruction::Parsed(instruction) = instr {
                                    // Convert the instruction to JSON string to access its fields
                                    if let Ok(instruction_json) = serde_json::to_string(&instruction) {
                                        if let Ok(instruction_value) = serde_json::from_str::<serde_json::Value>(&instruction_json) {
                                            if let Some(program) = instruction_value.get("program").and_then(|v| v.as_str()) {
                                                if program == "vote" {
                                                    log::info!("Found vote instruction from our validator in current slot {}!", current_slot);
                                                    
                                                    // Try to find lockouts in the vote instruction
                                                    let lockouts = instruction_value
                                                        .get("parsed")
                                                        .and_then(|p| p.get("info"))
                                                        .and_then(|i| i.get("towerSync"))
                                                        .and_then(|t| t.get("lockouts"));
                                                    
                                                    if let Some(lockouts) = lockouts {
                                                        if let Some(lockouts_arr) = lockouts.as_array() {
                                                            if !lockouts_arr.is_empty() {
                                                                let mut highest_slot = 0u64;
                                                                for lockout in lockouts_arr {
                                                                    if let Some(slot_val) = lockout.get("slot").and_then(|v| v.as_u64()) {
                                                                        if slot_val > highest_slot {
                                                                            highest_slot = slot_val;
                                                                        }
                                                                    }
                                                                }
                                                                if highest_slot > 0 {
                                                                    let latency = current_slot.saturating_sub(highest_slot);
                                                                    log::info!("Vote latency found in current slot: {} slots (tx slot: {}, voted slot: {})", latency, current_slot, highest_slot);
                                                                    return Ok(Some(latency));
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                
                // No vote transaction found in current slot
                Ok(None)
            }
            Err(e) => {
                if !e.to_string().contains("Block not available") {
                    log::debug!("Error fetching current slot {}: {}", current_slot, e);
                }
                Err(Box::new(e))
            }
        }
    }

}

impl SlotBasedMetrics {
    pub async fn new(client: SolanaClient) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let epoch_schedule = client.client.get_epoch_schedule().await?;
        let current_slot = client.get_slot().await?;
        let current_epoch = epoch_schedule.get_epoch(current_slot);
        let first_slot = epoch_schedule.get_first_slot_in_epoch(current_epoch);
        
        let mut leader_slots_cache = HashMap::new();
        let leader_schedule = client.client.get_leader_schedule(Some(first_slot)).await?
            .ok_or("Failed to get initial leader schedule")?;
        
        // Extract leader slots for our validator
        let leader_slots: Vec<u64> = leader_schedule
            .get(&client.identity_account)
            .map(|slots| slots.iter().map(|&slot| slot as u64 + first_slot).collect())
            .unwrap_or_default();
        
        log::info!("Cached {} leader slots for validator {} in epoch {}: {:?}", 
                   leader_slots.len(), client.identity_account, current_epoch, leader_slots);
        
        leader_slots_cache.insert(current_epoch, leader_slots);
        
        Ok(Self {
            last_checked_slot: current_slot,
            current_epoch,
            leader_slots_cache,
            epoch_schedule,
            client,
            on_vote_latency: None,
            on_slot_update: None,
            max_vote_latency: 0,
            max_vote_latency_last_reset: std::time::Instant::now(),
            on_vote_latency_max: None,
        })
    }

    pub async fn run_loop(&mut self) {
        loop {
            match self.process_new_slots().await {
                Ok(_) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    log::error!("Error in main loop: {}", e);
                    // Continue running, don't crash
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
    
    pub async fn process_new_slots(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let current_slot = self.client.get_slot().await?;
        
        // Trigger slot update callback
        if let Some(on_slot_update) = &self.on_slot_update {
            on_slot_update(current_slot);
        }
        
        // Check for epoch change
        let new_epoch = self.epoch_schedule.get_epoch(current_slot);
        if new_epoch != self.current_epoch {
            self.handle_epoch_change(new_epoch).await?;
        }
        
        // Check if we need to reset max vote latency (every 30 seconds)
        if self.max_vote_latency_last_reset.elapsed() >= Duration::from_secs(30) {
            if self.max_vote_latency > 0 {
                log::info!("Max vote latency in last 30s: {} slots", self.max_vote_latency);
            }
            // Report max before reset
            if let Some(on_vote_latency_max) = &self.on_vote_latency_max {
                on_vote_latency_max(self.max_vote_latency);
            }
            self.max_vote_latency = 0;
            self.max_vote_latency_last_reset = std::time::Instant::now();
        }
        
        // Process all new slots
        for slot in (self.last_checked_slot + 1)..=current_slot {
            self.process_slot(slot).await?;
        }
        
        self.last_checked_slot = current_slot;
        Ok(())
    }
    
    pub async fn handle_epoch_change(&mut self, new_epoch: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        log::info!("Epoch changed from {} to {}", self.current_epoch, new_epoch);
        
        let first_slot = self.epoch_schedule.get_first_slot_in_epoch(new_epoch);
        let leader_schedule = self.client.client.get_leader_schedule(Some(first_slot)).await?
            .ok_or("Failed to get leader schedule")?;
        
        // Extract leader slots for our validator
        let leader_slots: Vec<u64> = leader_schedule
            .get(&self.client.identity_account)
            .map(|slots| slots.iter().map(|&slot| slot as u64 + first_slot).collect())
            .unwrap_or_default();
        
        log::info!("Cached {} leader slots for validator {} in new epoch {}: {:?}", 
                   leader_slots.len(), self.client.identity_account, new_epoch, leader_slots);
        
        self.leader_slots_cache.insert(new_epoch, leader_slots);
        self.current_epoch = new_epoch;
        
        Ok(())
    }
    
    pub async fn process_slot(&mut self, slot: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Get leader slots for current epoch
        let leader_slots = self.leader_slots_cache.get(&self.current_epoch)
            .ok_or("No leader slots cached for current epoch")?;
        
        // Debug: Log leader slot detection
        if leader_slots.contains(&slot) {
            log::debug!("Processing leader slot {} for validator {}", slot, self.client.identity_account);
        }
        
        // Check vote latency (for all slots, not just leader slots)
        match self.client.get_current_slot_vote_latency(slot).await {
            Ok(Some(latency)) => {
                log::info!("Vote latency found in current slot: {} slots (tx slot: {}, voted slot: {})", 
                          latency, slot, slot.saturating_sub(latency));
                if let Some(on_vote_latency) = &self.on_vote_latency {
                    on_vote_latency(latency);
                }
                // Track max vote latency
                if latency > self.max_vote_latency {
                    self.max_vote_latency = latency;
                    log::info!("New max vote latency: {} slots", latency);
                }
            }
            Ok(None) => {
                // No vote transaction, that's normal
            }
            Err(e) => {
                log::warn!("Error checking vote latency for slot {}: {}", slot, e);
                // Continue processing other metrics
            }
        }
        
        Ok(())
    }
}

impl EpochBasedBlockRewards {
    pub async fn new(client: SolanaClient) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let epoch_schedule = client.client.get_epoch_schedule().await?;
        let current_slot = client.get_slot().await?;
        let current_epoch = epoch_schedule.get_epoch(current_slot);
        let first_slot = epoch_schedule.get_first_slot_in_epoch(current_epoch);

        // Cache leader schedule for the current epoch
        let leader_schedule = client.client.get_leader_schedule(Some(first_slot)).await?
            .ok_or("Failed to get initial leader schedule")?;
        let leader_slots: Vec<u64> = leader_schedule
            .get(&client.identity_account)
            .map(|slots| slots.iter().map(|&slot| slot as u64 + first_slot).collect())
            .unwrap_or_default();

        log::info!("Initializing epoch-based block rewards for validator {} in epoch {} (slots {}-{}, {} leader slots)",
                   client.identity_account, current_epoch, first_slot,
                   epoch_schedule.get_last_slot_in_epoch(current_epoch), leader_slots.len());

        let rewards = Self {
            client,
            current_epoch,
            last_processed_slot: first_slot - 1, // Start from before first slot to process all
            epoch_schedule,
            leader_slots,
            total_block_rewards: 0,
            on_block_rewards_update: None,
            recent_block_rewards: Vec::new(),
            on_last_block_rewards_update: None,
        };

        Ok(rewards)
    }

    pub async fn run_loop(&mut self) {
        loop {
            match self.process_epoch_block_rewards().await {
                Ok(_) => {
                    // Wait 5 seconds before next update
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                Err(e) => {
                    log::error!("Error in epoch-based block rewards loop: {}", e);
                    // Continue running, don't crash
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }
    
    pub async fn process_epoch_block_rewards(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        log::debug!("Starting process_epoch_block_rewards for validator {}", self.client.identity_account);

        let current_slot = self.client.get_slot().await?;
        let current_epoch = self.epoch_schedule.get_epoch(current_slot);

        log::debug!("Current slot: {}, current epoch: {}", current_slot, current_epoch);

        // Check for epoch change
        if current_epoch != self.current_epoch {
            self.handle_epoch_change(current_epoch).await?;
        }

        let last_slot = self.epoch_schedule.get_last_slot_in_epoch(current_epoch);

        log::debug!("Processing epoch {} block rewards: {} leader slots, last processed: {}, current: {}",
                   current_epoch, self.leader_slots.len(), self.last_processed_slot, current_slot);
        
        // Process new leader slots incrementally
        let mut new_rewards = 0i64;
        let mut processed_count = 0;
        
        log::debug!("Starting to process {} leader slots", self.leader_slots.len());

        // Process only a small number of leader slots at a time to avoid taking too long
        let max_slots_to_process = 10; // Process max 10 slots per cycle
        let mut slots_processed = 0;

        for &leader_slot in &self.leader_slots.clone() {
            // Only process slots we haven't processed yet and that are not in the future
            if leader_slot > self.last_processed_slot && leader_slot <= current_slot {
                log::debug!("Processing leader slot {}", leader_slot);
                match self.client.get_block_rewards(leader_slot).await {
                    Ok(rewards) => {
                        if rewards > 0 {
                            log::info!("Found block rewards: {} in leader slot {}", rewards, leader_slot);
                            new_rewards += rewards;
                            // Track recent non-zero rewards for averaging
                            self.recent_block_rewards.push(rewards);
                            // Keep only the last 10 non-zero rewards
                            if self.recent_block_rewards.len() > 10 {
                                self.recent_block_rewards.remove(0);
                            }
                        }
                        processed_count += 1;
                    }
                    Err(e) => {
                        if !e.to_string().contains("Block not available") {
                            log::warn!("Error getting block rewards for slot {}: {}", leader_slot, e);
                        }
                        // Continue processing other slots
                    }
                }
                
                slots_processed += 1;
                
                // Limit the number of slots processed per cycle
                if slots_processed >= max_slots_to_process {
                    log::debug!("Processed {} slots, stopping for this cycle", max_slots_to_process);
                    break;
                }
            }
        }
        
        log::debug!("Finished processing leader slots. New rewards: {}, processed count: {}", new_rewards, processed_count);
        
        // Update total rewards if we found new ones
        if new_rewards > 0 {
            self.total_block_rewards += new_rewards;
            log::info!("Updated total block rewards for epoch {}: {} (new: {}, processed: {} slots)", 
                      current_epoch, self.total_block_rewards, new_rewards, processed_count);
        }
        
        log::debug!("About to call callback. Total rewards: {}, callback set: {}", 
                   self.total_block_rewards, self.on_block_rewards_update.is_some());
        
        // Always call callback to update metric (even if no new rewards)
        if let Some(on_update) = &self.on_block_rewards_update {
            log::debug!("Calling block rewards callback with value: {}", self.total_block_rewards);
            on_update(self.total_block_rewards);
        } else {
            log::warn!("Block rewards callback is not set!");
        }
        
        // Calculate and report average of last 4 non-zero block rewards
        if let Some(on_last_update) = &self.on_last_block_rewards_update {
            let last_rewards = self.get_last_block_rewards_avg();
            log::debug!("Calling last block rewards callback with value: {}", last_rewards);
            on_last_update(last_rewards);
        }
        
        // Update last processed slot to current slot (but don't go beyond last slot of epoch)
        self.last_processed_slot = std::cmp::min(current_slot, last_slot);
        
        Ok(())
    }
    
    /// Calculate average of the last 4 non-zero block rewards
    fn get_last_block_rewards_avg(&self) -> i64 {
        if self.recent_block_rewards.is_empty() {
            return 0;
        }
        let count = std::cmp::min(4, self.recent_block_rewards.len());
        let start = self.recent_block_rewards.len().saturating_sub(4);
        let sum: i64 = self.recent_block_rewards[start..].iter().sum();
        sum / count as i64
    }
    
    pub async fn handle_epoch_change(&mut self, new_epoch: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        log::info!("Epoch changed from {} to {} for block rewards tracking", self.current_epoch, new_epoch);

        // Reset for new epoch
        self.current_epoch = new_epoch;
        self.total_block_rewards = 0;
        self.recent_block_rewards.clear();

        let first_slot = self.epoch_schedule.get_first_slot_in_epoch(new_epoch);
        self.last_processed_slot = first_slot - 1; // Start from before first slot

        // Refresh leader schedule cache for new epoch
        let leader_schedule = self.client.client.get_leader_schedule(Some(first_slot)).await?
            .ok_or("Failed to get leader schedule for new epoch")?;
        self.leader_slots = leader_schedule
            .get(&self.client.identity_account)
            .map(|slots| slots.iter().map(|&slot| slot as u64 + first_slot).collect())
            .unwrap_or_default();

        log::info!("Reset block rewards tracking for epoch {} (slots {}-{}, {} leader slots)",
                  new_epoch, first_slot, self.epoch_schedule.get_last_slot_in_epoch(new_epoch),
                  self.leader_slots.len());

        // Call callback with reset value
        if let Some(on_update) = &self.on_block_rewards_update {
            on_update(0);
        }

        Ok(())
    }
    
}
