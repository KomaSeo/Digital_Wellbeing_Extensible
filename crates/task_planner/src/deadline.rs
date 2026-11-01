//! Responsibility: Read a deadline the user writes in everyday words ("이틀 뒤", "금요일",
//! "다음 주 수요일", "11/5", "in 3 days") as a date.
//!
//! The Focus app shows the date it read next to the field, so the user sees at once whether
//! it was understood. Weeks run Monday to Sunday. A weekday on its own is the next such day,
//! today included; "이번 주"/"this" gives the day of this week, or of next week once it has
//! passed; "다음 주"/"next" gives the day of next week.

use chrono::{Datelike, Duration, Months, NaiveDate, Weekday};

/// The date `text` names, counted from `today`; `None` when it cannot be read.
pub fn parse_deadline(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let lowered = text.trim().to_lowercase();
    let cleaned = strip_particles(&lowered);
    if cleaned.is_empty() {
        return None;
    }
    // Korean is matched without spaces ("이틀 뒤" = "이틀뒤"); English word by word.
    let compact: String = cleaned
        .chars()
        .filter(|letter| !letter.is_whitespace())
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    absolute_date(&compact, today)
        .or_else(|| named_day(&compact, &words, today))
        .or_else(|| relative_korean(&compact, today))
        .or_else(|| relative_english(&words, today))
        .or_else(|| weekday_date(&compact, &words, today))
        .or_else(|| month_end(&compact, &words, today))
}

/// Without "까지", "by", "due" and a final full stop.
fn strip_particles(lowered: &str) -> String {
    let mut text = lowered
        .trim()
        .trim_end_matches(['.', '!'])
        .trim()
        .to_string();
    for suffix in ["까지만", "까지", "전까지", "안에", "이내"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            text = stripped.trim().to_string();
        }
    }
    for prefix in ["by ", "due ", "until ", "on "] {
        if let Some(stripped) = text.strip_prefix(prefix) {
            text = stripped.trim().to_string();
        }
    }
    text
}

/// `2026-11-05`, `2026.11.05`, `2026/11/05`, `11/5`, `11-5`, `11.5`, `11월 5일`, `5일`.
fn absolute_date(compact: &str, today: NaiveDate) -> Option<NaiveDate> {
    let unified: String = compact
        .trim_end_matches('.')
        .replace(['.', '/'], "-")
        .replace("년", "-")
        .replace("월", "-")
        .replace('일', "");
    let parts: Vec<&str> = unified.trim_end_matches('-').split('-').collect();
    let numbers: Vec<u32> = parts
        .iter()
        .map(|part| part.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    match numbers.as_slice() {
        [year, month, day] if *year >= 1000 => {
            NaiveDate::from_ymd_opt(i32::try_from(*year).ok()?, *month, *day)
        }
        [month, day] => {
            let this_year = NaiveDate::from_ymd_opt(today.year(), *month, *day)?;
            if this_year >= today {
                Some(this_year)
            } else {
                NaiveDate::from_ymd_opt(today.year() + 1, *month, *day)
            }
        }
        // "5일": the 5th of this month, or of next month once it has passed.
        [day] if compact.ends_with('일') => {
            let this_month = today.with_day(*day);
            match this_month {
                Some(date) if date >= today => Some(date),
                _ => today.checked_add_months(Months::new(1))?.with_day(*day),
            }
        }
        _ => None,
    }
}

/// 오늘, 내일, 모레, 글피, today, tomorrow.
fn named_day(compact: &str, words: &[&str], today: NaiveDate) -> Option<NaiveDate> {
    let days = match compact {
        "오늘" | "today" => 0,
        "내일" | "tomorrow" => 1,
        "모레" | "내일모레" => 2,
        "글피" => 3,
        "그글피" => 4,
        "다음주" | "담주" => 7,
        _ => match words {
            ["the", "day", "after", "tomorrow"] | ["day", "after", "tomorrow"] => 2,
            _ => return None,
        },
    };
    Some(today + Duration::days(days))
}

/// "이틀 뒤", "3일 후", "일주일 뒤", "2주 후", "한 달 뒤", "3개월 후", "보름 뒤".
fn relative_korean(compact: &str, today: NaiveDate) -> Option<NaiveDate> {
    let amount_text = ["뒤", "후", "있다가", "지나서"]
        .iter()
        .find_map(|suffix| compact.strip_suffix(suffix))?;
    // Day counts with their own words.
    let day_words = [
        ("하루", 1),
        ("이틀", 2),
        ("사흘", 3),
        ("나흘", 4),
        ("닷새", 5),
        ("엿새", 6),
        ("이레", 7),
        ("여드레", 8),
        ("아흐레", 9),
        ("열흘", 10),
        ("보름", 15),
    ];
    if let Some((_, days)) = day_words.iter().find(|(word, _)| *word == amount_text) {
        return Some(today + Duration::days(*days));
    }
    if amount_text == "일주일" {
        return Some(today + Duration::days(7));
    }
    for (unit, kind) in [
        ("개월", Unit::Months),
        ("달", Unit::Months),
        ("주일", Unit::Weeks),
        ("주", Unit::Weeks),
        ("일", Unit::Days),
    ] {
        if let Some(count_text) = amount_text.strip_suffix(unit) {
            let count = korean_count(count_text)?;
            return add(today, count, kind);
        }
    }
    None
}

/// "in 3 days", "3 days later", "in a week", "2 weeks", "in a month", "next week",
/// "next month".
fn relative_english(words: &[&str], today: NaiveDate) -> Option<NaiveDate> {
    let words: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| !matches!(*word, "in" | "later" | "from" | "now" | "after"))
        .collect();
    let (count, unit_word) = match words.as_slice() {
        ["next", unit_word] => (1, *unit_word),
        [count_word, unit_word] => (english_count(count_word)?, *unit_word),
        [unit_word] => (1, *unit_word),
        _ => return None,
    };
    let kind = match unit_word.trim_end_matches('s') {
        "day" => Unit::Days,
        "week" => Unit::Weeks,
        "month" => Unit::Months,
        _ => return None,
    };
    add(today, count, kind)
}

