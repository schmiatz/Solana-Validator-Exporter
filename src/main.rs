mod metrics;
mod solana;
use axum::routing::get;
use axum::Router;
use clap::Parser;
use env_logger::Env;
use log::info;
use metrics::exporter::metrics_handler;
use serde::Deserialize;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Prometheus exporter for solana validators
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct CliArgs {
    /// Path to config file
    #[arg(long)]
    config_file: String,
}

#[derive(Debug, Deserialize)]
struct Config {
    port: i32,
    // Network RPC URLs
    rpc_url_mainnet: Option<String>,
    rpc_url_testnet: Option<String>,
    rpc_url_devnet: Option<String>,
    // Mainnet validator accounts - now supports multiple validators
    mainnet_validators: Option<Vec<ValidatorAccount>>,
    // Legacy single validator support (for backward compatibility)
    mainnet_vote_account: Option<String>,
    mainnet_identity_account: Option<String>,
    // Testnet validator accounts - now supports multiple validators
    testnet_validators: Option<Vec<ValidatorAccount>>,
    // Legacy single validator support (for backward compatibility)
    testnet_vote_account: Option<String>,
    testnet_identity_account: Option<String>,
    // Devnet validator accounts - now supports multiple validators
    devnet_validators: Option<Vec<ValidatorAccount>>,
    // Legacy single validator support (for backward compatibility)
    devnet_vote_account: Option<String>,
    devnet_identity_account: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidatorAccount {
    vote_account: String,
    identity_account: String,
}

#[derive(Debug, Clone)]
struct ValidatorConfig {
    network: String,
    rpc_url: String,
    vote_account: String,
    identity_account: String,
}

#[tokio::main]
async fn main() {
    let args = CliArgs::parse();
    let file = File::open(args.config_file).unwrap();
    let reader = BufReader::new(file);
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    // Parse the YAML file
    let config: Config = serde_yaml::from_reader(reader).expect("Error parsing yaml file");

    // Debug: Print the parsed config to see what was read
    println!("Debug: Parsed config: {:?}", config);

    // Validate configuration and build validator configs
    let mut validator_configs = Vec::new();

    // Check mainnet configuration
    if let Some(rpc_url) = &config.rpc_url_mainnet {
        println!("Debug: Found mainnet RPC URL: {}", rpc_url);
        // First check for new array format
        if let Some(validators) = &config.mainnet_validators {
            println!("Debug: Found {} mainnet validators in array format", validators.len());
            for validator in validators {
                validator_configs.push(ValidatorConfig {
                    network: "mainnet".to_string(),
                    rpc_url: rpc_url.clone(),
                    vote_account: validator.vote_account.clone(),
                    identity_account: validator.identity_account.clone(),
                });
            }
        }
        // Fall back to legacy single validator format
        else if let (Some(vote_account), Some(identity_account)) = (
            &config.mainnet_vote_account,
            &config.mainnet_identity_account,
        ) {
            println!("Debug: Found mainnet validator in legacy format: {} / {}", vote_account, identity_account);
            validator_configs.push(ValidatorConfig {
                network: "mainnet".to_string(),
                rpc_url: rpc_url.clone(),
                vote_account: vote_account.clone(),
                identity_account: identity_account.clone(),
            });
        } else {
            println!("Debug: No mainnet validators found in either format");
        }
    } else {
        println!("Debug: No mainnet RPC URL found");
    }

    // Check testnet configuration
    if let Some(rpc_url) = &config.rpc_url_testnet {
        // First check for new array format
        if let Some(validators) = &config.testnet_validators {
            for validator in validators {
                validator_configs.push(ValidatorConfig {
                    network: "testnet".to_string(),
                    rpc_url: rpc_url.clone(),
                    vote_account: validator.vote_account.clone(),
                    identity_account: validator.identity_account.clone(),
                });
            }
        }
        // Fall back to legacy single validator format
        else if let (Some(vote_account), Some(identity_account)) = (
            &config.testnet_vote_account,
            &config.testnet_identity_account,
        ) {
            validator_configs.push(ValidatorConfig {
                network: "testnet".to_string(),
                rpc_url: rpc_url.clone(),
                vote_account: vote_account.clone(),
                identity_account: identity_account.clone(),
            });
        }
    }

    // Check devnet configuration
    if let Some(rpc_url) = &config.rpc_url_devnet {
        // First check for new array format
        if let Some(validators) = &config.devnet_validators {
            for validator in validators {
                validator_configs.push(ValidatorConfig {
                    network: "devnet".to_string(),
                    rpc_url: rpc_url.clone(),
                    vote_account: validator.vote_account.clone(),
                    identity_account: validator.identity_account.clone(),
                });
            }
        }
        // Fall back to legacy single validator format
        else if let (Some(vote_account), Some(identity_account)) = (
            &config.devnet_vote_account,
            &config.devnet_identity_account,
        ) {
            validator_configs.push(ValidatorConfig {
                network: "devnet".to_string(),
                rpc_url: rpc_url.clone(),
                vote_account: vote_account.clone(),
                identity_account: identity_account.clone(),
            });
        }
    }

    if validator_configs.is_empty() {
        panic!("At least one complete validator configuration (RPC URL, vote account, and identity account) must be provided");
    }

    info!("Starting exporter with {} validator(s)!", validator_configs.len());
    
    // Create shared metrics registry
    let shared_state = Arc::new(Mutex::new(metrics::exporter::AppState {
        registry: prometheus_client::registry::Registry::default(),
    }));
    
    // Create and start metrics collection for each validator
    let mut handles = Vec::new();
    for validator_config in validator_configs {
        info!("Starting metrics collection for {} validator: {}", 
              validator_config.network, validator_config.vote_account);
        
        let metrics = Arc::new(metrics::exporter::Metrics::new(
            validator_config.network,
            validator_config.rpc_url,
            validator_config.identity_account,
            validator_config.vote_account,
        ));
        
        // Initialize metrics in shared registry
        metrics.init_registry(shared_state.clone()).await;
        
        // Start metrics collection loop
        let handle = metrics.clone().run_loop();
        handles.push(handle);
    }

    let router = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(shared_state.clone());
    
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", config.port))
        .await
        .unwrap();

    axum::serve(listener, router).await.unwrap();
}
