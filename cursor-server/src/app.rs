use std::{future::IntoFuture, time::Duration};

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    control,
    cursor::{
        handlers,
        prompting::{PromptAssets, PromptCompiler},
        CursorSessionRegistry,
    },
    provider::ProviderRouter,
    run::RunRegistry,
    store::Store,
    Result,
};

pub struct App {
    config: Config,
    router: axum::Router,
    registry: CursorSessionRegistry,
}

impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let store = Store::connect(&config.database_url).await?;
        let assets = PromptAssets::embedded()?;
        let compiler = PromptCompiler::new(assets);
        let provider = std::sync::Arc::new(ProviderRouter::new(
            store.clone(),
            config.provider_request_timeout,
        ));
        let run_registry = RunRegistry::default();
        let registry = CursorSessionRegistry::new(store.clone(), provider, compiler, run_registry);
        let router = handlers::router(registry.clone())?.merge(control::router(store));
        Ok(Self {
            router,
            registry,
            config,
        })
    }

    pub async fn serve(self) -> Result<()> {
        let listener = TcpListener::bind(self.config.listen_addr).await?;
        tracing::info!(address = %self.config.listen_addr, "cursor server listening");
        let registry = self.registry;
        let shutdown = CancellationToken::new();
        let graceful = shutdown.clone();
        let server = axum::serve(listener, self.router)
            .with_graceful_shutdown(async move {
                graceful.cancelled().await;
            })
            .into_future();
        tokio::pin!(server);

        let signal = shutdown_signal(registry, shutdown);
        tokio::pin!(signal);
        tokio::select! {
            result = &mut server => result?,
            () = &mut signal => {
                match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
                    Ok(result) => result?,
                    Err(_) => tracing::warn!("graceful shutdown timed out; forcing server close"),
                }
            }
        }
        Ok(())
    }
}

async fn shutdown_signal(registry: CursorSessionRegistry, shutdown: CancellationToken) {
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
    shutdown.cancel();
    registry.shutdown().await;
}
