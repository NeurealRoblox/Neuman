//! Diagnostic/standalone host for the NeuMan loopback Studio bridge.
//!
//! The production desktop embeds the library and renders pairing approval in
//! its UI. This host intentionally requires an explicit console approval so it
//! remains safe for development and compatibility testing.

use std::{net::SocketAddr, path::PathBuf};

use neuman::bridge::{BridgeConfig, BridgeEvent, BridgeService};
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let discovery_addr: SocketAddr = std::env::var("NEUMAN_BRIDGE_DISCOVERY_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:34873".to_owned())
        .parse()?;
    let transfer_dir = std::env::var_os("NEUMAN_BRIDGE_TRANSFER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("neuman-studio-bridge"));
    let running = BridgeService::start(BridgeConfig {
        discovery_addr,
        transfer_dir,
        ..BridgeConfig::default()
    })
    .await?;
    let handle = running.handle.clone();
    let mut events = handle.subscribe();

    println!(
        "NeuMan Studio bridge discovery: http://{}",
        running.discovery_addr
    );
    println!(
        "NeuMan Studio bridge session:   ws://{}/v1/studio",
        running.session_addr
    );
    println!("Commands: approve <challenge> <plugin-id> | revoke <plugin-id> | quit");

    let event_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                BridgeEvent::PairingChallengeIssued {
                    challenge,
                    pairing_code,
                    expires_at,
                } => println!(
                    "PAIR CODE {pairing_code} for challenge {challenge} (expires {expires_at})"
                ),
                BridgeEvent::PairingRequested {
                    challenge,
                    plugin_installation_id,
                    studio_user_id,
                    ..
                } => println!(
                    "PAIR REQUEST challenge={challenge} plugin={plugin_installation_id} studio-user={studio_user_id:?}"
                ),
                BridgeEvent::Paired {
                    plugin_installation_id,
                } => println!("PAIRED plugin={plugin_installation_id}"),
                BridgeEvent::SessionConnected {
                    session_id,
                    plugin_installation_id,
                } => println!("CONNECTED session={session_id} plugin={plugin_installation_id}"),
                BridgeEvent::SessionDisconnected { session_id } => {
                    println!("DISCONNECTED session={session_id}")
                }
                BridgeEvent::CaptureProposal {
                    session_id,
                    cell_id,
                    transfer_id,
                    mutation_epoch,
                    ..
                } => println!(
                    "CAPTURE session={session_id} cell={cell_id} transfer={transfer_id} epoch={mutation_epoch}"
                ),
                BridgeEvent::ApplyReceipt {
                    session_id,
                    revision_id,
                    status,
                    verification,
                    ..
                } => println!(
                    "APPLY session={session_id} revision={revision_id} status={status} verification={verification}"
                ),
                BridgeEvent::TransferVerified {
                    session_id,
                    transfer_id,
                    content_hash,
                    size_bytes,
                    ..
                } => println!(
                    "TRANSFER session={session_id} id={transfer_id} hash={content_hash} bytes={size_bytes}"
                ),
                BridgeEvent::ProtocolViolation {
                    session_id, code, ..
                } => eprintln!("PROTOCOL VIOLATION session={session_id:?} code={code}"),
                BridgeEvent::StudioEvent { .. } => {}
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal?;
                break;
            }
            line = lines.next_line() => {
                let Some(line) = line? else { break; };
                let fields: Vec<_> = line.split_whitespace().collect();
                match fields.as_slice() {
                    ["approve", challenge, plugin] => match handle.approve_pairing(challenge, plugin).await {
                        Ok(()) => println!("Approved; retry pairing in Studio."),
                        Err(error) => eprintln!("Approval failed: {error}"),
                    },
                    ["revoke", plugin] => {
                        handle.revoke_plugin(plugin).await;
                        println!("Revoked {plugin}");
                    }
                    ["quit" | "exit"] => break,
                    [] => {}
                    _ => eprintln!("Expected: approve <challenge> <plugin-id> | revoke <plugin-id> | quit"),
                }
            }
        }
    }

    running.shutdown().await;
    event_task.abort();
    Ok(())
}