/// "금요일", "금", "이번 주 금요일", "다음 주 수요일", "다다음 주 월요일", "주말", "friday",
/// "next friday", "this weekend".
fn weekday_date(compact: &str, words: &[&str], today: NaiveDate) -> Option<NaiveDate> {
    let (weeks_ahead, day_text) = if let Some(rest) = compact
        .strip_prefix("다다음주")
        .or_else(|| compact.strip_prefix("다다음"))
    {
        (Some(2), rest.to_string())
    } else if let Some(rest) = compact
        .strip_prefix("다음주")
        .or_else(|| compact.strip_prefix("담주"))
        .or_else(|| compact.strip_prefix("다음"))
    {
        (Some(1), rest.to_string())
    } else if let Some(rest) = compact
        .strip_prefix("이번주")
        .or_else(|| compact.strip_prefix("이번"))
    {
        (Some(0), rest.to_string())
    } else {
        match words {
            ["next", day_word] => (Some(1), day_word.to_string()),
            ["this", day_word] => (Some(0), day_word.to_string()),
            [day_word] => (None, day_word.to_string()),
            _ => (None, compact.to_string()),
        }
    };
    let weekday = weekday_of(&day_text)?;
    let days_from_monday = i64::from(today.weekday().num_days_from_monday());
    let monday_this_week = today - Duration::days(days_from_monday);
    let in_week = |weeks: i64| {
        monday_this_week
            + Duration::weeks(weeks)
            + Duration::days(i64::from(weekday.num_days_from_monday()))
    };
    Some(match weeks_ahead {
        // This week's day, or next week's once it has passed.
        Some(0) => {
            let this_week = in_week(0);
            if this_week >= today {
                this_week
            } else {
                in_week(1)
            }
        }
        Some(weeks) => in_week(weeks),
        // The next such day, today included.
        None => {
            let days_until =
                (i64::from(weekday.num_days_from_monday()) - days_from_monday).rem_euclid(7);
            today + Duration::days(days_until)
        }
    })
}

/// "월말", "이번 달 말", "다음 달 말", "다음 달 5일", "end of month", "end of next month".
fn month_end(compact: &str, words: &[&str], today: NaiveDate) -> Option<NaiveDate> {
    let (months_ahead, rest) = if let Some(rest) = compact.strip_prefix("다음달") {
        (1, rest)
    } else if let Some(rest) = compact.strip_prefix("이번달") {
        (0, rest)
    } else {
        match words {
            ["end", "of", "month"]
            | ["end", "of", "the", "month"]
            | ["end", "of", "this", "month"] => (0, "말"),
            ["end", "of", "next", "month"] => (1, "말"),
            _ if compact == "월말" || compact == "달말" => (0, "말"),
            _ => return None,
        }
    };
    let first_of_month = today
        .with_day(1)?
        .checked_add_months(Months::new(months_ahead))?;
    match rest {
        "말" | "월말" => first_of_month
            .checked_add_months(Months::new(1))
            .map(|first_of_next| first_of_next - Duration::days(1)),
        // "다음 달" on its own: the same day next month.
        "" => today.checked_add_months(Months::new(months_ahead)),
        day_text => {
            let day = day_text.strip_suffix('일')?.parse::<u32>().ok()?;
            first_of_month.with_day(day)
        }
    }
}

#[derive(Clone, Copy)]
enum Unit {
    Days,
    Weeks,
    Months,
}

fn add(today: NaiveDate, count: u32, unit: Unit) -> Option<NaiveDate> {
    if count > 3650 {
        return None;
    }
    match unit {
        Unit::Days => Some(today + Duration::days(i64::from(count))),
        Unit::Weeks => Some(today + Duration::weeks(i64::from(count))),
        Unit::Months => today.checked_add_months(Months::new(count)),
    }
}

