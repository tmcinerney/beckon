use beckon::slow_job::{JobOutcome, SourceJobs};

#[test]
fn expired_source_work_is_not_executed_and_does_not_destroy_the_single_lane() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut jobs = SourceJobs::spawn(move || {
        move |value: u64, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            value
        }
    })
    .unwrap();
    let now = Instant::now();
    jobs.submit(1, now).unwrap();
    let limit = now + Duration::from_secs(5);
    while jobs.is_busy() {
        let _ = jobs.poll(Instant::now());
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    jobs.submit(2, limit).unwrap();
    loop {
        if let Some(JobOutcome::Completed { result, .. }) = jobs.poll(Instant::now()) {
            assert_eq!(result, 2);
            break;
        }
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn source_process_deadline_and_combined_output_limit_kill_reap_without_unbounded_capture() {
    use beckon::process_job::{output, with_deadline};
    use std::process::Command;
    let now = Instant::now();
    let error = with_deadline(now, || {
        output(Command::new("/bin/sh").args(["-c", "exit 0"]))
    })
    .unwrap_err();
    assert!(error.to_string().contains("deadline"));
    let error = with_deadline(Instant::now() + Duration::from_secs(5), || {
        output(Command::new("/bin/sh").args([
            "-c",
            "while :; do printf '0123456789012345678901234567890123456789'; done",
        ]))
    })
    .unwrap_err();
    assert!(error.to_string().contains("limit"));
    let result = with_deadline(Instant::now() + Duration::from_secs(5), || {
        output(Command::new("/bin/sh").args(["-c", "printf 'ok'"]))
    })
    .unwrap();
    assert_eq!(result.stdout, b"ok");
    assert!(result.status.success());
}
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

#[test]
fn daemon_turn_slow_source_has_one_lease_and_timeout_never_replays_a_late_result() {
    let (release, wait) = mpsc::sync_channel::<()>(1);
    let (started, seen) = mpsc::sync_channel(1);
    let mut jobs = SourceJobs::spawn(move || {
        move |value: u64, _deadline| {
            started.send(()).unwrap();
            wait.recv().unwrap();
            value + 1
        }
    })
    .unwrap();
    let now = Instant::now();
    let id = jobs.submit(7, now + Duration::from_millis(1000)).unwrap();
    seen.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(jobs.poll(now).is_none());
    assert!(jobs.submit(99, now + Duration::from_secs(2)).is_err());
    assert!(
        matches!(jobs.poll(now+Duration::from_millis(1000)),Some(JobOutcome::TimedOut {id:actual}) if actual==id)
    );
    assert!(
        jobs.is_busy(),
        "timeout is not permission for overlapping source work"
    );
    release.send(()).unwrap();
    let limit = Instant::now() + Duration::from_secs(5);
    while jobs.is_busy() {
        assert!(
            jobs.poll(now + Duration::from_secs(2)).is_none(),
            "late result must be discarded"
        );
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
}
