//! Integration tests for [`levshell_core::EventBus`].

use std::time::Duration;

use levshell_core::{Event, EventBus, EventKind};
use uuid::Uuid;

#[tokio::test]
async fn delivers_only_subscribed_kinds() {
    let bus = EventBus::new();
    let mut workspace_rx = bus.subscribe("ws", [EventKind::WorkspaceChanged], 16);
    let mut power_rx = bus.subscribe("pwr", [EventKind::PowerStateChanged], 16);

    bus.publish(Event::WorkspaceChanged {
        name: "research".into(),
        focused_window: Some("alacritty".into()),
    });
    bus.publish(Event::PowerStateChanged { on_battery: true });
    bus.publish(Event::WindowFocused {
        app_id: Some("firefox".into()),
        title: "tab".into(),
    });

    // workspace_rx must see exactly the workspace event
    let ev = tokio::time::timeout(Duration::from_secs(1), workspace_rx.recv())
        .await
        .expect("recv timeout")
        .expect("rx closed");
    assert!(matches!(ev, Event::WorkspaceChanged { ref name, .. } if name == "research"));
    assert!(workspace_rx.try_recv().is_err(), "workspace_rx should not see other kinds");

    // power_rx must see exactly the power event
    let ev = tokio::time::timeout(Duration::from_secs(1), power_rx.recv())
        .await
        .expect("recv timeout")
        .expect("rx closed");
    assert!(matches!(ev, Event::PowerStateChanged { on_battery: true }));
    assert!(power_rx.try_recv().is_err());
}

#[tokio::test]
async fn full_subscriber_does_not_block_publisher() {
    let bus = EventBus::new();
    // Capacity of 1 — second publish will fill it up.
    let _rx = bus.subscribe("slow", [EventKind::DataStoreUpdated], 1);

    // Publish 5 events; only the first lands, the rest are dropped.
    for _ in 0..5 {
        bus.publish(Event::DataStoreUpdated {
            entity_type: "note".into(),
            entity_id: Uuid::now_v7(),
        });
    }

    // The publisher returned without blocking and the subscriber is still
    // present (not removed) — only the dropped counter went up.
    let stats = bus.stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].name, "slow");
    assert!(
        stats[0].dropped >= 4,
        "expected at least 4 dropped events, saw {}",
        stats[0].dropped
    );
}

#[tokio::test]
async fn closed_subscribers_are_cleaned_up() {
    let bus = EventBus::new();
    {
        let _rx = bus.subscribe("transient", [EventKind::WindowFocused], 4);
        assert_eq!(bus.subscriber_count(), 1);
    }
    // Receiver dropped — subscriber is still present until next publish.
    assert_eq!(bus.subscriber_count(), 1);

    bus.publish(Event::WindowFocused {
        app_id: None,
        title: "ignored".into(),
    });

    assert_eq!(bus.subscriber_count(), 0, "publish should reap dead subs");
}

#[tokio::test]
async fn multiple_matching_subscribers_each_get_a_copy() {
    let bus = EventBus::new();
    let mut a = bus.subscribe("a", [EventKind::WorkspaceChanged], 16);
    let mut b = bus.subscribe("b", [EventKind::WorkspaceChanged], 16);

    bus.publish(Event::WorkspaceChanged {
        name: "writing".into(),
        focused_window: None,
    });

    let ea = a.recv().await.unwrap();
    let eb = b.recv().await.unwrap();
    assert!(matches!(ea, Event::WorkspaceChanged { ref name, .. } if name == "writing"));
    assert!(matches!(eb, Event::WorkspaceChanged { ref name, .. } if name == "writing"));
}