/// "3", "한", "두", "세", "네", "다섯", … "열", and "이" (as in "이주").
fn korean_count(text: &str) -> Option<u32> {
    if let Ok(count) = text.parse::<u32>() {
        return Some(count);
    }
    let words = [
        ("한", 1),
        ("두", 2),
        ("이", 2),
        ("세", 3),
        ("삼", 3),
        ("네", 4),
        ("사", 4),
        ("다섯", 5),
        ("여섯", 6),
        ("일곱", 7),
        ("여덟", 8),
        ("아홉", 9),
        ("열", 10),
    ];
    words
        .iter()
        .find(|(word, _)| *word == text)
        .map(|(_, count)| *count)
}

fn english_count(word: &str) -> Option<u32> {
    if let Ok(count) = word.parse::<u32>() {
        return Some(count);
    }
    let words = [
        ("a", 1),
        ("an", 1),
        ("one", 1),
        ("two", 2),
        ("three", 3),
        ("four", 4),
        ("five", 5),
        ("six", 6),
        ("seven", 7),
        ("eight", 8),
        ("nine", 9),
        ("ten", 10),
    ];
    words
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, count)| *count)
}

fn weekday_of(text: &str) -> Option<Weekday> {
    let text = text.trim_end_matches("요일");
    Some(match text {
        "월" | "monday" | "mon" => Weekday::Mon,
        "화" | "tuesday" | "tue" | "tues" => Weekday::Tue,
        "수" | "wednesday" | "wed" => Weekday::Wed,
        "목" | "thursday" | "thu" | "thurs" => Weekday::Thu,
        "금" | "friday" | "fri" => Weekday::Fri,
        "토" | "saturday" | "sat" | "주말" | "weekend" => Weekday::Sat,
        "일" | "sunday" | "sun" => Weekday::Sun,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-01 is a Sunday.
    fn read(text: &str) -> Option<String> {
        let today = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        parse_deadline(text, today).map(|date| date.to_string())
    }

    #[test]
    fn everyday_korean_deadlines_are_read() {
        for (text, expected) in [
            ("오늘", "2026-11-01"),
            ("내일까지", "2026-11-02"),
            ("모레", "2026-11-03"),
            ("이틀 뒤", "2026-11-03"),
            ("이틀뒤", "2026-11-03"),
            ("3일 후", "2026-11-04"),
            ("사흘 뒤까지", "2026-11-04"),
            ("일주일 뒤", "2026-11-08"),
            ("2주 후", "2026-11-15"),
            ("이주 뒤", "2026-11-15"),
            ("한 달 뒤", "2026-12-01"),
            ("3개월 후", "2027-02-01"),
            ("보름 뒤", "2026-11-16"),
            ("다음 주", "2026-11-08"),
            ("금요일", "2026-11-06"),
            ("금", "2026-11-06"),
            ("일요일", "2026-11-01"),
            ("이번 주 금요일", "2026-11-06"),
            ("다음 주 수요일", "2026-11-04"),
            ("다다음 주 월요일", "2026-11-09"),
            ("주말", "2026-11-07"),
            ("월말", "2026-11-30"),
            ("다음 달 말", "2026-12-31"),
            ("다음 달 5일", "2026-12-05"),
            ("11월 5일", "2026-11-05"),
            ("10월 5일", "2027-10-05"),
            ("5일", "2026-11-05"),
            ("1일", "2026-11-01"),
            ("11/20", "2026-11-20"),
            ("2026-12-24", "2026-12-24"),
            ("2026.12.24.", "2026-12-24"),
            ("2027년 1월 3일", "2027-01-03"),
        ] {
            assert_eq!(read(text).as_deref(), Some(expected), "{text}");
        }
    }

    #[test]
    fn everyday_english_deadlines_are_read() {
        for (text, expected) in [
            ("today", "2026-11-01"),
            ("Tomorrow", "2026-11-02"),
            ("the day after tomorrow", "2026-11-03"),
            ("in 3 days", "2026-11-04"),
            ("2 days later", "2026-11-03"),
            ("in a week", "2026-11-08"),
            ("next week", "2026-11-08"),
            ("in two months", "2027-01-01"),
            ("friday", "2026-11-06"),
            ("by Friday", "2026-11-06"),
            ("next wed", "2026-11-04"),
            ("this saturday", "2026-11-07"),
            ("end of month", "2026-11-30"),
            ("end of next month", "2026-12-31"),
        ] {
            assert_eq!(read(text).as_deref(), Some(expected), "{text}");
        }
    }

    #[test]
    fn unreadable_text_gives_no_date() {
        for text in [
            "",
            "언젠가",
            "soon",
            "13/45",
            "다음 주 아무때나",
            "99999일 뒤",
        ] {
            assert_eq!(read(text), None, "{text}");
        }
    }
}
