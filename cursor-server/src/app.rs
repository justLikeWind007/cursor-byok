use tokio::net::TcpListener;

use crate::{
    config::Config,
    cursor::handlers,
    prompting::{PromptAssets, PromptCompiler},
    provider::build_provider,
    run::RunRegistry,
    store::Store,
    Result,
};

pub struct App {
    config: Config,
    router: axum::Router,
    registry: RunRegistry,
}

impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let store = Store::connect(&config.database_url).await?;
        let assets = PromptAssets::embedded()?;
        let compiler = PromptCompiler::new(assets);
        let provider = build_provider(&config.provider)?;
        let registry = RunRegistry::new(store, provider, compiler, config.provider.model.clone());
        Ok(Self {
            router: handlers::router(registry.clone()),
            registry,
            config,
        })
    }

    pub async fn serve(self) -> Result<()> {
        let listener = TcpListener::bind(self.config.listen_addr).await?;
        tracing::info!(address = %self.config.listen_addr, "cursor server listening");
        let registry = self.registry;
        axum::serve(listener, self.router)
            .with_graceful_shutdown(shutdown_signal(registry))
            .await?;
        Ok(())
    }
}

async fn shutdown_signal(registry: RunRegistry) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
    tracing::info!("shutdown signal received; cancelling active runs");
    registry.shutdown().await;
}
