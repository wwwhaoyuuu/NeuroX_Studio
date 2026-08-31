use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueStats {
    pub capacity: usize,
    pub depth: usize,
    pub max_depth: usize,
    pub rejected: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum QueueError<T> {
    Full(T),
    Closed(T),
    Timeout,
}

struct QueueState<T> {
    items: VecDeque<T>,
    max_depth: usize,
    rejected: u64,
    producer_open: bool,
    consumer_open: bool,
}

struct QueueInner<T> {
    capacity: usize,
    state: Mutex<QueueState<T>>,
    not_empty: Condvar,
}

pub struct BoundedProducer<T> {
    inner: Arc<QueueInner<T>>,
}

pub struct BoundedConsumer<T> {
    inner: Arc<QueueInner<T>>,
}

pub fn bounded_queue<T>(
    capacity: usize,
) -> Result<(BoundedProducer<T>, BoundedConsumer<T>), &'static str> {
    if capacity == 0 {
        return Err("queue capacity must be greater than zero");
    }

    let inner = Arc::new(QueueInner {
        capacity,
        state: Mutex::new(QueueState {
            items: VecDeque::with_capacity(capacity),
            max_depth: 0,
            rejected: 0,
            producer_open: true,
            consumer_open: true,
        }),
        not_empty: Condvar::new(),
    });
    Ok((
        BoundedProducer {
            inner: Arc::clone(&inner),
        },
        BoundedConsumer { inner },
    ))
}

impl<T> BoundedProducer<T> {
    pub fn try_send(&self, item: T) -> Result<(), QueueError<T>> {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("bounded queue mutex poisoned");
        if !state.consumer_open {
            return Err(QueueError::Closed(item));
        }
        if state.items.len() >= self.inner.capacity {
            state.rejected += 1;
            return Err(QueueError::Full(item));
        }

        state.items.push_back(item);
        state.max_depth = state.max_depth.max(state.items.len());
        self.inner.not_empty.notify_one();
        Ok(())
    }

    pub fn stats(&self) -> QueueStats {
        stats_from(&self.inner)
    }
}

impl<T> Drop for BoundedProducer<T> {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("bounded queue mutex poisoned");
        state.producer_open = false;
        self.inner.not_empty.notify_all();
    }
}

impl<T> BoundedConsumer<T> {
    pub fn try_recv(&self) -> Result<T, QueueError<()>> {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("bounded queue mutex poisoned");
        if let Some(item) = state.items.pop_front() {
            return Ok(item);
        }
        if !state.producer_open {
            return Err(QueueError::Closed(()));
        }
        Err(QueueError::Timeout)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, QueueError<()>> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .inner
            .state
            .lock()
            .expect("bounded queue mutex poisoned");

        loop {
            if let Some(item) = state.items.pop_front() {
                return Ok(item);
            }
            if !state.producer_open {
                return Err(QueueError::Closed(()));
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(QueueError::Timeout);
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next_state, wait_result) = self
                .inner
                .not_empty
                .wait_timeout(state, remaining)
                .expect("bounded queue mutex poisoned while waiting");
            state = next_state;
            if wait_result.timed_out() && state.items.is_empty() {
                return Err(QueueError::Timeout);
            }
        }
    }

    pub fn stats(&self) -> QueueStats {
        stats_from(&self.inner)
    }
}

impl<T> Drop for BoundedConsumer<T> {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("bounded queue mutex poisoned");
        state.consumer_open = false;
    }
}

fn stats_from<T>(inner: &QueueInner<T>) -> QueueStats {
    let state = inner.state.lock().expect("bounded queue mutex poisoned");
    QueueStats {
        capacity: inner.capacity,
        depth: state.items.len(),
        max_depth: state.max_depth,
        rejected: state.rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_items_at_capacity_without_growing() {
        let (producer, consumer) = bounded_queue(2).unwrap();
        producer.try_send(10).unwrap();
        producer.try_send(20).unwrap();

        assert_eq!(producer.try_send(30), Err(QueueError::Full(30)));
        assert_eq!(producer.stats().depth, 2);
        assert_eq!(producer.stats().max_depth, 2);
        assert_eq!(producer.stats().rejected, 1);
        assert_eq!(consumer.try_recv(), Ok(10));
        assert_eq!(consumer.try_recv(), Ok(20));
    }

    #[test]
    fn receiver_wakes_when_producer_closes() {
        let (producer, consumer) = bounded_queue::<u8>(1).unwrap();
        drop(producer);

        assert_eq!(
            consumer.recv_timeout(Duration::from_millis(10)),
            Err(QueueError::Closed(()))
        );
    }

    #[test]
    fn rejects_zero_capacity() {
        assert!(bounded_queue::<u8>(0).is_err());
    }
}
