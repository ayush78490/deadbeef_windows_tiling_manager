//! strftime-style formatting and month grids from `GetLocalTime`.

use windows::Win32::Foundation::SYSTEMTIME;

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Supports %H %M %S %I %p %a %A %d %e %b %B %m %Y %y %%.
pub fn format(t: &SYSTEMTIME, fmt: &str) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let dow = DAYS[t.wDayOfWeek as usize % 7];
        let mon = MONTHS[(t.wMonth as usize).clamp(1, 12) - 1];
        let h12 = match t.wHour % 12 {
            0 => 12,
            h => h,
        };
        match chars.next() {
            Some('H') => out += &format!("{:02}", t.wHour),
            Some('M') => out += &format!("{:02}", t.wMinute),
            Some('S') => out += &format!("{:02}", t.wSecond),
            Some('I') => out += &format!("{h12:02}"),
            Some('p') => out += if t.wHour < 12 { "AM" } else { "PM" },
            Some('a') => out += &dow[..3],
            Some('A') => out += dow,
            Some('d') => out += &format!("{:02}", t.wDay),
            Some('e') => out += &t.wDay.to_string(),
            Some('b') => out += &mon[..3],
            Some('B') => out += mon,
            Some('m') => out += &format!("{:02}", t.wMonth),
            Some('Y') => out += &t.wYear.to_string(),
            Some('y') => out += &format!("{:02}", t.wYear % 100),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

pub fn days_in_month(year: u16, month: u16) -> u16 {
    match month {
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Weekday (0 = Sunday) of the 1st of the current month.
pub fn first_weekday(t: &SYSTEMTIME) -> u16 {
    (t.wDayOfWeek + 7 * 6 - (t.wDay - 1) % 7) % 7
}

pub fn month_name(t: &SYSTEMTIME) -> &'static str {
    MONTHS[(t.wMonth as usize).clamp(1, 12) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(y: u16, mo: u16, d: u16, dow: u16, h: u16, mi: u16) -> SYSTEMTIME {
        SYSTEMTIME { wYear: y, wMonth: mo, wDay: d, wDayOfWeek: dow, wHour: h, wMinute: mi, ..Default::default() }
    }

    #[test]
    fn formats() {
        // 14 Oct 2026 is a Wednesday.
        let t = st(2026, 10, 14, 3, 22, 5);
        assert_eq!(format(&t, "%a %d %b  %H:%M"), "Wed 14 Oct  22:05");
        assert_eq!(format(&t, "%I:%M %p, %A %e %B %Y"), "10:05 PM, Wednesday 14 October 2026");
    }

    #[test]
    fn calendar() {
        // October 2026 starts on a Thursday (4).
        assert_eq!(first_weekday(&st(2026, 10, 14, 3, 0, 0)), 4);
        assert_eq!(first_weekday(&st(2026, 10, 1, 4, 0, 0)), 4);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
    }
}
