//! Balancing over many picks: weights, tiers, least connections, power of two
//! choices, consistent hashing with bounded load, and determinism across
//! nodes (edge cases E-150 to E-157).

use proxy_net::core::{Balancer, EndpointId, HealthTable, Outcome, Tick};
use proxy_net::{Balance, Endpoint, HashKey, Outlier, Pool, PoolConfig, ProxyBuilder};

fn pool(p: Pool) -> PoolConfig {
    ProxyBuilder::new()
        .pool("p", p)
        .config()
        .expect("config")
        .pool("p")
        .expect("pool")
        .clone()
}

fn addrs(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("10.{}.{}.1:80", i / 250, i % 250))
        .collect()
}

fn ready(p: &PoolConfig) -> (Balancer, HealthTable) {
    let b = Balancer::new(p);
    let h = HealthTable::new(p);
    let _ = b.refresh(&h);
    (b, h)
}

fn counts(b: &Balancer, n: usize, picks: usize, key: impl Fn(usize) -> Option<u64>) -> Vec<usize> {
    let mut c = vec![0; n];
    for i in 0..picks {
        c[b.pick(key(i), &[]).expect("pick").index()] += 1;
    }
    c
}

#[test]
fn test_weighted_round_robin_is_exact_per_cycle() {
    // FR-10: every window of sum(weights) picks holds each endpoint `weight` times.
    let weights = [1u16, 2, 3, 7];
    let p = weights
        .iter()
        .enumerate()
        .fold(Pool::new(Vec::<&str>::new()), |p, (i, w)| {
            p.endpoint(
                Endpoint::parse(&format!("10.0.0.{}:80", i + 1))
                    .expect("e")
                    .weight(*w),
            )
        });
    let p = pool(p);
    let (b, _) = ready(&p);
    let sum: usize = weights.iter().map(|w| usize::from(*w)).sum();
    for _ in 0..5 {
        let c = counts(&b, weights.len(), sum, |_| None);
        assert_eq!(
            c,
            weights.iter().map(|w| usize::from(*w)).collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_one_endpoint_and_extreme_weights() {
    // E-150, E-151.
    let (b, _) = ready(&pool(Pool::new(["10.0.0.1:80"])));
    assert_eq!(counts(&b, 1, 10, |_| None), [10]);
    let p = pool(
        Pool::new(Vec::<&str>::new())
            .endpoint(Endpoint::parse("10.0.0.1:80").expect("e").weight(1))
            .endpoint(Endpoint::parse("10.0.0.2:80").expect("e").weight(1000)),
    );
    let (b, _) = ready(&p);
    assert_eq!(counts(&b, 2, 1001, |_| None), [1, 1000]);
}

#[test]
fn test_unhealthy_endpoints_are_skipped() {
    // E-152.
    let p = pool(
        Pool::new(addrs(4)).outlier(
            Outlier::default()
                .consecutive_failures(1)
                .max_ejected(100)
                .panic_below(0),
        ),
    );
    let (b, h) = ready(&p);
    for i in 0..3 {
        let _ = h.record(EndpointId(i), Outcome::Failure, Tick(0));
    }
    assert_eq!(b.refresh(&h), 1);
    assert_eq!(counts(&b, 4, 20, |_| None), [0, 0, 0, 20]);
}

#[test]
fn test_tier_failover_and_recovery() {
    // E-157, FR-15.
    let p = pool(
        Pool::new(addrs(2))
            .endpoint(Endpoint::parse("10.9.9.1:80").expect("e").tier(1))
            .outlier(
                Outlier::default()
                    .consecutive_failures(1)
                    .max_ejected(100)
                    .panic_below(0),
            ),
    );
    let (b, h) = ready(&p);
    assert_eq!(counts(&b, 3, 10, |_| None)[2], 0);
    for i in 0..2 {
        let _ = h.record(EndpointId(i), Outcome::Failure, Tick(0));
    }
    let _ = b.refresh(&h);
    assert_eq!(counts(&b, 3, 10, |_| None), [0, 0, 10]);
    let mut changes = Vec::new();
    h.expire(Tick(u64::MAX), &mut changes);
    assert_eq!(changes.len(), 2);
    let _ = b.refresh(&h);
    assert_eq!(counts(&b, 3, 10, |_| None)[2], 0);
}

#[test]
fn test_least_connections_spreads_load() {
    let p = pool(Pool::new(addrs(4)).balance(Balance::LeastConnections));
    let (b, _) = ready(&p);
    // Start 40 exchanges without finishing: they spread evenly.
    for _ in 0..40 {
        let id = b.pick(None, &[]).expect("pick");
        b.started(id);
    }
    for i in 0..4 {
        assert_eq!(b.active(EndpointId(i)), 10);
    }
}

#[test]
fn test_power_of_two_and_random_cover_every_endpoint() {
    for policy in [Balance::PowerOfTwo, Balance::Random] {
        let p = pool(Pool::new(addrs(5)).balance(policy.clone()));
        let (b, _) = ready(&p);
        let c = counts(&b, 5, 5000, |_| None);
        assert!(c.iter().all(|&x| x > 700), "{policy:?}: {c:?}");
    }
}

#[test]
fn test_consistent_hashing_agrees_across_nodes_and_moves_little() {
    // FR-13, NFR-8, E-153, E-154.
    let hashed = |n: usize| {
        pool(Pool::new(addrs(n)).balance(Balance::consistent_hash(HashKey::Path).bounded_load(0)))
    };
    let keys: Vec<u64> = (0..10_000)
        .map(|i| Balancer::key(format!("/item/{i}").as_bytes()))
        .collect();
    let (a, _) = ready(&hashed(10));
    let (b, _) = ready(&hashed(10));
    let picks_a: Vec<EndpointId> = keys
        .iter()
        .map(|k| a.pick(Some(*k), &[]).expect("a"))
        .collect();
    let picks_b: Vec<EndpointId> = keys
        .iter()
        .map(|k| b.pick(Some(*k), &[]).expect("b"))
        .collect();
    assert_eq!(picks_a, picks_b);
    // Even spread.
    let mut spread = [0usize; 10];
    for p in &picks_a {
        spread[p.index()] += 1;
    }
    assert!(
        spread.iter().all(|&c| (800..1200).contains(&c)),
        "{spread:?}"
    );
    // The same pool with endpoint 9 ejected: only its keys move.
    let p = pool(
        Pool::new(addrs(10))
            .balance(Balance::consistent_hash(HashKey::Path).bounded_load(0))
            .outlier(Outlier::default().consecutive_failures(1).max_ejected(100)),
    );
    let (c, h) = ready(&p);
    let _ = h.record(EndpointId(9), Outcome::Failure, Tick(0));
    let _ = c.refresh(&h);
    for (k, before) in keys.iter().zip(&picks_a) {
        let after = c.pick(Some(*k), &[]).expect("c");
        if *before != EndpointId(9) {
            assert_eq!(after, *before);
        } else {
            assert_ne!(after, EndpointId(9));
        }
    }
    // E-153: one endpoint and many.
    let (one, _) = ready(&hashed(1));
    assert_eq!(one.pick(Some(keys[0]), &[]), Some(EndpointId(0)));
    let (many, _) = ready(&hashed(1000));
    assert!(many.pick(Some(keys[0]), &[]).is_some());
}

#[test]
fn test_bounded_load_moves_keys_off_hot_endpoints() {
    // FR-14, E-155.
    let p = pool(Pool::new(addrs(4)).balance(Balance::consistent_hash(HashKey::ClientAddress)));
    let (b, _) = ready(&p);
    let hot = Balancer::key(b"192.0.2.1");
    let home = b.pick(Some(hot), &[]).expect("pick");
    // One key sending 100 concurrent requests cannot put them all on one endpoint.
    let mut on_home = 0;
    for _ in 0..100 {
        let id = b.pick(Some(hot), &[]).expect("pick");
        on_home += usize::from(id == home);
        b.started(id);
    }
    assert!(on_home < 50, "{on_home}");
    for i in 0..4 {
        assert!(
            b.active(EndpointId(i)) <= 32,
            "endpoint {i}: {}",
            b.active(EndpointId(i))
        );
    }
}

#[test]
fn test_missing_key_falls_back_to_round_robin() {
    // E-156.
    let p = pool(
        Pool::new(addrs(3)).balance(Balance::consistent_hash(HashKey::Header("x-user".into()))),
    );
    let (b, _) = ready(&p);
    let c = counts(&b, 3, 30, |_| None);
    assert!(c.iter().all(|&x| x > 0), "{c:?}");
}
