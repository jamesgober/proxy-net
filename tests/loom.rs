//! Model checks of the lock-free core (every interleaving loom can find).
//! Run with `RUSTFLAGS="--cfg proxy_net_loom" cargo test --release --test loom`.

#![cfg(proxy_net_loom)]

use loom::sync::Arc;
use loom::thread;
use proxy_net::core::{EndpointId, HealthTable, Outcome, RetryBudget, Tick};
use proxy_net::{Outlier, Pool, PoolConfig, ProxyBuilder, Retry};

fn pool(o: Outlier) -> PoolConfig {
    ProxyBuilder::new()
        .pool("p", Pool::new(["10.0.0.1:80", "10.0.0.2:80"]).outlier(o))
        .config()
        .expect("config")
        .pool("p")
        .expect("pool")
        .clone()
}

/// Two threads fail the same endpoint at once: it is ejected exactly once,
/// and the ejection count is exact.
#[test]
fn loom_concurrent_failures_eject_once() {
    loom::model(|| {
        let p = pool(Outlier::default().consecutive_failures(2).max_ejected(100));
        let h = Arc::new(HealthTable::new(&p));
        let a = {
            let h = Arc::clone(&h);
            thread::spawn(move || h.record(EndpointId(0), Outcome::Failure, Tick(0)).is_some())
        };
        let b = {
            let h = Arc::clone(&h);
            thread::spawn(move || h.record(EndpointId(0), Outcome::Failure, Tick(0)).is_some())
        };
        let ejected = usize::from(a.join().expect("a")) + usize::from(b.join().expect("b"));
        assert_eq!(ejected, 1);
        assert_eq!(h.ejected_count(), 1);
    });
}

/// Two endpoints failing at once in a pool that may eject one of two: never
/// both.
#[test]
fn loom_ejection_cap_holds_under_races() {
    loom::model(|| {
        let p = pool(Outlier::default().consecutive_failures(1).max_ejected(50));
        let h = Arc::new(HealthTable::new(&p));
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let h = Arc::clone(&h);
                thread::spawn(move || {
                    let _ = h.record(EndpointId(i), Outcome::Failure, Tick(0));
                })
            })
            .collect();
        for t in handles {
            t.join().expect("join");
        }
        assert_eq!(h.ejected_count(), 1);
    });
}

/// Two threads take retries from a budget of one: exactly one wins.
#[test]
fn loom_retry_budget_is_never_exceeded() {
    loom::model(|| {
        let b = Arc::new(RetryBudget::new(&Retry::default().budget(0, 1)));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let b = Arc::clone(&b);
                thread::spawn(move || b.try_retry())
            })
            .collect();
        let granted = handles
            .into_iter()
            .map(|t| usize::from(t.join().expect("join")))
            .sum::<usize>();
        assert_eq!(granted, 1);
        assert_eq!(b.retries(), 1);
    });
}
