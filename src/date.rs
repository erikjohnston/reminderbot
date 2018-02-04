use regex::Regex;
use chrono::{DateTime, Datelike, Duration, Timelike, Utc, Weekday};
use failure::{Error, ResultExt};

pub fn parse_human_datetime(input: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, Error> {
    let input = input.trim().to_lowercase();

    if input == "next week" {
        let days = 7 - now.weekday().number_from_monday() + 1;
        return Ok(set_to_morning(now + Duration::days(days as i64)));
    }

    if input == "tomorrow" {
        return Ok(set_to_morning(now + Duration::days(1)));
    }
    if input == "day after tomorrow" {
        return Ok(set_to_morning(now + Duration::days(2)));
    }

    let mut date = if let Some(date) = parse_in_clause(&input, now)? {
        date
    } else if let Some(date) = parse_special_words(&input, now)? {
        date
    } else if let Some(date) = parse_on_day_clause(&input, now)? {
        date
    } else if let Some(date) = parse_on_date_clause(&input, now)? {
        date
    } else {
        bail!("couldn't parse duration")
    };

    date = parse_at_clause(&input, now, date)?;

    return Ok(date);
}

fn parse_in_clause(input: &str, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, Error> {
    let relative_time_regex = Regex::new(
        r"^in\s*([0-9]+|half an?|a couple of|a few)\s*(s|seconds?|m|minutes?|h|hours?|d|days?|w|weeks?|months?|years?)"
    ).expect("invalid regex");

    if let Some(capt) = relative_time_regex.captures(&input) {
        let number: f64 = match &capt[1] {
            "half a" | "half an" => 0.5,
            "a couple of" => 2.0,
            "a few" => 3.0,
            d => d.parse::<f64>().context("invalid number")?,
        };
        let dtype = &capt[2];

        if number > 10000000.0 {
            bail!("duration too large");
        }

        let dur = get_duration_from_string(dtype);
        let dur = Duration::seconds((dur.num_seconds() as f64 * number) as i64);

        let mut date = now + dur;

        // Clean up month/year if we were using integers
        if number >= 1.0 {
            match dtype {
                "month" | "months" => {
                    // Hack as its hard to add months.
                    date = date.with_day(now.day()).unwrap();
                }
                "year" | "years" => {
                    date = now.with_year(now.year() + number as i32).unwrap();
                }
                _ => {}
            }
        }

        if now + Duration::hours(48) < date {
            date = set_to_morning(date);
        }

        Ok(Some(date))
    } else {
        Ok(None)
    }
}

fn parse_special_words(input: &str, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, Error> {
    let special_time_regex = Regex::new(r"^(tomorrow|day after tomorrow)").expect("invalid regex");

    if let Some(capt) = special_time_regex.captures(&input) {
        match &capt[1] {
            "tomorrow" => Ok(Some(now + Duration::days(1))),
            "day after tomorrow" => Ok(Some(now + Duration::days(2))),
            _ => bail!("unexpectedly didn't match predefined match arms"),
        }
    } else {
        Ok(None)
    }
}

fn parse_on_day_clause(input: &str, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, Error> {
    let on_regex = Regex::new(r"(on\s+)?((mon|tues?|wed|thu?r?s?|fri|sat?|sun?)(day)?)")
        .expect("invalid regex");

    if let Some(capt) = on_regex.captures(&input) {
        let weekday: Weekday = capt[2]
            .parse::<Weekday>()
            .map_err(|_| format_err!("failed to parse day"))?;

        let mut date = now - Duration::days(now.weekday().num_days_from_monday() as i64);
        date = date + Duration::days(weekday.num_days_from_monday() as i64);

        if date < now {
            // If we're in the past then we should shoot forward a week
            date = date + Duration::weeks(1);
        }

        date = set_to_morning(date);

        Ok(Some(date))
    } else {
        Ok(None)
    }
}

fn parse_on_date_clause(input: &str, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, Error> {
    let full_date_regex = Regex::new(r"(on\s+)?(\d\d\d\d)-(\d\d)-(\d\d)").expect("invalid regex");

    if let Some(capt) = full_date_regex.captures(&input) {
        let mut year: i32 = capt[2].parse::<i32>().context("failed to parse year")?;
        let mut month: u32 = capt[3].parse::<u32>().context("failed to parse month")?;
        let mut day: u32 = capt[4].parse::<u32>().context("failed to parse day")?;

        let mut date = now;

        date = date.with_year(year).ok_or(format_err!("invalid year"))?;
        date = date.with_month(month).ok_or(format_err!("invalid month"))?;
        date = date.with_day(day).ok_or(format_err!("invalid day"))?;

        date = set_to_morning(date);

        Ok(Some(date))
    } else {
        Ok(None)
    }
}

fn parse_at_clause(
    input: &str,
    now: DateTime<Utc>,
    mut date: DateTime<Utc>,
) -> Result<DateTime<Utc>, Error> {
    let at_pm_regex = Regex::new(r"at (\d+)\s*(am|pm)").expect("invalid regex");

    let at_time_regex = Regex::new(r"at ((\d\d?):?(\d\d))").expect("invalid regex");

    date = if let Some(capt) = at_time_regex.captures(&input) {
        let hours: u32 = capt[2].parse::<u32>().context("invalid hours")?;
        let minutes: u32 = capt[3].parse::<u32>().context("invalid minutes")?;

        date = date.with_hour(hours).ok_or(format_err!("invalid hour"))?;
        date = date.with_minute(minutes)
            .ok_or(format_err!("invalid minutes"))?;
        date = date.with_second(0).ok_or(format_err!("invalid seconds"))?;

        date
    } else if let Some(capt) = at_pm_regex.captures(&input) {
        let hours: u32 = capt[1].parse::<u32>().context("invalid hours")?;
        let am_pm = &capt[2] == "pm";

        if am_pm {
            date = date.with_hour(hours + 12)
                .ok_or(format_err!("invalid hour"))?
        } else {
            date = date.with_hour(hours).ok_or(format_err!("invalid hour"))?
        }

        date
    } else {
        date
    };

    if date < now {
        // Uh oh, we've gone backwards. This is probably because we just
        // said "at 10:00" when we meant at 10:00 tomorrow, so lets just
        // add a day.

        date = date + Duration::days(1);
    }

    Ok(date)
}

fn set_to_morning(n: DateTime<Utc>) -> DateTime<Utc> {
    return n.with_hour(9)
        .unwrap()
        .with_minute(30)
        .unwrap()
        .with_second(0)
        .unwrap();
}

fn get_duration_from_string(s: &str) -> Duration {
    match s {
        "s" | "second" | "seconds" => return Duration::seconds(1),
        "m" | "minute" | "minutes" => return Duration::minutes(1),
        "h" | "hour" | "hours" => return Duration::hours(1),
        "d" | "day" | "days" => {
            return Duration::days(1);
        }
        "w" | "week" | "weeks" => return Duration::weeks(1),
        "month" | "months" => {
            // Hack as its hard to add months.
            return Duration::days(30);
        }
        "year" | "years" => return Duration::days(365),
        _ => panic!("unrecognized type"),
    }
}

#[test]
fn date_parse_test() {
    use chrono::TimeZone;

    let dt = Utc.ymd(2014, 7, 8).and_hms(9, 10, 11);

    assert_eq!(
        parse_human_datetime("tomorrow", dt).unwrap(),
        Utc.ymd(2014, 7, 9).and_hms(9, 30, 0)
    );

    assert_eq!(
        parse_human_datetime("tomorrow at 1800", dt).unwrap(),
        Utc.ymd(2014, 7, 9).and_hms(18, 00, 0)
    );

    assert!(parse_human_datetime("tomorrow at 9900", dt).is_err());

    assert_eq!(
        parse_human_datetime("in 1 week", dt).unwrap(),
        Utc.ymd(2014, 7, 15).and_hms(9, 30, 0)
    );

    assert_eq!(
        parse_human_datetime("in 1 week at 10:00", dt).unwrap(),
        Utc.ymd(2014, 7, 15).and_hms(10, 00, 0)
    );

    assert_eq!(
        parse_human_datetime("in a few hours", dt).unwrap(),
        Utc.ymd(2014, 7, 8).and_hms(12, 10, 11)
    );

    assert_eq!(
        parse_human_datetime("in half an hour", dt).unwrap(),
        Utc.ymd(2014, 7, 8).and_hms(9, 40, 11)
    );

    assert_eq!(
        parse_human_datetime("wed", dt).unwrap(),
        Utc.ymd(2014, 7, 9).and_hms(9, 30, 00)
    );

    assert_eq!(
        parse_human_datetime("on monday", dt).unwrap(),
        Utc.ymd(2014, 7, 14).and_hms(9, 30, 00)
    );

    assert_eq!(
        parse_human_datetime("on 2017-12-04", dt).unwrap(),
        Utc.ymd(2017, 12, 04).and_hms(9, 30, 00)
    );
}
