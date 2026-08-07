use swansong::{Guard, GuardReport, GuardReportEntry, Swansong};

#[test]
fn auto_traits() {
    fn assert_auto_traits<T: Send + Sync + Unpin>() {}
    assert_auto_traits::<Guard>();
    assert_auto_traits::<GuardReport>();
    assert_auto_traits::<GuardReportEntry>();
}

#[test]
fn report_aggregates_by_creation_site() {
    let swansong = Swansong::new();
    assert!(swansong.guard_report().is_empty());
    assert_eq!(swansong.guard_report().guard_count(), 0);

    let mut guards = Vec::new();
    let repeated_line = line!() + 2;
    for _ in 0..3 {
        guards.push(swansong.guard());
    }
    let single = swansong.guard();
    let single_line = line!() - 1;

    let report = swansong.guard_report();
    assert_eq!(report.guard_count(), 4);
    assert_eq!(report.entries().len(), 2);

    let by_line = |line: u32| {
        report
            .entries()
            .iter()
            .find(|entry| entry.location().line() == line)
            .unwrap()
    };
    let repeated = by_line(repeated_line);
    assert_eq!(repeated.count(), 3);
    assert!(repeated.location().file().ends_with("guard_report.rs"));
    assert_eq!(by_line(single_line).count(), 1);

    drop(guards);
    let report = swansong.guard_report();
    assert_eq!(report.guard_count(), 1);
    assert_eq!(report.entries()[0].location().line(), single_line);
    drop(single);
    assert!(swansong.guard_report().is_empty());
}

#[test]
fn clones_inherit_creation_location() {
    let swansong = Swansong::new();
    let original = swansong.guard();
    let original_line = line!() - 1;
    let clone = original.clone();
    let _clone_of_clone = clone.clone();

    let report = swansong.guard_report();
    assert_eq!(report.guard_count(), 3);
    assert_eq!(report.entries().len(), 1);
    let entry = &report.entries()[0];
    assert_eq!(entry.location().line(), original_line);
    assert_eq!(entry.count(), 3);
}

#[test]
fn guarded_and_interrupt_record_caller_location() {
    let swansong = Swansong::new();
    let _guarded = swansong.guarded(std::future::pending::<()>());
    let guarded_line = line!() - 1;
    let _interrupt = swansong.interrupt(std::future::pending::<()>()).guarded();
    let interrupt_line = line!() - 1;

    let report = swansong.guard_report();
    assert_eq!(report.guard_count(), 2);
    let lines: Vec<u32> = report
        .entries()
        .iter()
        .map(|entry| entry.location().line())
        .collect();
    assert!(lines.contains(&guarded_line));
    assert!(lines.contains(&interrupt_line));
    for entry in &report {
        assert!(entry.location().file().ends_with("guard_report.rs"));
    }
}

#[test]
fn parent_report_includes_child_guards() {
    let parent = Swansong::new();
    let child = parent.child();
    let _child_guard = child.guard();
    let child_line = line!() - 1;
    let _parent_guard = parent.guard();

    assert_eq!(parent.guard_report().guard_count(), 2);
    let child_report = child.guard_report();
    assert_eq!(child_report.guard_count(), 1);
    assert_eq!(child_report.entries()[0].location().line(), child_line);
}

#[test]
fn guard_outlives_child_swansong_handle() {
    let parent = Swansong::new();
    let guard = {
        let child = parent.child();
        child.guard()
    };
    let guard_line = line!() - 2;

    let report = parent.guard_report();
    assert_eq!(report.guard_count(), 1);
    assert_eq!(report.entries()[0].location().line(), guard_line);
    drop(guard);
    assert!(parent.guard_report().is_empty());
}

#[cfg(not(miri))]
#[test]
fn ages_are_reported() {
    let swansong = Swansong::new();
    let _guard = swansong.guard();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let report = swansong.guard_report();
    let age = report.entries()[0].oldest_age().unwrap();
    assert!(age >= std::time::Duration::from_millis(10));
}

#[test]
fn display() {
    let swansong = Swansong::new();
    assert_eq!(swansong.guard_report().to_string(), "no outstanding guards");

    let _guard = swansong.guard();
    let display = swansong.guard_report().to_string();
    assert!(display.starts_with("1 outstanding guard:"), "{display}");
    assert!(display.contains("1 × "), "{display}");
    assert!(display.contains("guard_report.rs"), "{display}");

    let _another = swansong.guard();
    let display = swansong.guard_report().to_string();
    assert!(display.starts_with("2 outstanding guards:"), "{display}");
}
