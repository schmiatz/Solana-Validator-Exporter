use std::str::FromStr;

use log::{debug, info, error};
use serde::Deserialize;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcBlockConfig, RpcGetVoteAccountsConfig, RpcProgramAccountsConfig,
};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use solana_rpc_client_api::response::RpcPerfSample;
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
    block_rewards: HashMap<u64, i64>,
    current_epoch: Option<u64>,
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
}

pub struct EpochBasedBlockRewards {
    pub client: SolanaClient,
    pub current_epoch: u64,
    pub last_processed_slot: u64,
    pub epoch_schedule: solana_sdk::epoch_schedule::EpochSchedule,
    pub total_block_rewards: i64,
    pub on_block_rewards_update: Option<Box<dyn Fn(i64) + Send + Sync>>,
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
            block_rewards: HashMap::new(),
            current_epoch: None,
        }
    }

    pub async fn get_slot(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let slot = self.client.get_slot().await?;
        Ok(slot)
    }

    pub async fn get_epoch(&self) -> Result<(i64, i64), Box<dyn std::error::Error + Send + Sync>> {
        let epoch = self.client.get_epoch_info().await?;
        Ok((
            epoch.epoch as i64,
            ((epoch.slot_index as f64 / epoch.slots_in_epoch as f64) * 10000.0) as i64,
        ))
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
    pub async fn get_leader_info(&self) -> Result<Vec<u64>, Box<dyn std::error::Error + Send + Sync>> {
        let epoch_info = self.client.get_epoch_info().await?;
        let epoch_schedule = self.client.get_epoch_schedule().await?;
        let first_slot_in_epoch = epoch_schedule.get_first_slot_in_epoch(epoch_info.epoch);
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

    pub async fn get_block_production(&self) -> Result<(usize, usize), Box<dyn std::error::Error + Send + Sync>> {
        let blocks = self.client.get_block_production().await?;
        let my_blocks = blocks
            .value
            .by_identity
            .iter()
            .filter(|(identity, _)| **identity == self.identity_account)
            .next();
        let mut bp: (usize, usize) = (0, 0);
        if let Some((_, block_production)) = my_blocks {
            bp = *block_production;
        }
        Ok(bp)
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

    /// Fetches a block with full transaction details and extracts both rewards and vote latency
    /// Returns (rewards, vote_latency) where vote_latency is Some(latency) if found, None otherwise
    pub async fn get_block_rewards_and_vote_latency(&self, slot: u64) -> Result<(i64, Option<u64>), Box<dyn std::error::Error + Send + Sync>> {
        match self
            .client
            .get_block_with_config(
                slot,
                RpcBlockConfig {
                    encoding: Some(UiTransactionEncoding::JsonParsed),
                    transaction_details: Some(TransactionDetails::Full),
                    rewards: Some(true),
                    max_supported_transaction_version: Some(0),
                    ..RpcBlockConfig::default()
                },
            )
            .await
        {
            Ok(block) => {
                // Extract rewards
                let rewards = block.rewards.ok_or_else(|| "Error fetching rewards")?;
                let reward_amount = if rewards.is_empty() { 0 } else { rewards[0].lamports };
                
                // Extract vote latency from transactions
                let mut vote_latency = None;
                if let Some(transactions) = block.transactions {
                    for tx_with_meta in transactions.iter() {
                        if let EncodedTransaction::Json(tx_json) = &tx_with_meta.transaction {
                            let message = &tx_json.message;
                            let account_keys = match message {
                                UiMessage::Parsed(msg) => &msg.account_keys,
                                _ => continue,
                            };
                            
                            // Check if this transaction is signed by our validator's identity key
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
                                    if let Ok(instruction_json) = serde_json::to_string(&instruction) {
                                        if let Ok(instruction_value) = serde_json::from_str::<serde_json::Value>(&instruction_json) {
                                            if let Some(program) = instruction_value.get("program").and_then(|v| v.as_str()) {
                                                if program == "vote" {
                                                    // Found vote instruction, extract lockouts
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
                                                                    let latency = slot.saturating_sub(highest_slot);
                                                                    vote_latency = Some(latency);
                                                                    break; // Found vote latency, no need to check other instructions
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
                            
                            if vote_latency.is_some() {
                                break; // Found vote latency, no need to check other transactions
                            }
                        }
                    }
                }
                
                Ok((reward_amount, vote_latency))
            },
            Err(e) => {
                let error = e.to_string();
                if error.contains("skipped") {
                    Ok((0, None))
                } else {
                    Err(Box::new(e))
                }
            }
        }
    }

    pub async fn get_block_rewards_sum(
        &mut self,
        current_slot: u64,
        current_epoch: u64,
        leader_slots: Vec<u64>,
    ) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        // New epoch detection and reset
        if let Some(cached_epoch) = self.current_epoch {
            if current_epoch != cached_epoch {
                info!("New epoch detected: {} -> {}", cached_epoch, current_epoch);
                self.block_rewards.clear();
            }
        }
        self.current_epoch = Some(current_epoch);

        // Filter slots that need fetching: not already cached, not in future, and within reasonable range
        let slots_to_fetch: Vec<u64> = leader_slots
            .iter()
            .filter(|&slot| {
                !self.block_rewards.contains_key(slot) 
                && *slot <= current_slot 
                && *slot > current_slot.saturating_sub(1000) // Only fetch recent slots
            })
            .copied()
            .collect();

        if slots_to_fetch.is_empty() {
            let sum: i64 = self.block_rewards.values().sum();
            return Ok(sum);
        }

        // Sort by descending order (newest first) to prioritize recent slots
        let mut sorted_slots = slots_to_fetch;
        sorted_slots.sort_by(|a, b| b.cmp(a));

        // Log how far behind we are from the current slot
        if let Some(&newest_target_slot) = sorted_slots.first() {
            let slots_behind = current_slot.saturating_sub(newest_target_slot);
            info!("Latest Slot: {} | Target Slot: {} is {} slots behind | Fetching {} total slots", 
                  current_slot, newest_target_slot, slots_behind, sorted_slots.len());
        }

        // Process in batches to avoid overwhelming the RPC
        const BATCH_SIZE: usize = 10;
        let total_start = std::time::Instant::now();
        let mut total_fetched = 0;
        
        for batch in sorted_slots.chunks(BATCH_SIZE) {
            let batch_start = std::time::Instant::now();
            info!("Fetching block rewards for {} slots in parallel: {:?}", batch.len(), batch);
            
            // Create futures for parallel fetching
            let futures: Vec<_> = batch
                .iter()
                .map(|&slot| self.get_block_rewards(slot))
                .collect();

            // Execute all requests in parallel
            let results = futures::future::join_all(futures).await;
            
            let mut batch_success_count = 0;
            // Process results
            for (i, result) in results.into_iter().enumerate() {
                let slot = batch[i];
                match result {
                    Ok(rewards) => {
                        self.block_rewards.insert(slot, rewards);
                        batch_success_count += 1;
                    }
                    Err(e) => {
                        error!("Error fetching block rewards for slot {}: {}", slot, e);
                        // Continue with other slots, don't fail the entire batch
                    }
                }
            }

            let batch_duration = batch_start.elapsed();
            info!("Fetched {} out of {} slots in {:?} (avg: {:.1}ms per slot)", 
                  batch_success_count, batch.len(), batch_duration,
                  batch_duration.as_millis() as f64 / batch.len() as f64);
            
            total_fetched += batch_success_count;

            // Add a small delay between batches to be nice to the RPC
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        let total_duration = total_start.elapsed();
        if total_fetched > 0 {
            info!("Total: fetched {} slots in {:?} (avg: {:.1}ms per slot)", 
                  total_fetched, total_duration,
                  total_duration.as_millis() as f64 / total_fetched as f64);
        }

        let sum: i64 = self.block_rewards.values().sum();
        Ok(sum)
    }

    /// Optimized function that fetches blocks and extracts both rewards and vote latency in one pass
    /// Returns (total_rewards, latest_vote_latency) where vote_latency is Some(latency) if found, None otherwise
    pub async fn get_block_rewards_and_vote_latency_sum(
        &mut self,
        current_slot: u64,
        current_epoch: u64,
        leader_slots: Vec<u64>,
    ) -> Result<(i64, Option<u64>), Box<dyn std::error::Error + Send + Sync>> {
        // New epoch detection and reset
        if let Some(cached_epoch) = self.current_epoch {
            if current_epoch != cached_epoch {
                info!("New epoch detected: {} -> {}", cached_epoch, current_epoch);
                self.block_rewards.clear();
            }
        }
        self.current_epoch = Some(current_epoch);

        // Get all recent slots we need to check (last 200 slots)
        let recent_slots: Vec<u64> = (current_slot.saturating_sub(200)..=current_slot).collect();
        
        // Filter leader slots that need block rewards fetching (not already cached)
        let leader_slots_to_fetch: Vec<u64> = leader_slots
            .iter()
            .filter(|&slot| {
                recent_slots.contains(slot) && // Only check slots in our recent range
                !self.block_rewards.contains_key(slot) 
                && *slot <= current_slot 
            })
            .copied()
            .collect();

        // If no new leader slots to fetch and no recent slots to check, return early
        if leader_slots_to_fetch.is_empty() && recent_slots.is_empty() {
            let sum: i64 = self.block_rewards.values().sum();
            return Ok((sum, None));
        }

        info!("Checking {} recent slots for vote latency and {} leader slots for block rewards", 
              recent_slots.len(), leader_slots_to_fetch.len());

        // Process all slots in batches to avoid overwhelming the RPC
        const BATCH_SIZE: usize = 10;
        let total_start = std::time::Instant::now();
        let mut total_fetched = 0;
        let mut latest_vote_latency = None;
        
        // Process slots in batches
        for batch in recent_slots.chunks(BATCH_SIZE) {
            let batch_start = std::time::Instant::now();
            info!("Fetching {} slots in parallel: {:?}", batch.len(), batch);
            
            // Create futures for parallel fetching with full transaction details
            let futures: Vec<_> = batch
                .iter()
                .map(|&slot| self.get_block_rewards_and_vote_latency(slot))
                .collect();

            // Execute all requests in parallel
            let results = futures::future::join_all(futures).await;
            
            let mut batch_success_count = 0;
            // Process results
            for (i, result) in results.into_iter().enumerate() {
                let slot = batch[i];
                let is_leader_slot = leader_slots.contains(&slot);
                
                match result {
                    Ok((rewards, vote_latency)) => {
                        // Store block rewards if this is a leader slot and we need to fetch it
                        if is_leader_slot && leader_slots_to_fetch.contains(&slot) {
                            self.block_rewards.insert(slot, rewards);
                        }
                        
                        // Check for vote latency if this is NOT a leader slot
                        if !is_leader_slot && vote_latency.is_some() {
                            latest_vote_latency = vote_latency;
                            info!("Found vote latency in non-leader slot {}: {:?}", slot, vote_latency);
                        }
                        
                        batch_success_count += 1;
                    }
                    Err(e) => {
                        if !e.to_string().contains("Block not available") {
                            error!("Error fetching block data for slot {}: {}", slot, e);
                        }
                        // Continue with other slots, don't fail the entire batch
                    }
                }
            }

            let batch_duration = batch_start.elapsed();
            info!("Fetched {} out of {} slots in {:?} (avg: {:.1}ms per slot)", 
                  batch_success_count, batch.len(), batch_duration,
                  batch_duration.as_millis() as f64 / batch.len() as f64);
            
            total_fetched += batch_success_count;

            // Add a small delay between batches to be nice to the RPC
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        let total_duration = total_start.elapsed();
        if total_fetched > 0 {
            info!("Total: fetched {} slots in {:?} (avg: {:.1}ms per slot)", 
                  total_fetched, total_duration,
                  total_duration.as_millis() as f64 / total_fetched as f64);
        }

        let sum: i64 = self.block_rewards.values().sum();
        Ok((sum, latest_vote_latency))
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

    pub async fn get_vote_credit_rank(&self) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        let vote_accounts = self
            .client
            .get_vote_accounts_with_config(RpcGetVoteAccountsConfig {
                keep_unstaked_delinquents: Some(false),
                delinquent_slot_distance: Some(125),
                ..RpcGetVoteAccountsConfig::default()
            })
            .await?;
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
        let mut place = 0;
        for (index, (vote_pubkey, _)) in validator_credits.iter().enumerate() {
            if *vote_pubkey == *self.vote_account {
                place = (index + 1) as u32;
                break;
            }
        }
        Ok(place)
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
            return Ok::<i64, Box<dyn std::error::Error + Send + Sync>>(-1);
        }
        let samples = self.client.get_recent_performance_samples(Some(60)).await?;

        let (slots, secs) = samples.iter().fold(
            (0, 0u64),
            |(slots, secs): (u64, u64),
             RpcPerfSample {
                 num_slots,
                 sample_period_secs,
                 ..
             }| {
                (
                    slots.saturating_add(*num_slots),
                    secs.saturating_add((*sample_period_secs).into()),
                )
            },
        );
        let average_slot_time_ms = secs.saturating_mul(1000).checked_div(slots).unwrap_or(400);
        let mut next_slot: u64 = 0;
        for slot in leader_slots.iter() {
            if *slot > current_slot {
                next_slot = *slot;
                break;
            }
        }
        Ok(((next_slot - current_slot) * average_slot_time_ms) as i64)
    }

    pub async fn get_last_block_rewards(&self) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        let mut keys: Vec<u64> = self.block_rewards.keys().cloned().collect();
        keys.sort();
        let block_rewards: Vec<_> = keys
            .iter()
            .map(|&k| (k, self.block_rewards.get(&k).unwrap()))
            .filter(|(_, reward)| **reward > 0)
            .collect();

        let last_4 = &block_rewards[block_rewards.len().saturating_sub(4)..];
        let mut sum = 0;
        for (_, rewards) in last_4 {
            sum += **rewards;
        }
        Ok(sum / 4)
    }

    /// Returns the latest vote latency in slots (transaction_slot - voted_slot) for this validator.
    /// This method checks the provided slots for vote transactions from our validator.
    pub async fn get_latest_vote_latency_slots(&self, slots_to_check: &[u64]) -> Result<Option<u64>, Box<dyn std::error::Error + Send + Sync>> {
        log::info!("Checking {} slots for vote transactions signed by {}", slots_to_check.len(), self.identity_account);
        for &slot in slots_to_check.iter().rev() {
            match self.client.get_block_with_config(
                slot,
                RpcBlockConfig {
                    encoding: Some(UiTransactionEncoding::JsonParsed),
                    transaction_details: Some(TransactionDetails::Full),
                    rewards: Some(false),
                    commitment: None,
                    max_supported_transaction_version: Some(0),
                },
            ).await {
                Ok(block) => {
                    log::debug!("Got block for slot {} with {} transactions", slot, 
                               block.transactions.as_ref().map(|t| t.len()).unwrap_or(0));
                    if let Some(transactions) = block.transactions {
                        for (tx_idx, tx_with_meta) in transactions.iter().enumerate() {
                            log::debug!("Processing transaction {} in slot {}", tx_idx, slot);
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
                                    UiMessage::Parsed(msg) => {
                                        log::debug!("Found {} parsed instructions in transaction {}", msg.instructions.len(), tx_idx);
                                        &msg.instructions
                                    },
                                    _ => {
                                        log::debug!("Transaction {} has non-parsed message format", tx_idx);
                                        continue
                                    },
                                };
                                
                                log::info!("Found transaction signed by our validator in slot {} with {} instructions", slot, instructions.len());
                                
                                for (instr_idx, instr) in instructions.iter().enumerate() {
                                    if let UiInstruction::Parsed(instruction) = instr {
                                        // Convert the instruction to JSON string to access its fields
                                        if let Ok(instruction_json) = serde_json::to_string(&instruction) {
                                            log::debug!("Processing instruction {} in slot {}: {}", instr_idx, slot, instruction_json);
                                            if let Ok(instruction_value) = serde_json::from_str::<serde_json::Value>(&instruction_json) {
                                                if let Some(program) = instruction_value.get("program").and_then(|v| v.as_str()) {
                                                    log::debug!("Instruction {} program: {}", instr_idx, program);
                                                    if program == "vote" {
                                                        log::info!("Found vote instruction in slot {}!", slot);
                                                        log::debug!("Found vote instruction, checking vote account");
                                                        log::debug!("Full vote instruction structure: {}", serde_json::to_string(&instruction_value).unwrap_or_default());
                                                        
                                                        // Since we already verified the vote account is in the transaction's account keys,
                                                        // we can proceed to extract the lockouts
                                                        log::info!("Found vote instruction from our validator in slot {}", slot);
                                                        
                                                        // Try to find lockouts in the vote instruction
                                                        let lockouts = instruction_value
                                                            .get("parsed")
                                                            .and_then(|p| p.get("info"))
                                                            .and_then(|i| i.get("towerSync"))
                                                            .and_then(|t| t.get("lockouts"));
                                                        
                                                        if let Some(lockouts) = lockouts {
                                                            if let Some(lockouts_arr) = lockouts.as_array() {
                                                                log::info!("Found {} lockouts in vote instruction", lockouts_arr.len());
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
                                                                        let latency = slot.saturating_sub(highest_slot);
                                                                        log::info!("Vote latency found: {} slots (tx slot: {}, voted slot: {})", latency, slot, highest_slot);
                                                                        return Ok(Some(latency));
                                                                    } else {
                                                                        log::warn!("No valid slots found in lockouts array");
                                                                    }
                                                                } else {
                                                                    log::warn!("Lockouts array is empty");
                                                                }
                                                            } else {
                                                                log::warn!("Lockouts is not an array");
                                                            }
                                                        } else {
                                                            log::warn!("No lockouts found in vote instruction");
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
                Err(e) => {
                    if !e.to_string().contains("Block not available") {
                        log::debug!("Error fetching block {}: {}", slot, e);
                    }
                }
            }
        }
        log::warn!("No recent vote latency found for identity account {} in the checked slots", self.identity_account);
        Ok(None)
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
                    for (tx_idx, tx_with_meta) in transactions.iter().enumerate() {
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
                            
                            for (_instr_idx, instr) in instructions.iter().enumerate() {
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

    /// Efficiently processes multiple slots, extracting both block rewards and vote latency from each slot.
    /// This method fetches each slot only once and extracts both metrics based on whether it's a leader slot.
    pub async fn get_efficient_slot_metrics(
        &mut self,
        current_slot: u64,
        current_epoch: u64,
        leader_slots: Vec<u64>,
    ) -> Result<(i64, Option<u64>), Box<dyn std::error::Error + Send + Sync>> {
        // New epoch detection and reset
        if let Some(cached_epoch) = self.current_epoch {
            if current_epoch != cached_epoch {
                info!("New epoch detected: {} -> {}", cached_epoch, current_epoch);
                self.block_rewards.clear();
            }
        }
        self.current_epoch = Some(current_epoch);

        // Get all recent slots we need to check (last 200 slots)
        let recent_slots: Vec<u64> = (current_slot.saturating_sub(200)..=current_slot).collect();
        
        // Filter leader slots that need block rewards fetching (not already cached)
        let leader_slots_to_fetch: Vec<u64> = leader_slots
            .iter()
            .filter(|&slot| {
                recent_slots.contains(slot) && // Only check slots in our recent range
                !self.block_rewards.contains_key(slot) 
                && *slot <= current_slot 
            })
            .copied()
            .collect();

        info!("Processing {} recent slots for metrics ({} leader slots to fetch)", 
              recent_slots.len(), leader_slots_to_fetch.len());

        // Process all slots in batches to avoid overwhelming the RPC
        const BATCH_SIZE: usize = 10;
        let total_start = std::time::Instant::now();
        let mut total_fetched = 0;
        let mut latest_vote_latency = None;
        
        // Process slots in batches
        for batch in recent_slots.chunks(BATCH_SIZE) {
            let batch_start = std::time::Instant::now();
            info!("Fetching {} slots in parallel: {:?}", batch.len(), batch);
            
            // Create futures for parallel fetching with single fetch per slot
            let futures: Vec<_> = batch
                .iter()
                .map(|&slot| {
                    let is_leader_slot = leader_slots.contains(&slot);
                    self.get_slot_metrics(slot, is_leader_slot)
                })
                .collect();

            // Execute all requests in parallel
            let results = futures::future::join_all(futures).await;
            
            let mut batch_success_count = 0;
            // Process results
            for (i, result) in results.into_iter().enumerate() {
                let slot = batch[i];
                let is_leader_slot = leader_slots.contains(&slot);
                
                match result {
                    Ok((block_rewards, vote_latency)) => {
                        // Store block rewards if this is a leader slot and we need to fetch it
                        if is_leader_slot && leader_slots_to_fetch.contains(&slot) {
                            if let Some(rewards) = block_rewards {
                                self.block_rewards.insert(slot, rewards);
                            }
                        }
                        
                        // Check for vote latency if this is NOT a leader slot
                        if !is_leader_slot && vote_latency.is_some() {
                            latest_vote_latency = vote_latency;
                            info!("Found vote latency in non-leader slot {}: {:?}", slot, vote_latency);
                        }
                        
                        batch_success_count += 1;
                    }
                    Err(e) => {
                        if !e.to_string().contains("Block not available") {
                            error!("Error fetching block data for slot {}: {}", slot, e);
                        }
                        // Continue with other slots, don't fail the entire batch
                    }
                }
            }

            let batch_duration = batch_start.elapsed();
            info!("Fetched {} out of {} slots in {:?} (avg: {:.1}ms per slot)", 
                  batch_success_count, batch.len(), batch_duration,
                  batch_duration.as_millis() as f64 / batch.len() as f64);
            
            total_fetched += batch_success_count;

            // Add a small delay between batches to be nice to the RPC
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        let total_duration = total_start.elapsed();
        if total_fetched > 0 {
            info!("Total: fetched {} slots in {:?} (avg: {:.1}ms per slot)", 
                  total_fetched, total_duration,
                  total_duration.as_millis() as f64 / total_fetched as f64);
        }

        let sum: i64 = self.block_rewards.values().sum();
        Ok((sum, latest_vote_latency))
    }

    /// Fetches a single slot and extracts both block rewards and vote latency from the same block data.
    /// This is more efficient than fetching the same slot multiple times.
    pub async fn get_slot_metrics(&self, slot: u64, is_leader_slot: bool) -> Result<(Option<i64>, Option<u64>), Box<dyn std::error::Error + Send + Sync>> {
        match self.client.get_block_with_config(
            slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::JsonParsed),
                transaction_details: Some(TransactionDetails::Full),
                rewards: Some(true), // We need rewards for block rewards calculation
                commitment: None,
                max_supported_transaction_version: Some(0),
            },
        ).await {
            Ok(block) => {
                let mut block_rewards = None;
                let mut vote_latency = None;
                
                // Extract block rewards if this is a leader slot
                if is_leader_slot {
                    if let Some(rewards) = block.rewards {
                        let validator_rewards: i64 = rewards
                            .iter()
                            .filter(|reward| reward.pubkey == self.identity_account)
                            .map(|reward| reward.lamports)
                            .sum();
                        block_rewards = Some(validator_rewards);
                    }
                }
                
                // Extract vote latency if this is NOT a leader slot (validators don't vote on their own blocks)
                if !is_leader_slot {
                    if let Some(transactions) = block.transactions {
                        for (tx_idx, tx_with_meta) in transactions.iter().enumerate() {
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
                                
                                for (instr_idx, instr) in instructions.iter().enumerate() {
                                    if let UiInstruction::Parsed(instruction) = instr {
                                        // Convert the instruction to JSON string to access its fields
                                        if let Ok(instruction_json) = serde_json::to_string(&instruction) {
                                            if let Ok(instruction_value) = serde_json::from_str::<serde_json::Value>(&instruction_json) {
                                                if let Some(program) = instruction_value.get("program").and_then(|v| v.as_str()) {
                                                    if program == "vote" {
                                                        log::info!("Found vote instruction from our validator in slot {} (non-leader slot)", slot);
                                                        
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
                                                                        let latency = slot.saturating_sub(highest_slot);
                                                                        log::info!("Vote latency found: {} slots (tx slot: {}, voted slot: {})", latency, slot, highest_slot);
                                                                        vote_latency = Some(latency);
                                                                        break; // Found vote latency, no need to check more instructions
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
                                
                                // If we found vote latency, no need to check more transactions
                                if vote_latency.is_some() {
                                    break;
                                }
                            }
                        }
                    }
                }
                
                Ok((block_rewards, vote_latency))
            }
            Err(e) => {
                if !e.to_string().contains("Block not available") {
                    log::debug!("Error fetching block {}: {}", slot, e);
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
    
    pub async fn process_slot(&self, slot: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
        
        log::info!("Initializing epoch-based block rewards for validator {} in epoch {} (slots {}-{})", 
                   client.identity_account, current_epoch, first_slot, 
                   epoch_schedule.get_last_slot_in_epoch(current_epoch));
        
        let mut rewards = Self {
            client,
            current_epoch,
            last_processed_slot: first_slot - 1, // Start from before first slot to process all
            epoch_schedule,
            total_block_rewards: 0,
            on_block_rewards_update: None,
        };
        
        // Initialize with 0 rewards
        if let Some(on_update) = &rewards.on_block_rewards_update {
            on_update(0);
        }
        
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
        
        // Get leader slots for current epoch
        let first_slot = self.epoch_schedule.get_first_slot_in_epoch(current_epoch);
        let last_slot = self.epoch_schedule.get_last_slot_in_epoch(current_epoch);
        
        // Get leader schedule for this epoch
        let leader_schedule = self.client.client.get_leader_schedule(Some(first_slot)).await?
            .ok_or("Failed to get leader schedule")?;
        
        log::debug!("Got leader schedule for epoch {}", current_epoch);
        
        // Extract leader slots for our validator
        let leader_slots: Vec<u64> = leader_schedule
            .get(&self.client.identity_account)
            .map(|slots| slots.iter().map(|&slot| slot as u64 + first_slot).collect())
            .unwrap_or_default();
        
        log::debug!("Processing epoch {} block rewards: {} leader slots, last processed: {}, current: {}", 
                   current_epoch, leader_slots.len(), self.last_processed_slot, current_slot);
        
        // Process new leader slots incrementally
        let mut new_rewards = 0i64;
        let mut processed_count = 0;
        
        log::debug!("Starting to process {} leader slots", leader_slots.len());
        
        // Process only a small number of leader slots at a time to avoid taking too long
        let max_slots_to_process = 10; // Process max 10 slots per cycle
        let mut slots_processed = 0;
        
        for &leader_slot in &leader_slots {
            // Only process slots we haven't processed yet and that are not in the future
            if leader_slot > self.last_processed_slot && leader_slot <= current_slot {
                log::debug!("Processing leader slot {}", leader_slot);
                match self.client.get_block_rewards(leader_slot).await {
                    Ok(rewards) => {
                        if rewards > 0 {
                            log::info!("Found block rewards: {} in leader slot {}", rewards, leader_slot);
                            new_rewards += rewards;
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
        
        // Update last processed slot to current slot (but don't go beyond last slot of epoch)
        self.last_processed_slot = std::cmp::min(current_slot, last_slot);
        
        Ok(())
    }
    
    pub async fn handle_epoch_change(&mut self, new_epoch: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        log::info!("Epoch changed from {} to {} for block rewards tracking", self.current_epoch, new_epoch);
        
        // Reset for new epoch
        self.current_epoch = new_epoch;
        self.total_block_rewards = 0;
        
        let first_slot = self.epoch_schedule.get_first_slot_in_epoch(new_epoch);
        self.last_processed_slot = first_slot - 1; // Start from before first slot
        
        log::info!("Reset block rewards tracking for epoch {} (slots {}-{})", 
                  new_epoch, first_slot, self.epoch_schedule.get_last_slot_in_epoch(new_epoch));
        
        // Call callback with reset value
        if let Some(on_update) = &self.on_block_rewards_update {
            on_update(0);
        }
        
        Ok(())
    }
    
    pub fn get_total_block_rewards(&self) -> i64 {
        self.total_block_rewards
    }
}
