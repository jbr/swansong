fn main() {
    use std::{thread, time::Duration};
    use swansong::Swansong;
    let swansong = Swansong::new();

    thread::spawn({
        let guard = swansong.guard();
        move || {
            let _guard = guard;
            thread::sleep(Duration::from_secs(1));
        }
    });

    thread::spawn({
        let swansong = swansong.clone();
        move || {
            for n in swansong
                .interrupt(std::iter::repeat_with(|| fastrand::u8(..)))
                .guarded()
            {
                thread::sleep(Duration::from_millis(100));
                println!("{n}");
            }
        }
    });

    thread::sleep(Duration::from_millis(500));

    swansong.shut_down().block();
}
