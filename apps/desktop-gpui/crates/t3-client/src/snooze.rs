//! Snooze presets match the existing client's local calendar rules.

use jiff::{ToSpan, Zoned};

use crate::shell::{ThreadShell, timestamp_ms};

pub struct Preset {
    pub label: &'static str,
    pub wake: Zoned,
}

pub fn presets(now: &Zoned) -> Result<Vec<Preset>, jiff::Error> {
    let at_hour = |day: &Zoned, hour| {
        day.with()
            .hour(hour)
            .minute(0)
            .second(0)
            .subsec_nanosecond(0)
            .build()
    };
    let mut presets = vec![
        Preset {
            label: "In 1 hour",
            wake: now.checked_add(1.hour())?,
        },
        Preset {
            label: "In 3 hours",
            wake: now.checked_add(3.hours())?,
        },
    ];
    let evening = at_hour(now, 18)?;
    if evening.timestamp().as_millisecond() - now.timestamp().as_millisecond() > 3_600_000 {
        presets.push(Preset {
            label: "This evening",
            wake: evening,
        });
    }
    // Calendar arithmetic keeps 9am on the next local day across DST.
    let tomorrow = at_hour(&now.checked_add(1.day())?, 9)?;
    let days_to_monday = 7 - i64::from(now.weekday().to_monday_zero_offset());
    let next_week = at_hour(&now.checked_add(days_to_monday.days())?, 9)?;
    let distinct_next_week = next_week != tomorrow;
    presets.push(Preset {
        label: "Tomorrow",
        wake: tomorrow,
    });
    if distinct_next_week {
        presets.push(Preset {
            label: "Next week",
            wake: next_week,
        });
    }
    Ok(presets)
}

pub fn current_presets() -> Result<Vec<Preset>, jiff::Error> {
    presets(&Zoned::now())
}

pub fn valid_until(until: &str, now_ms: i64) -> bool {
    timestamp_ms(Some(until)).is_some_and(|wake| wake > now_ms)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Unit {
    Minutes,
    Hours,
    Days,
}

impl Unit {
    pub fn label(self) -> &'static str {
        match self {
            Self::Minutes => "Minutes",
            Self::Hours => "Hours",
            Self::Days => "Days",
        }
    }
}

pub enum Custom<'a> {
    DateTime { date: &'a str, time: &'a str },
    Duration { amount: &'a str, unit: Unit },
}

pub fn resolve_custom(input: Custom<'_>, now: &Zoned) -> Option<String> {
    let wake = match input {
        Custom::DateTime { date, time } => {
            if date.len() != 10 || time.len() != 5 {
                return None;
            }
            let local = format!("{date}T{time}:00")
                .parse::<jiff::civil::DateTime>()
                .ok()?;
            let wake = local.to_zoned(now.time_zone().clone()).ok()?;
            // Compatible DST resolution can roll a nonexistent time forward.
            if wake.datetime() != local {
                return None;
            }
            wake
        }
        Custom::Duration { amount, unit } => {
            let amount = amount.trim().parse::<f64>().ok()?;
            if !amount.is_finite() || amount <= 0. {
                return None;
            }
            let seconds = amount
                * match unit {
                    Unit::Minutes => 60.,
                    Unit::Hours => 3600.,
                    Unit::Days => 86400.,
                };
            now.checked_add(jiff::SignedDuration::try_from_secs_f64(seconds).ok()?)
                .ok()?
        }
    };
    (wake.timestamp() > now.timestamp()).then(|| wake.timestamp().to_string())
}

pub fn resolve_custom_now(input: Custom<'_>) -> Option<String> {
    resolve_custom(input, &Zoned::now())
}

pub fn initial_custom_date_time() -> Result<(String, String), jiff::Error> {
    let initial = Zoned::now().checked_add(1.hour())?;
    Ok((
        initial.date().to_string(),
        format!("{:02}:{:02}", initial.hour(), initial.minute()),
    ))
}

