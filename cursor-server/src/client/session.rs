use tokio::sync::mpsc;

use super::{ClientCommand, ClientEvent};

pub struct ClientPort {
    pub commands: mpsc::Receiver<ClientCommand>,
    pub events: mpsc::Sender<ClientEvent>,
}

pub struct ClientSession {
    pub commands: mpsc::Sender<ClientCommand>,
    pub events: mpsc::Receiver<ClientEvent>,
}

pub fn session(capacity: usize) -> (ClientPort, ClientSession) {
    let (commands_tx, commands_rx) = mpsc::channel(capacity);
    let (events_tx, events_rx) = mpsc::channel(capacity);
    (
        ClientPort {
            commands: commands_rx,
            events: events_tx,
        },
        ClientSession {
            commands: commands_tx,
            events: events_rx,
        },
    )
}
