use crate::solana;
use crate::solana::validator::{StakeState, SlotBasedMetrics, EpochBasedBlockRewards};
use axum::body::Body;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use log::error;
use prometheus_client::encoding::text::encode;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct MethodLabels {
    pub network: String,
    pub vote_account: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StakeLabels {
    pub network: String,
    pub stake_type: String,
    pub vote_account: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct BlockLabels {
    pub network: String,
    pub block_type: String,
    pub vote_account: String,
}

pub struct Metrics {
    network: String,
    rpc_url: String,
    identity_account: String,
    vote_account: String,
    pub slot: Family<MethodLabels, Gauge>,
    pub epoch: Family<MethodLabels, Gauge>,
    pub epoch_progress: Family<MethodLabels, Gauge>,
    pub stake: Family<StakeLabels, Gauge>,
    pub identity_balance: Family<MethodLabels, Gauge>,
    pub vote_account_balance: Family<MethodLabels, Gauge>,
    pub blocks: Family<BlockLabels, Gauge>,
    pub jito_tips: Family<MethodLabels, Gauge>,
    pub vote_credit_rank: Family<MethodLabels, Gauge>,
    pub usd_price: Family<MethodLabels, Gauge>,
    pub epoch_block_rewards: Family<MethodLabels, Gauge>,
    pub ms_to_next_slot: Family<MethodLabels, Gauge>,
    pub last_block_rewards: Family<MethodLabels, Gauge>,
    pub vote_latency_slots: Family<MethodLabels, Gauge>,
}

impl Metrics {
    pub fn new(network: String, rpc_url: String, identity_account: String, vote_account: String) -> Metrics {
        Metrics {
            network,
            rpc_url,
            identity_account,
            vote_account,
            slot: Family::default(),
            epoch: Family::default(),
            epoch_progress: Family::default(),
            stake: Family::default(),
            identity_balance: Family::default(),
            vote_account_balance: Family::default(),
            blocks: Family::default(),
            jito_tips: Family::default(),
            vote_credit_rank: Family::default(),
            usd_price: Family::default(),
            epoch_block_rewards: Family::default(),
            ms_to_next_slot: Family::default(),
            last_block_rewards: Family::default(),
            vote_latency_slots: Family::default(),
        }
    }

    pub async fn init_registry(&self, shared_state: Arc<Mutex<AppState>>) {
        let mut state = shared_state.lock().await;
        
        state
            .registry
            .register("solana_slot", "Slot of cluster", self.slot.clone());

        state
            .registry
            .register("solana_epoch", "Current epoch", self.epoch.clone());

        state.registry.register(
            "solana_epoch_progress",
            "Epoch progress",
            self.epoch_progress.clone(),
        );

        state
            .registry
            .register("solana_stake", "Stake info", self.stake.clone());

        state.registry.register(
            "solana_identity_balance",
            "Identity balance",
            self.identity_balance.clone(),
        );

        state.registry.register(
            "solana_vote_account_balance",
            "Vote account balance",
            self.vote_account_balance.clone(),
        );

        state
            .registry
            .register("solana_blocks", "Block production", self.blocks.clone());

        state
            .registry
            .register("solana_jito_tips", "Jito tips", self.jito_tips.clone());

        state.registry.register(
            "solana_vote_credit_rank",
            "Vote credit rank",
            self.vote_credit_rank.clone(),
        );

        state
            .registry
            .register("solana_usd_price", "USD Price", self.usd_price.clone());

        state.registry.register(
            "solana_epoch_block_rewards",
            "Sum of block rewards this epoch",
            self.epoch_block_rewards.clone(),
        );

        state.registry.register(
            "solana_ms_to_next_slot",
            "Time to next leader slot",
            self.ms_to_next_slot.clone(),
        );

        state.registry.register(
            "solana_last_block_rewards",
            "Average of last non-zero block rewards",
            self.last_block_rewards.clone(),
        );

        state.registry.register(
            "solana_vote_latency_slots",
            "Latest vote latency in slots (transaction_slot - voted_slot)",
            self.vote_latency_slots.clone(),
        );
    }

    pub fn run_loop(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Create separate clients for each system
            let vote_latency_client = solana::validator::SolanaClient::new(
                &self.rpc_url,
                &self.identity_account,
                &self.vote_account,
            );

            let block_rewards_client = solana::validator::SolanaClient::new(
                &self.rpc_url,
                &self.identity_account,
                &self.vote_account,
            );

            // Initialize slot-based metrics for vote latency (100ms intervals)
            let mut slot_based_metrics = match solana::validator::SlotBasedMetrics::new(vote_latency_client).await {
                Ok(mut metrics) => {
                    log::info!("Initialized slot-based vote latency metrics for validator {}", self.identity_account);
                    
                    // Set up callback for vote latency
                    let vote_latency_metric = self.vote_latency_slots.clone();
                    let network = self.network.clone();
                    let vote_account = self.vote_account.clone();
                    let slot_metric = self.slot.clone();
                    let network_slot = self.network.clone();
                    let vote_account_slot = self.vote_account.clone();
                    metrics.on_vote_latency = Some(Box::new(move |latency| {
                        vote_latency_metric
                            .get_or_create(&MethodLabels {
                                network: network.clone(),
                                vote_account: vote_account.clone(),
                            })
                            .set(latency as i64);
                        log::info!("Updated vote latency metric: {} slots", latency);
                    }));
                    
                    // Set up callback for slot updates
                    let slot_metric = self.slot.clone();
                    let network_slot = self.network.clone();
                    let vote_account_slot = self.vote_account.clone();
                    metrics.on_slot_update = Some(Box::new(move |slot| {
                        slot_metric
                            .get_or_create(&MethodLabels {
                                network: network_slot.clone(),
                                vote_account: vote_account_slot.clone(),
                            })
                            .set(slot as i64);
                    }));
                    
                    metrics
                }
                Err(e) => {
                    log::error!("Failed to initialize slot-based metrics: {}", e);
                    return;
                }
            };

            // Initialize epoch-based block rewards (5-second intervals)
            let mut epoch_block_rewards = match solana::validator::EpochBasedBlockRewards::new(block_rewards_client).await {
                Ok(mut rewards) => {
                    log::info!("Initialized epoch-based block rewards for validator {}", self.identity_account);
                    
                    // Set up callback for block rewards
                    let epoch_block_rewards_metric = self.epoch_block_rewards.clone();
                    let network_rewards = self.network.clone();
                    let vote_account_rewards = self.vote_account.clone();
                    rewards.on_block_rewards_update = Some(Box::new(move |total_rewards| {
                        epoch_block_rewards_metric
                            .get_or_create(&MethodLabels {
                                network: network_rewards.clone(),
                                vote_account: vote_account_rewards.clone(),
                            })
                            .set(total_rewards);
                        log::info!("Updated epoch block rewards metric: {}", total_rewards);
                    }));
                    
                    rewards
                }
                Err(e) => {
                    log::error!("Failed to initialize epoch-based block rewards: {}", e);
                    return;
                }
            };

            // Start both systems in separate tasks
            let slot_metrics_handle = {
                let mut slot_metrics = slot_based_metrics;
                tokio::spawn(async move {
                    slot_metrics.run_loop().await;
                })
            };

            let block_rewards_handle = {
                let mut block_rewards = epoch_block_rewards;
                tokio::spawn(async move {
                    block_rewards.run_loop().await;
                })
            };

            // Start the original metrics collection loop (30-second intervals)
            let metrics_handle = {
                let metrics = self.clone();
                let client = solana::validator::SolanaClient::new(
                    &self.rpc_url,
                    &self.identity_account,
                    &self.vote_account,
                );
                tokio::spawn(async move {
                    loop {
                        // Update all basic metrics
                        metrics.update_all_metrics(&client).await;
                        
                        // Wait 30 seconds before next update
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                })
            };

            // Wait for all tasks (they should run indefinitely)
            tokio::select! {
                _ = slot_metrics_handle => {
                    log::error!("Slot-based metrics task ended unexpectedly");
                }
                _ = block_rewards_handle => {
                    log::error!("Epoch-based block rewards task ended unexpectedly");
                }
                _ = metrics_handle => {
                    log::error!("Basic metrics collection task ended unexpectedly");
                }
            }
        })
    }

    async fn update_all_metrics(&self, client: &solana::validator::SolanaClient) {
        // Update epoch info
        if let Ok((epoch, epoch_progress)) = client.get_epoch().await {
            self.set_epoch(epoch);
            self.set_epoch_progress(epoch_progress);
            
            // Update jito tips for current epoch
            if let Ok(tips) = client.get_jito_tips(epoch).await {
                self.set_jito_tips(tips);
            }
        }

        // Update stake details
        if let Ok(stake_details) = client.get_stake_details().await {
            self.set_stake(stake_details);
        }

        // Update balances
        if let Ok(balance) = client.get_identity_balance().await {
            self.set_identity_balance(balance);
        }

        if let Ok(balance) = client.get_vote_balance().await {
            self.set_vote_account_balance(balance);
        }

        // Update block production
        if let Ok(block_production) = client.get_block_production().await {
            let (blocks_produced, blocks_total) = block_production;
            let blocks_skipped = blocks_total - blocks_produced;
            self.set_block_production(
                blocks_total as u64,
                blocks_produced as u64,
                blocks_skipped as u64,
            );
        }

        // Update vote credit rank
        if let Ok(rank) = client.get_vote_credit_rank().await {
            self.set_vote_credit_rank(rank);
        }

        // Update USD price
        if let Ok(price) = client.get_sol_usd_price().await {
            self.set_usd_price(price);
        }

        // Update ms to next slot
        if let Ok(leader_slots) = client.get_leader_info().await {
            if let Ok(current_slot) = client.get_slot().await {
                if let Ok(ms_to_next) = client.get_ms_to_next_slot(current_slot, leader_slots).await {
                    self.set_ms_to_next_slot(ms_to_next);
                }
            }
        }

        // Update last block rewards
        if let Ok(last_rewards) = client.get_last_block_rewards().await {
            self.set_last_block_rewards(last_rewards);
        }

        // Update vote latency slots (get latest from recent slots)
        let current_slot = match client.get_slot().await {
            Ok(slot) => slot,
            Err(_) => return,
        };
        
        // Check last 10 slots for vote latency
        let slots_to_check: Vec<u64> = (current_slot.saturating_sub(10)..=current_slot).collect();
        if let Ok(Some(latency)) = client.get_latest_vote_latency_slots(&slots_to_check).await {
            self.set_vote_latency_slots(latency);
        }
    }

    pub fn set_slot(&self, slot: u64) {
        self.slot
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(slot as i64);
    }

    pub fn set_epoch(&self, epoch: i64) {
        self.epoch
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(epoch);
    }

    pub fn set_epoch_progress(&self, progress: i64) {
        self.epoch_progress
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(progress);
    }

    pub fn set_stake(&self, stake_state: StakeState) {
        self.stake
            .get_or_create(&StakeLabels {
                network: self.network.clone(),
                stake_type: "activated".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(stake_state.activated_stake as i64);

        self.stake
            .get_or_create(&StakeLabels {
                network: self.network.clone(),
                stake_type: "activating".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(stake_state.activating_stake as i64);

        self.stake
            .get_or_create(&StakeLabels {
                network: self.network.clone(),
                stake_type: "deactivating".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(stake_state.deactivating_stake as i64);

        self.stake
            .get_or_create(&StakeLabels {
                network: self.network.clone(),
                stake_type: "locked".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(stake_state.locked_stake as i64);

        self.stake
            .get_or_create(&StakeLabels {
                network: self.network.clone(),
                stake_type: "activated_accounts".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(stake_state.activated_stake_accounts as i64);

        self.stake
            .get_or_create(&StakeLabels {
                network: self.network.clone(),
                stake_type: "activating_accounts".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(stake_state.activating_stake_accounts as i64);

        self.stake
            .get_or_create(&StakeLabels {
                network: self.network.clone(),
                stake_type: "deactivating_accounts".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(stake_state.deactivating_stake_accounts as i64);
    }

    pub fn set_identity_balance(&self, balance: u64) {
        self.identity_balance
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(balance as i64);
    }

    pub fn set_vote_account_balance(&self, balance: u64) {
        self.vote_account_balance
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(balance as i64);
    }

    pub fn set_block_production(&self, total: u64, produced: u64, skipped: u64) {
        self.blocks
            .get_or_create(&BlockLabels {
                network: self.network.clone(),
                block_type: "total".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(total as i64);

        self.blocks
            .get_or_create(&BlockLabels {
                network: self.network.clone(),
                block_type: "produced".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(produced as i64);

        self.blocks
            .get_or_create(&BlockLabels {
                network: self.network.clone(),
                block_type: "skipped".to_string(),
                vote_account: self.vote_account.clone(),
            })
            .set(skipped as i64);
    }

    pub fn set_jito_tips(&self, tips: u64) {
        self.jito_tips
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(tips as i64);
    }

    pub fn set_vote_credit_rank(&self, rank: u32) {
        self.vote_credit_rank
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(rank as i64);
    }

    pub fn set_usd_price(&self, price: i64) {
        self.usd_price
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(price);
    }

    pub fn set_epoch_block_rewards(&self, block_rewards: i64) {
        self.epoch_block_rewards
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(block_rewards);
    }

    pub fn set_ms_to_next_slot(&self, ms_to_next_slot: i64) {
        self.ms_to_next_slot
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(ms_to_next_slot);
    }

    pub fn set_last_block_rewards(&self, last_block_rewards: i64) {
        self.last_block_rewards
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(last_block_rewards);
    }

    pub fn set_vote_latency_slots(&self, latency: u64) {
        self.vote_latency_slots
            .get_or_create(&MethodLabels {
                network: self.network.clone(),
                vote_account: self.vote_account.clone(),
            })
            .set(latency as i64);
    }
}

pub struct AppState {
    pub registry: Registry,
}

pub async fn metrics_handler(State(state): State<Arc<Mutex<AppState>>>) -> impl IntoResponse {
    let state = state.lock().await;
    let mut body = String::new();
    if let Err(e) = encode(&mut body, &state.registry) {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!("Error encoding metrics: {}", e)))
            .unwrap();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/openmetrics-text")
        .body(Body::from(body))
        .unwrap()
}
