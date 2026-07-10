//! NeuMan Hub executable entry point.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    neuman::hub::run_from_env().await
}
