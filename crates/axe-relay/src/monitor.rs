use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_EVENTS: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Client {
    pub id: u64,
    pub client_id: String,
    pub transport: String,
    pub peer: String,
    pub public_address: String,
    pub connected_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Status {
    pub version: u8,
    pub uptime_seconds: u64,
    pub tcp_control: SocketAddr,
    pub quic_control: SocketAddr,
    pub clients: Vec<Client>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Event {
    pub seq: u64,
    pub kind: EventKind,
    pub client: Client,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EventBatch {
    pub epoch: Uuid,
    pub cursor: u64,
    pub clients: Vec<Client>,
    pub events: Vec<Event>,
}

struct Session {
    client_id: String,
    transport: &'static str,
    peer: String,
    public_address: String,
    connected_at: Instant,
}

#[derive(Default)]
struct Sessions {
    next_id: u64,
    active: HashMap<u64, Session>,
    last_event: u64,
    events: VecDeque<Event>,
}

impl Sessions {
    fn clients(&self) -> Vec<Client> {
        let mut clients: Vec<_> = self
            .active
            .iter()
            .map(|(&id, session)| Client {
                id,
                client_id: session.client_id.clone(),
                transport: session.transport.to_owned(),
                peer: session.peer.clone(),
                public_address: session.public_address.clone(),
                connected_seconds: session.connected_at.elapsed().as_secs(),
            })
            .collect();
        clients.sort_unstable_by_key(|client| client.id);
        clients
    }

    fn record(&mut self, kind: EventKind, client: Client) {
        self.last_event = self
            .last_event
            .checked_add(1)
            .expect("relay event ID exhausted");
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            seq: self.last_event,
            kind,
            client,
        });
    }
}

pub struct Monitor {
    epoch: Uuid,
    started_at: Instant,
    sessions: Mutex<Sessions>,
    addresses: Mutex<Option<(SocketAddr, SocketAddr)>>,
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            epoch: Uuid::new_v4(),
            sessions: Mutex::new(Sessions::default()),
            addresses: Mutex::new(None),
        }
    }

    pub fn set_addresses(&self, tcp: SocketAddr, quic: SocketAddr) {
        *self
            .addresses
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((tcp, quic));
    }

    pub fn snapshot(&self) -> Option<Status> {
        let (tcp_control, quic_control) = (*self
            .addresses
            .lock()
            .unwrap_or_else(|error| error.into_inner()))?;
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Some(Status {
            version: 1,
            uptime_seconds: self.started_at.elapsed().as_secs(),
            tcp_control,
            quic_control,
            clients: sessions.clients(),
        })
    }

    pub fn events(&self, after: Option<(Uuid, u64)>) -> Result<EventBatch, &'static str> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let cursor = sessions.last_event;
        let Some((epoch, after)) = after else {
            return Ok(EventBatch {
                epoch: self.epoch,
                cursor,
                clients: sessions.clients(),
                events: Vec::new(),
            });
        };
        if epoch != self.epoch
            || after > cursor
            || sessions
                .events
                .front()
                .is_some_and(|oldest| after < oldest.seq - 1)
        {
            return Err("event cursor expired or invalid; restart watch");
        }
        Ok(EventBatch {
            epoch: self.epoch,
            cursor,
            clients: Vec::new(),
            events: sessions
                .events
                .iter()
                .filter(|event| event.seq > after)
                .cloned()
                .collect(),
        })
    }

    pub fn register(
        &self,
        transport: &'static str,
        client_id: &str,
        peer: &str,
        public_address: String,
    ) -> RegistrationGuard<'_> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let id = sessions.next_id;
        sessions.next_id = id.checked_add(1).expect("relay registration ID exhausted");
        println!(
            "relay: registered client={client_id:?} transport={transport} from {peer}; public {public_address}; active_clients={}",
            sessions.active.len() + 1
        );
        sessions.active.insert(
            id,
            Session {
                client_id: client_id.to_owned(),
                transport,
                peer: peer.to_owned(),
                public_address,
                connected_at: Instant::now(),
            },
        );
        let session = &sessions.active[&id];
        let client = Client {
            id,
            client_id: session.client_id.clone(),
            transport: session.transport.to_owned(),
            peer: session.peer.clone(),
            public_address: session.public_address.clone(),
            connected_seconds: 0,
        };
        sessions.record(EventKind::Connected, client);
        RegistrationGuard { monitor: self, id }
    }
}

pub struct RegistrationGuard<'a> {
    monitor: &'a Monitor,
    id: u64,
}

impl Drop for RegistrationGuard<'_> {
    fn drop(&mut self) {
        let mut sessions = self
            .monitor
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(session) = sessions.active.remove(&self.id) {
            println!(
                "relay: disconnected client={:?} transport={} from {}; public {}; connected={}s; active_clients={}",
                session.client_id,
                session.transport,
                session.peer,
                session.public_address,
                session.connected_at.elapsed().as_secs(),
                sessions.active.len(),
            );
            sessions.record(
                EventKind::Disconnected,
                Client {
                    id: self.id,
                    client_id: session.client_id,
                    transport: session.transport.to_owned(),
                    peer: session.peer,
                    public_address: session.public_address,
                    connected_seconds: session.connected_at.elapsed().as_secs(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Monitor, Uuid};

    #[test]
    fn expired_event_cursor_reports_loss_instead_of_skipping_connections() {
        let monitor = Monitor::new();
        for _ in 0..65 {
            let session = monitor.register(
                "tcp",
                "short-lived",
                "127.0.0.1:1",
                "relay.example.test:3000".to_owned(),
            );
            drop(session);
        }
        assert!(monitor.events(Some((monitor.epoch, 0))).is_err());
        assert!(monitor.events(Some((Uuid::nil(), 130))).is_err());
        let retained = monitor.events(Some((monitor.epoch, 2))).unwrap();
        assert_eq!(retained.cursor, 130);
        assert_eq!(retained.events.first().unwrap().seq, 3);
        assert_eq!(retained.events.last().unwrap().seq, 130);
    }
}
