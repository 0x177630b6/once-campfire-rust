//! In-process replacement for the Redis subscription adapter: one `tokio::sync::broadcast`
//! channel per broadcasting, created on first subscribe and dropped with its last subscriber.
//!
//! Payloads are already-encoded JSON, as they are on the Redis wire. Each channel's ring buffer
//! is bounded; a subscriber that falls behind gets `Lagged` and its connection is closed with
//! `reconnect: true` rather than silently skipping messages.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

pub type Payload = Arc<str>;

pub struct Hub {
    capacity: usize,
    streams: Mutex<HashMap<String, broadcast::Sender<Payload>>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecvError {
    /// The subscriber missed messages; its connection must be closed.
    Lagged,
    Closed,
}

impl Hub {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self { capacity, streams: Mutex::new(HashMap::new()) })
    }

    /// Publishes to every current subscriber of `broadcasting`. Returns how many received it.
    pub fn broadcast(&self, broadcasting: &str, payload: Payload) -> usize {
        let mut streams = self.streams.lock().unwrap();
        match streams.get(broadcasting).map(|sender| sender.send(payload)) {
            Some(Ok(receivers)) => receivers,
            Some(Err(_)) => {
                streams.remove(broadcasting);
                0
            }
            None => 0,
        }
    }

    pub fn subscribe(self: &Arc<Self>, broadcasting: &str) -> Subscriber {
        let mut streams = self.streams.lock().unwrap();
        let receiver = match streams.get(broadcasting) {
            Some(sender) => sender.subscribe(),
            None => {
                let (sender, receiver) = broadcast::channel(self.capacity);
                streams.insert(broadcasting.to_string(), sender);
                receiver
            }
        };
        Subscriber { hub: self.clone(), broadcasting: broadcasting.to_string(), receiver: Some(receiver) }
    }

    /// Number of broadcastings with at least one live subscriber channel.
    pub fn stream_count(&self) -> usize {
        self.streams.lock().unwrap().len()
    }

    fn release(&self, broadcasting: &str) {
        let mut streams = self.streams.lock().unwrap();
        if streams.get(broadcasting).is_some_and(|sender| sender.receiver_count() == 0) {
            streams.remove(broadcasting);
        }
    }
}

pub struct Subscriber {
    hub: Arc<Hub>,
    broadcasting: String,
    receiver: Option<broadcast::Receiver<Payload>>,
}

impl Subscriber {
    pub fn broadcasting(&self) -> &str {
        &self.broadcasting
    }

    pub async fn recv(&mut self) -> Result<Payload, RecvError> {
        match self.receiver.as_mut().expect("receiver is present until drop").recv().await {
            Ok(payload) => Ok(payload),
            Err(broadcast::error::RecvError::Lagged(_)) => Err(RecvError::Lagged),
            Err(broadcast::error::RecvError::Closed) => Err(RecvError::Closed),
        }
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        drop(self.receiver.take());
        self.hub.release(&self.broadcasting);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delivers_to_every_subscriber_and_cleans_up() {
        let hub = Hub::new(8);
        let mut a = hub.subscribe("room");
        let mut b = hub.subscribe("room");
        assert_eq!(hub.broadcast("room", "1".into()), 2);
        assert_eq!(&*a.recv().await.unwrap(), "1");
        assert_eq!(&*b.recv().await.unwrap(), "1");
        drop(a);
        assert_eq!(hub.stream_count(), 1);
        drop(b);
        assert_eq!(hub.stream_count(), 0);
        assert_eq!(hub.broadcast("room", "2".into()), 0);
    }

    #[tokio::test]
    async fn slow_subscribers_see_lag_instead_of_gaps() {
        let hub = Hub::new(2);
        let mut slow = hub.subscribe("room");
        for i in 0..5 {
            hub.broadcast("room", i.to_string().into());
        }
        assert_eq!(slow.recv().await, Err(RecvError::Lagged));
    }
}
