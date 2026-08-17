#![allow(dead_code)]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use cursor_server::{
    model::{ModelInvocation, ModelRequest},
    provider::{ModelEvent, Provider, ProviderStream},
    Error,
};
use futures_util::stream;
use tokio_util::sync::CancellationToken;

type FakeResponse = Vec<Result<ModelEvent, Error>>;

#[derive(Clone, Default)]
pub struct FakeProvider {
    responses: Arc<Mutex<VecDeque<FakeResponse>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl FakeProvider {
    pub fn push(&self, events: Vec<ModelEvent>) {
        self.responses
            .lock()
            .unwrap()
            .push_back(events.into_iter().map(Ok).collect());
    }
    pub fn push_error(&self, error: Error) {
        self.responses.lock().unwrap().push_back(vec![Err(error)]);
    }
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Provider for FakeProvider {
    fn stream(
        &self,
        invocation: ModelInvocation,
        _cancellation: CancellationToken,
    ) -> ProviderStream {
        self.requests.lock().unwrap().push(invocation.request);
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("fake response configured");
        Box::pin(stream::iter(events))
    }
}
