//! Temporary comparison of the old and current mobile snapshot-preparation work, not rendering.
#[path = "benchmark_support/snapshot_fixture.rs"]
mod fixture;
use peersus_ui::view::{self, View};
use std::{hint::black_box, time::Instant};

fn old(current: &View, snapshot: &peersus_core::model::Snapshot) -> View {
    let mut next = current.clone();
    black_box(view::handoffs(&View::of(snapshot).transfers, |_| None));
    black_box(
        View::of(snapshot)
            .transfers
            .iter()
            .filter(|t| {
                t.incoming && matches!(t.phase, view::Phase::Cancelled | view::Phase::Failed(_))
            })
            .count(),
    );
    black_box(view::refresh(&mut next, snapshot));
    next
}
fn current(previous: &View, snapshot: &peersus_core::model::Snapshot) -> View {
    let mut next = View::of(snapshot);
    next.transfers_at_start = previous.transfers_at_start.clone();
    black_box(view::handoffs(&next.transfers, |_| None));
    black_box(
        next.transfers
            .iter()
            .filter(|t| {
                t.incoming && matches!(t.phase, view::Phase::Cancelled | view::Phase::Failed(_))
            })
            .count(),
    );
    black_box(*previous != next);
    next
}
fn main() {
    let snapshot = fixture::snapshot_sized(16, 128);
    let previous = View::of(&snapshot);
    assert_eq!(old(&previous, &snapshot), current(&previous, &snapshot));
    let mut a = Vec::new();
    let mut b = Vec::new();
    for i in 0..1200 {
        let mut measure = |before: bool| {
            let started = Instant::now();
            black_box(if before {
                old(black_box(&previous), black_box(&snapshot))
            } else {
                current(black_box(&previous), black_box(&snapshot))
            });
            let elapsed = started.elapsed().as_secs_f64() * 1e6;
            if i >= 200 {
                if before {
                    a.push(elapsed);
                } else {
                    b.push(elapsed);
                }
            }
        };
        measure(i % 2 == 0);
        measure(i % 2 != 0);
    }
    a.sort_by(f64::total_cmp);
    b.sort_by(f64::total_cmp);
    println!(
        "synthetic mobile preparation, same profile/process, 16 peers/128 transfers, 1000 alternating samples: before_median_us={:.3}, after_median_us={:.3}, before_p95_us={:.3}, after_p95_us={:.3}",
        a[500], b[500], a[949], b[949]
    );
}