pub fn can_snooze(thread: &ThreadShell, now_ms: i64) -> bool {
    if thread.pending_runtime_request.is_some()
        || matches!(
            thread.runtime_status(),
            Some("preparing" | "queued" | "starting")
        )
    {
        return false;
    }
    let Some(message) = timestamp_ms(thread.latest_user_message_at.as_deref()) else {
        return true;
    };
    if thread.runtime_status() == Some("failed") || now_ms.abs_diff(message) > 120_000 {
        return true;
    }
    [
        thread.latest_run_requested_at.as_deref(),
        thread.latest_run_started_at.as_deref(),
        thread.latest_run_completed_at(),
    ]
    .into_iter()
    .filter_map(timestamp_ms)
    .any(|run| run >= message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preset_choices_drop_evening_when_near_and_duplicate_monday_on_sunday() {
        let late = "2026-04-08T17:30:00+05:30[Asia/Kolkata]".parse().unwrap();
        assert_eq!(
            presets(&late)
                .unwrap()
                .iter()
                .map(|p| p.label)
                .collect::<Vec<_>>(),
            ["In 1 hour", "In 3 hours", "Tomorrow", "Next week"]
        );
        let sunday = "2026-08-30T07:01:00+05:30[Asia/Kolkata]".parse().unwrap();
        assert_eq!(
            presets(&sunday)
                .unwrap()
                .iter()
                .map(|p| p.label)
                .collect::<Vec<_>>(),
            ["In 1 hour", "In 3 hours", "This evening", "Tomorrow"]
        );
    }

    #[test]
    fn tomorrow_uses_the_local_calendar_across_both_dst_transitions() {
        for (now, date) in [
            ("2026-03-07T23:30:00-05:00[America/New_York]", "2026-03-08"),
            ("2026-10-31T23:30:00-04:00[America/New_York]", "2026-11-01"),
        ] {
            let choices = presets(&now.parse().unwrap()).unwrap();
            let tomorrow = &choices.iter().find(|p| p.label == "Tomorrow").unwrap().wake;
            assert_eq!(tomorrow.date().to_string(), date);
            assert_eq!(tomorrow.hour(), 9);
        }
    }

    #[test]
    fn custom_dates_reject_invalid_past_and_nonexistent_local_times() {
        let now = "2026-03-07T12:00:00-05:00[America/New_York]"
            .parse()
            .unwrap();
        for (date, time) in [
            ("2026-02-30", "09:00"),
            ("2026-03-06", "09:00"),
            ("2026-03-08", "02:30"),
            ("2026-03-08", "25:00"),
        ] {
            assert_eq!(resolve_custom(Custom::DateTime { date, time }, &now), None);
        }
        assert_eq!(
            resolve_custom(
                Custom::DateTime {
                    date: "2026-03-08",
                    time: "09:00"
                },
                &now
            ),
            Some("2026-03-08T13:00:00Z".into())
        );
    }

    #[test]
    fn custom_durations_allow_fractions_and_use_elapsed_time_for_days() {
        let now = "2026-03-07T12:00:00-05:00[America/New_York]"
            .parse()
            .unwrap();
        for amount in ["", "0", "-1", "NaN", "inf", "1e100"] {
            assert_eq!(
                resolve_custom(
                    Custom::Duration {
                        amount,
                        unit: Unit::Hours
                    },
                    &now
                ),
                None
            );
        }
        assert_eq!(
            resolve_custom(
                Custom::Duration {
                    amount: "1.5",
                    unit: Unit::Hours
                },
                &now
            ),
            Some("2026-03-07T18:30:00Z".into())
        );
        assert_eq!(
            resolve_custom(
                Custom::Duration {
                    amount: "1",
                    unit: Unit::Days
                },
                &now
            ),
            Some("2026-03-08T17:00:00Z".into())
        );
    }

    #[test]
    fn pending_questions_and_queued_starts_block_snoozing_but_running_work_does_not() {
        let now = "2026-01-01T12:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        for (patch, allowed) in [
            (
                json!({"pendingRuntimeRequest":{"kind":"user_input", "blocking":false}}),
                false,
            ),
            (json!({"status":"queued", "latestRunId":"r"}), false),
            (json!({"status":"running", "latestRunId":"r"}), true),
            (json!({"latestUserMessageAt":"2026-01-01T11:59:59Z"}), false),
            (json!({"latestUserMessageAt":"2026-01-01T11:00:00Z"}), true),
            (
                json!({"latestUserMessageAt":"2026-01-01T11:59:59Z", "latestRunId":"r", "latestRunStartedAt":"2026-01-01T12:00:00Z"}),
                true,
            ),
        ] {
            let mut value = json!({"id":"t", "projectId":"p", "title":"Test", "status":"idle", "createdAt":"2026-01-01T00:00:00Z", "updatedAt":"2026-01-01T00:00:00Z"});
            value
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert_eq!(
                can_snooze(&serde_json::from_value(value).unwrap(), now),
                allowed,
                "{patch}"
            );
        }
    }
}
