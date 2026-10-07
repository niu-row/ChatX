use super::call_history_path;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs};

pub(super) fn parse_call_history(
    text: &str,
    limit: usize,
    tool_name: Option<&str>,
    status: Option<&str>,
) -> Value {
    let mut records = Vec::new();
    let mut tool_names = BTreeSet::new();
    let mut total = 0usize;
    let mut invalid_entries = 0usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(mut record) = serde_json::from_str::<Value>(line) else {
            invalid_entries += 1;
            continue;
        };
        let Some(name) = record
            .get("toolName")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            invalid_entries += 1;
            continue;
        };
        total += 1;
        tool_names.insert(name.clone());
        let success = record
            .get("output")
            .and_then(|output| output.get("isError"))
            .and_then(Value::as_bool)
            != Some(true);
        if tool_name.is_some_and(|filter| filter != name) {
            continue;
        }
        if status == Some("success") && !success {
            continue;
        }
        if status == Some("error") && success {
            continue;
        }
        if let Value::Object(object) = &mut record {
            object.insert("success".into(), Value::Bool(success));
        }
        records.push(record);
    }

    let filtered_total = records.len();
    let success_count = records
        .iter()
        .filter(|record| {
            record.get("success").and_then(Value::as_bool) == Some(true)
        })
        .count();
    let error_count = filtered_total.saturating_sub(success_count);
    let mut durations = records
        .iter()
        .filter_map(|record| record.get("duration").and_then(Value::as_u64))
        .collect::<Vec<_>>();
    durations.sort_unstable();
    let average_duration_ms = if durations.is_empty() {
        None
    } else {
        let total_duration = durations
            .iter()
            .map(|value| *value as f64)
            .sum::<f64>();
        Some(
            (total_duration / durations.len() as f64 * 10.0).round()
                / 10.0,
        )
    };
    let p95_duration_ms = if durations.is_empty() {
        None
    } else {
        let index = (durations.len() * 95)
            .div_ceil(100)
            .saturating_sub(1);
        durations.get(index).copied()
    };

    records.reverse();
    records.truncate(limit);
    json!({
        "items": records,
        "total": total,
        "filteredTotal": filtered_total,
        "toolNames": tool_names.into_iter().collect::<Vec<_>>(),
        "invalidEntries": invalid_entries,
        "stats": {
            "successCount": success_count,
            "errorCount": error_count,
            "averageDurationMs": average_duration_ms,
            "p95DurationMs": p95_duration_ms
        }
    })
}

#[tauri::command]
pub(super) fn get_call_history(
    app: tauri::AppHandle,
    limit: Option<usize>,
    tool_name: Option<String>,
    status: Option<String>,
) -> Result<Value, String> {
    let limit = limit.unwrap_or(100).clamp(1, 1000);
    let tool_name = tool_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let status = status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if !matches!(status, None | Some("success") | Some("error")) {
        return Err(
            "调用记录状态筛选仅支持 success 或 error。".into(),
        );
    }
    let path = call_history_path(&app)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            String::new()
        }
        Err(error) => {
            return Err(format!("读取调用记录失败：{error}"));
        }
    };
    Ok(parse_call_history(
        &text,
        limit,
        tool_name,
        status,
    ))
}

#[tauri::command]
pub(super) fn clear_call_history(
    app: tauri::AppHandle,
) -> Result<Value, String> {
    let path = call_history_path(&app)?;
    if path.exists() {
        fs::write(&path, "")
            .map_err(|e| format!("清空调用记录失败：{e}"))?;
    }
    Ok(json!({"cleared": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_filters_and_skips_bad_lines() {
        let text = concat!(
            "{\"timestamp\":\"2026-09-18T08:00:00.000Z\",\"toolName\":\"read_file\",\"arguments\":{},\"output\":{},\"duration\":4}\n",
            "not-json\n",
            "{\"timestamp\":\"2026-09-18T08:00:01.000Z\",\"toolName\":\"write_file\",\"arguments\":{},\"output\":{\"isError\":true},\"duration\":7}\n"
        );
        let all = parse_call_history(text, 10, None, None);
        assert_eq!(all["total"], 2);
        assert_eq!(all["invalidEntries"], 1);
        assert_eq!(all["items"][0]["toolName"], "write_file");
        assert_eq!(all["items"][0]["success"], false);
        assert_eq!(all["stats"]["successCount"], 1);
        assert_eq!(all["stats"]["errorCount"], 1);
        assert_eq!(all["stats"]["averageDurationMs"], 5.5);
        assert_eq!(all["stats"]["p95DurationMs"], 7);
        let limited = parse_call_history(text, 1, None, None);
        assert_eq!(limited["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(limited["filteredTotal"], 2);
        assert_eq!(limited["stats"]["successCount"], 1);

        let failed = parse_call_history(text, 10, None, Some("error"));
        assert_eq!(failed["filteredTotal"], 1);
        assert_eq!(failed["items"][0]["toolName"], "write_file");

        let reads = parse_call_history(
            text,
            10,
            Some("read_file"),
            Some("success"),
        );
        assert_eq!(reads["filteredTotal"], 1);
        assert_eq!(reads["items"][0]["success"], true);
    }
}
