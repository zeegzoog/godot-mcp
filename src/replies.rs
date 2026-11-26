//! Hands each game runtime reply (`mcp:reply`) to the one call that waits for it.
//!
//! A reply can carry a large screenshot. The connection's reader parses it once, looks up the
//! waiting call by request id and moves the parsed value to it. Waiting calls neither see nor
//! parse each other's replies, and a reply nobody waits for is dropped.

use crate::util::lock;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::sync::oneshot;

type Slots<K> = Arc<Mutex<HashMap<K, oneshot::Sender<Value>>>>;

/// The calls waiting for a reply, by key (a request id, or a debugger session and a request id).
#[derive(Debug)]
pub struct ReplyRouter<K> {
    slots: Slots<K>,
}

impl<K> Default for ReplyRouter<K> {
    fn default() -> Self {
        Self {
            slots: Arc::default(),
        }
    }
}

impl<K> Clone for ReplyRouter<K> {
    fn clone(&self) -> Self {
        Self {
            slots: Arc::clone(&self.slots),
        }
    }
}

impl<K: Eq + Hash + Clone> ReplyRouter<K> {
    /// Registers a waiter. Register before sending the request, so a fast reply is not missed.
    /// Dropping the returned wait (timeout, cancellation) removes the slot again.
    pub fn expect(&self, key: K) -> ReplyWait<K> {
        let (tx, rx) = oneshot::channel();
        lock(&self.slots).insert(key.clone(), tx);
        ReplyWait {
            rx,
            key,
            slots: Arc::clone(&self.slots),
        }
    }

    /// Whether any call waits, so a reader can skip parsing a reply nobody will read.
    pub fn is_waiting(&self) -> bool {
        !lock(&self.slots).is_empty()
    }

    /// Moves `reply` to the call waiting under `key`. `false` when no call waits for it.
    pub fn deliver(&self, key: &K, reply: Value) -> bool {
        let Some(slot) = lock(&self.slots).remove(key) else {
            return false;
        };
        slot.send(reply).is_ok()
    }

    /// Ends every wait: the connection the replies would come over is gone.
    pub fn close_all(&self) {
        lock(&self.slots).clear();
    }
}

/// One call's pending reply. Resolves to the parsed reply, or to an error when the connection
/// closed first.
#[derive(Debug)]
pub struct ReplyWait<K: Eq + Hash> {
    rx: oneshot::Receiver<Value>,
    key: K,
    slots: Slots<K>,
}

impl<K: Eq + Hash + Unpin> Future for ReplyWait<K> {
    type Output = Result<Value, oneshot::error::RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().rx).poll(cx)
    }
}

impl<K: Eq + Hash> Drop for ReplyWait<K> {
    fn drop(&mut self) {
        lock(&self.slots).remove(&self.key);
    }
}

/// The request id a reply echoes. Godot's JSON parser stores every number as a float, so a reply
/// to request 1 says `1.0`.
pub fn reply_id(reply: &Value) -> Option<i64> {
    let id = reply.get("id")?;
    if let Some(id) = id.as_i64() {
        return Some(id);
    }
    let number = id.as_f64()?;
    if number.fract() != 0.0 || number.abs() > 9_007_199_254_740_992.0 {
        return None;
    }
    Some(number as i64)
}

/// Parses one `mcp:reply` text and hands it to its waiter. Returns why it was dropped, if it was.
pub fn route_reply<K: Eq + Hash + Clone>(
    router: &ReplyRouter<K>,
    text: &str,
    key_of: impl FnOnce(i64) -> K,
) -> Result<(), String> {
    if !router.is_waiting() {
        return Err("no call is waiting for a reply".into());
    }
    let reply: Value = crate::godot::json_text::parse(text)
        .map_err(|err| format!("the reply is not JSON: {err}"))?;
    let Some(id) = reply_id(&reply) else {
        return Err("the reply has no request id".into());
    };
    if router.deliver(&key_of(id), reply) {
        Ok(())
    } else {
        Err(format!("no call is waiting for request {id}"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn two_concurrent_waiters_each_get_their_own_reply() {
        let router = ReplyRouter::<i64>::default();
        let first = router.expect(1);
        let second = router.expect(2);
        // Answered in the other order.
        route_reply(&router, r#"{"id": 2.0, "result": "two"}"#, |id| id).unwrap();
        route_reply(&router, r#"{"id": 1, "result": "one"}"#, |id| id).unwrap();
        assert_eq!(second.await.unwrap()["result"], "two");
        assert_eq!(first.await.unwrap()["result"], "one");
        assert!(!router.is_waiting());
    }

    #[tokio::test]
    async fn a_reply_for_an_unknown_id_is_dropped_and_the_waiter_keeps_waiting() {
        let router = ReplyRouter::<i64>::default();
        let mut wait = router.expect(5);
        let dropped = route_reply(&router, r#"{"id": 6, "result": 1}"#, |id| id).unwrap_err();
        assert!(dropped.contains("request 6"), "{dropped}");
        assert!(route_reply(&router, "not json", |id| id).is_err());
        assert!(route_reply(&router, r#"{"result": 1}"#, |id| id).is_err());
        assert!(route_reply(&router, r#"{"id": 5.5}"#, |id| id).is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait)
                .await
                .is_err(),
            "the waiter got a reply that was not its own"
        );
        route_reply(&router, r#"{"id": 5, "result": 1}"#, |id| id).unwrap();
        assert_eq!(wait.await.unwrap()["result"], 1);
    }

    #[tokio::test]
    async fn a_dropped_wait_frees_its_slot_and_closing_ends_the_others() {
        let router = ReplyRouter::<(i64, i64)>::default();
        drop(router.expect((7, 1)));
        assert!(!router.is_waiting());
        let wait = router.expect((7, 2));
        // The same id from another debugger session is someone else's reply.
        assert!(route_reply(&router, r#"{"id": 2}"#, |id| (8, id)).is_err());
        router.close_all();
        assert!(wait.await.is_err());
    }

    #[test]
    fn reply_ids_accept_integral_floats_only() {
        assert_eq!(reply_id(&json!({"id": 3})), Some(3));
        assert_eq!(reply_id(&json!({"id": 3.0})), Some(3));
        assert_eq!(reply_id(&json!({"id": 3.5})), None);
        assert_eq!(reply_id(&json!({"id": "3"})), None);
        assert_eq!(reply_id(&json!({})), None);
    }
}
