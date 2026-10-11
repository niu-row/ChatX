use crate::mcp_sources;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, io::{Read, Seek, SeekFrom}, path::Path};

const HISTORY_MAX_BYTES_PER_TRANSPORT: u64 = 2 * 1024 * 1024;

fn read_call_history_tail(path: &Path, max_bytes: u64) -> Result<String, String> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("打开调用记录失败：{error}")),
    };
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    // Bound memory even if Desktop Commander appends while the file is open.
    file.take(max_bytes).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let mut content = String::from_utf8_lossy(&bytes).to_string();
    if start > 0 {
        if let Some(pos) = content.find('\n') { content = content[pos + 1..].to_string(); }
        else { content.clear(); }
    }
    Ok(content)
}


#[cfg(test)]
pub(super) fn parse_call_history(
    text: &str, limit: usize, tool_name: Option<&str>, status: Option<&str>,
) -> Value {
    parse_call_histories(&[(mcp_sources::TUNNEL, text)], limit, tool_name, status)
}

pub(super) fn parse_call_histories(
    sources: &[(&str, &str)], limit: usize,
    tool_name: Option<&str>, status: Option<&str>,
) -> Value {
    let mut records = Vec::new();
    let mut tool_names = BTreeSet::new();
    let mut total = 0usize;
    let mut invalid_entries = 0usize;
    for (source, text) in sources {
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
            object.insert("source".into(), Value::String((*source).into()));
        }
        records.push(record);
      }
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

    records.sort_by(|a, b| {
        let at = a.get("timestamp").and_then(Value::as_str).unwrap_or("");
        let bt = b.get("timestamp").and_then(Value::as_str).unwrap_or("");
        bt.cmp(at)
    });
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
    let mut texts = Vec::new();
    for (source, path) in mcp_sources::history_paths(&app)? {
        let text = read_call_history_tail(&path, HISTORY_MAX_BYTES_PER_TRANSPORT)
            .map_err(|e| format!("读取 {source} 调用记录失败：{e}"))?;
        texts.push((source, text));
    }
    let refs = texts.iter().map(|(source, text)| (*source, text.as_str())).collect::<Vec<_>>();
    Ok(parse_call_histories(&refs, limit, tool_name, status))
}

#[tauri::command]
pub(super) fn clear_call_history(
    app: tauri::AppHandle,
) -> Result<Value, String> {
    for (source, path) in mcp_sources::history_paths(&app)? {
        if path.exists() {
            fs::write(&path, "")
                .map_err(|e| format!("清空 {source} 调用记录失败：{e}"))?;
        }
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

    #[test]
    fn bounded_history_tail_skips_partial_lines() {
        let dir = std::env::temp_dir().join(format!(
            "chatx-history-review-{}-{:?}", std::process::id(), std::thread::current().id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("calls.jsonl");
        let mut text = String::new();
        for i in 0..150 {
            text.push_str(&format!(
                "{{\"timestamp\":\"2026-10-11T12:{:02}:00Z\",\"toolName\":\"call-{}\"}}\n",
                i % 60, i
            ));
        }
        fs::write(&file, text).unwrap();
        let tail = read_call_history_tail(&file, 512).unwrap();
        assert!(tail.len() < 512);
        assert!(tail.contains("call-149"));
        assert!(!tail.contains("call-0\""));
        assert!(tail.lines().all(|line| serde_json::from_str::<Value>(line).is_ok()));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mixed_transport_history_is_tagged_and_sorted() {
        let tunnel = r#"{"timestamp":"2026-10-11T09:00:00Z","toolName":"read_file","output":{},"duration":7}"#;
        let https = r#"{"timestamp":"2026-10-11T09:01:00Z","toolName":"write_file","output":{},"duration":12}"#;
        let joined = parse_call_histories(
            &[(mcp_sources::TUNNEL, tunnel), (mcp_sources::HTTPS, https)],
            20, None, None,
        );
        assert_eq!(joined["total"], 2);
        assert_eq!(joined["items"][0]["source"], mcp_sources::HTTPS);
        assert_eq!(joined["items"][1]["source"], mcp_sources::TUNNEL);
        let limited = parse_call_histories(
            &[(mcp_sources::TUNNEL, tunnel), (mcp_sources::HTTPS, https)],
            1, None, None,
        );
        assert_eq!(limited["items"].as_array().unwrap().len(), 1);
        assert_eq!(limited["filteredTotal"], 2);
    }
}
