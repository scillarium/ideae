//! The machine's own calendar: days and hours as the clock on the wall has
//! them, for "this weekend, in the morning".

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

#[cfg(unix)]
fn tm_of(secs: u64) -> libc::tm {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r only writes into the struct it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

#[cfg(unix)]
fn secs_of(tm: &mut libc::tm) -> u64 {
    // SAFETY: mktime reads the struct and normalizes it in place.
    unsafe { libc::mktime(tm) }.max(0) as u64
}

/// The C runtime's `struct tm`: nine ints on Windows as elsewhere.
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
}

#[cfg(windows)]
unsafe extern "C" {
    fn _localtime64_s(tm: *mut Tm, time: *const i64) -> i32;
    fn _mktime64(tm: *mut Tm) -> i64;
}

#[cfg(windows)]
fn tm_of(secs: u64) -> Tm {
    let mut tm = Tm::default();
    // SAFETY: _localtime64_s only writes into the struct it is given.
    unsafe { _localtime64_s(&mut tm, &(secs as i64)) };
    tm
}

#[cfg(windows)]
fn secs_of(tm: &mut Tm) -> u64 {
    // SAFETY: _mktime64 reads the struct and normalizes it in place.
    unsafe { _mktime64(tm) }.max(0) as u64
}

/// The day of the week of `secs`, 0 for Sunday.
pub fn weekday(secs: u64) -> u32 {
    tm_of(secs).tm_wday as u32
}

/// `days` after the day of `now`, at `hour` o'clock there.
pub fn at(now: u64, days: i32, hour: i32) -> u64 {
    let mut tm = tm_of(now);
    tm.tm_mday += days;
    tm.tm_hour = hour;
    tm.tm_min = 0;
    tm.tm_sec = 0;
    tm.tm_isdst = -1;
    secs_of(&mut tm)
}

/// "Sat 22 Nov", with the year when it is not this one's.
pub fn day(secs: u64, now: u64) -> String {
    let tm = tm_of(secs);
    let year = (tm.tm_year != tm_of(now).tm_year).then(|| format!(" {}", 1900 + tm.tm_year)).unwrap_or_default();
    format!("{} {} {}{year}", DAYS[tm.tm_wday.rem_euclid(7) as usize], tm.tm_mday, MONTHS[tm.tm_mon.rem_euclid(12) as usize])
}

/// "Sat 22 Nov, 09:00".
pub fn day_and_hour(secs: u64, now: u64) -> String {
    let tm = tm_of(secs);
    format!("{}, {:02}:{:02}", day(secs, now), tm.tm_hour, tm.tm_min)
}

/// "09:05", of a moment on the clock, in this machine's time on every system.
pub fn hour_minute(time: std::time::SystemTime) -> String {
    let tm = tm_of(time.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// "09:05:12".
pub fn clock(secs: u64) -> String {
    let tm = tm_of(secs);
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_ahead_at_an_hour_is_that_hour_there() {
        let now = 1_795_000_000;
        let then = at(now, 3, 9);
        let (tm, today) = (tm_of(then), tm_of(now));
        assert_eq!((tm.tm_hour, tm.tm_min), (9, 0));
        assert_eq!(weekday(then), (today.tm_wday as u32 + 3) % 7);
        assert!(then > now + 2 * 86_400 && then < now + 4 * 86_400);
        assert!(day_and_hour(then, now).ends_with(", 09:00"), "{}", day_and_hour(then, now));
        assert!(day(now + 400 * 86_400, now).split(' ').count() == 4, "a year that is not this one is said");
    }
}
