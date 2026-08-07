use log::{LevelFilter, Metadata, Record};
use std::sync::Mutex;
use swansong::Swansong;

static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOGGER: CaptureLogger = CaptureLogger;

struct CaptureLogger;
impl log::Log for CaptureLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &Record<'_>) {
        if record.level() == log::Level::Debug {
            CAPTURED.lock().unwrap().push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

fn take_captured() -> Vec<String> {
    std::mem::take(&mut *CAPTURED.lock().unwrap())
}

// A single test fn because `log::set_logger` is process-global and integration
// test binaries run each `#[test]` in the same process.
#[test]
fn guard_drop_logs_at_debug_only_after_shutdown_initiated() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(LevelFilter::Debug);

    let swansong = Swansong::new();

    let before_stop = swansong.guard();
    drop(before_stop);
    assert!(
        take_captured().is_empty(),
        "guards dropped before shutdown must not log"
    );

    let straggler = swansong.guard();
    let straggler_line = line!() - 1;
    swansong.shut_down();
    drop(straggler);

    let captured = take_captured();
    assert_eq!(captured.len(), 1, "{captured:?}");
    let message = &captured[0];
    assert!(
        message.contains(&format!("guard_drop_logging.rs:{straggler_line}")),
        "{message}"
    );
    assert!(message.contains("dropped during shutdown"), "{message}");

    // guards embedded in wrapper types log through the same path
    let interrupted = swansong.interrupt(std::future::pending::<()>()).guarded();
    let interrupt_line = line!() - 1;
    drop(interrupted);
    let captured = take_captured();
    assert_eq!(captured.len(), 1, "{captured:?}");
    assert!(
        captured[0].contains(&format!("guard_drop_logging.rs:{interrupt_line}")),
        "{}",
        captured[0]
    );
}
