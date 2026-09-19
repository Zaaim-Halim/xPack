//! Transport timeouts, including against a server that stalls.

use std::time::Duration;

use xpack_update::Timeouts;

#[test]
fn the_default_total_budget_is_generous_enough_for_a_large_package() {
    // A 300 MB package on a 1 MB/s connection takes five minutes. A budget of
    // five minutes would therefore fail that download at exactly the moment it
    // was about to succeed, which is how the previous value was wrong.
    let defaults = Timeouts::default();
    assert_eq!(defaults.total, Duration::from_secs(30 * 60));
    assert!(defaults.total > Duration::from_secs(10 * 60));
}

#[test]
fn the_narrow_timeouts_are_much_shorter_than_the_total() {
    // The total budget is the backstop. It is far too generous to notice a
    // stalled server quickly, so the narrower ones have to do that.
    let t = Timeouts::default();
    assert!(t.resolve < t.total / 10, "resolve must fail fast");
    assert!(t.connect < t.total / 10, "connect must fail fast");
    assert!(
        t.response < t.total / 10,
        "a server that accepts and says nothing must not hold the whole budget"
    );
}

#[test]
fn a_total_budget_can_be_set_explicitly() {
    let t = Timeouts::with_total(Duration::from_secs(90)).unwrap();
    assert_eq!(t.total, Duration::from_secs(90));
    // Overriding the total must not quietly discard the others.
    assert_eq!(t.connect, Timeouts::default().connect);
    assert_eq!(t.response, Timeouts::default().response);
}

#[test]
fn an_unusable_total_budget_is_rejected_rather_than_accepted() {
    // Zero would make every download fail in a way that looks like a network
    // problem rather than a configuration mistake.
    for absurd in [Duration::ZERO, Duration::from_millis(1), Duration::from_millis(999)] {
        let err = Timeouts::with_total(absurd).unwrap_err();
        assert!(err.to_string().contains("too short"), "got {err}");
    }
    Timeouts::with_total(Duration::from_secs(1)).expect("one second is the floor, not below it");
}

#[cfg(feature = "https")]
#[test]
fn a_server_that_accepts_and_says_nothing_fails_fast() {
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Instant;
    use xpack_update::{HttpsTransport, UpdateTransport};

    // Accepts the connection, reads the request, then never replies. Without a
    // response timeout this holds the client for the whole total budget.
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            std::thread::sleep(Duration::from_secs(30));
            drop(stream);
        }
    });

    let timeouts = Timeouts {
        total: Duration::from_secs(600),
        response: Duration::from_secs(2),
        ..Timeouts::default()
    };
    let transport = HttpsTransport::with_timeouts(timeouts);

    let started = Instant::now();
    let mut sink = Vec::new();
    let result = transport.fetch(&format!("http://127.0.0.1:{port}/stable.json"), 1024, &mut sink);
    let elapsed = started.elapsed();

    assert!(result.is_err(), "a silent server must not appear to succeed");
    assert!(
        elapsed < Duration::from_secs(20),
        "must give up on the response timeout, not the total budget; took {elapsed:?}"
    );
    let _ = std::io::stdout().flush();
}
