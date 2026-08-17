use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::model::{ConversationId, RunId};

#[derive(Clone, Default)]
pub struct RunRegistry {
    active: Arc<Mutex<HashMap<ConversationId, ActiveRun>>>,
}

struct ActiveRun {
    run_id: RunId,
    cancellation: CancellationToken,
}

impl RunRegistry {
    pub async fn activate(
        &self,
        conversation_id: ConversationId,
        run_id: RunId,
        cancellation: CancellationToken,
    ) {
        let previous = self.active.lock().await.insert(
            conversation_id,
            ActiveRun {
                run_id: run_id.clone(),
                cancellation,
            },
        );
        if let Some(previous) = previous.filter(|previous| previous.run_id != run_id) {
            previous.cancellation.cancel();
        }
    }

    pub async fn release(&self, conversation_id: &ConversationId, run_id: &RunId) {
        let mut active = self.active.lock().await;
        if active
            .get(conversation_id)
            .is_some_and(|current| &current.run_id == run_id)
        {
            active.remove(conversation_id);
        }
    }

    pub async fn shutdown(&self) {
        let active = std::mem::take(&mut *self.active.lock().await);
        for run in active.into_values() {
            run.cancellation.cancel();
        }
    }
}
