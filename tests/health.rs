//! Endpoint health driven by explicit time (edge cases E-140 to E-148).

use std::time::Duration;

use proxy_net::core::{Balancer, EndpointId, EndpointState, HealthTable, Outcome, Tick};
use proxy_net::{HealthCheck, Outlier, Pool, PoolConfig, ProxyBuilder};

fn pool(p: Pool) -> PoolConfig {
    ProxyBuilder::new()
        .pool("p", p)
        .config()
        .expect("config")
        .pool("p")
        .expect("pool")
        .clone()
}

const S: u64 = 1_000_000_000;

#[test]
fn test_rise_and_fall_at_the_thresholds() {
    // E-140.
    let p = pool(Pool::new(["10.0.0.1:80"]).health(HealthCheck::http("/h").rise(3).fall(2)));
    let h = HealthTable::new(&p);
    let a = EndpointId(0);
    for _ in 0..2 {
        assert!(h.check_result(a, true, Tick(0)).is_none());
    }
    assert_eq!(
        h.check_result(a, true, Tick(0)).map(|t| t.to),
        Some(EndpointState::Healthy)
    );
    assert!(h.check_result(a, false, Tick(0)).is_none());
    assert_eq!(
        h.check_result(a, false, Tick(0)).map(|t| t.to),
        Some(EndpointState::Unhealthy)
    );
}

#[test]
fn test_flapping_endpoint_never_reaches_rise() {
    // E-141.
    let p = pool(Pool::new(["10.0.0.1:80"]).health(HealthCheck::http("/h").rise(2).fall(2)));
    let h = HealthTable::new(&p);
    for i in 0..100 {
        assert!(h.check_result(EndpointId(0), i % 2 == 0, Tick(0)).is_none());
    }
    assert_eq!(h.state(EndpointId(0)), EndpointState::Unknown);
}

#[test]
fn test_every_endpoint_down_uses_the_panic_threshold() {
    // E-142, FR-18.
    let p = pool(Pool::new(["10.0.0.1:80", "10.0.0.2:80"]).health(HealthCheck::tcp().fall(1)));
    let h = HealthTable::new(&p);
    let b = Balancer::new(&p);
    for i in 0..2 {
        let _ = h.check_result(EndpointId(i), false, Tick(0));
    }
    assert_eq!(h.healthy_count(), 0);
    assert_eq!(b.refresh(&h), 2);
    assert!(b.pick(None, &[]).is_some());
}

#[test]
fn test_ejection_backoff_doubles_to_the_cap() {
    // E-144.
    let o = Outlier::default()
        .consecutive_failures(1)
        .base_ejection(Duration::from_secs(10))
        .max_ejection(Duration::from_secs(25))
        .max_ejected(100);
    let p = pool(Pool::new(["10.0.0.1:80"]).outlier(o));
    let h = HealthTable::new(&p);
    let a = EndpointId(0);
    let mut now = 0;
    let mut changes = Vec::new();
    for expected in [10, 20, 25, 25] {
        let t = h
            .record(a, Outcome::Failure, Tick(now * S))
            .expect("ejected");
        assert_eq!(
            t.to,
            EndpointState::Ejected {
                until: Tick((now + expected) * S)
            }
        );
        now += expected;
        h.expire(Tick(now * S), &mut changes);
        assert_eq!(h.state(a), EndpointState::Healthy);
    }
}

#[test]
fn test_concurrent_failures_eject_once() {
    let p = pool(
        Pool::new(["10.0.0.1:80", "10.0.0.2:80"])
            .outlier(Outlier::default().consecutive_failures(10).max_ejected(100)),
    );
    let h = std::sync::Arc::new(HealthTable::new(&p));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let h = h.clone();
            std::thread::spawn(move || {
                (0..1000)
                    .filter(|_| h.record(EndpointId(0), Outcome::Failure, Tick(0)).is_some())
                    .count()
            })
        })
        .collect();
    let ejections: usize = threads.into_iter().map(|t| t.join().expect("join")).sum();
    assert_eq!(ejections, 1);
    assert_eq!(h.ejected_count(), 1);
}
