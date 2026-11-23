use serde_json::Value;

use crate::theme::current_local_hms;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub timestamp_hms: String,
    pub heap_usage_bytes: u64,
    pub heap_capacity_bytes: u64,
    pub external_usage_bytes: u64,
    pub rss_bytes: u64,
    pub rss_is_estimated: bool,
    pub is_gc_event: bool,
}

/// Parses process RSS from official Dart VM Service `getProcessMemoryUsage` response (`root.size`).
pub fn parse_process_memory_rss(v: &Value) -> Option<u64> {
    let root = v.get("result").unwrap_or(v);
    root.get("root")
        .and_then(|r| r.get("size"))
        .and_then(Value::as_u64)
        .filter(|&size| size > 0)
}

impl MemorySnapshot {
    pub fn parse_memory_usage(
        v: &Value,
        process_rss_bytes: Option<u64>,
        is_gc_event: bool,
    ) -> Option<Self> {
        let root = v.get("result").unwrap_or(v);
        let heap_usage_bytes = root.get("heapUsage").and_then(Value::as_u64)?;
        let heap_capacity_bytes = root
            .get("heapCapacity")
            .and_then(Value::as_u64)
            .unwrap_or(heap_usage_bytes);
        let external_usage_bytes = root
            .get("externalUsage")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        let (rss_bytes, rss_is_estimated) = match process_rss_bytes.filter(|&b| b > 0) {
            Some(real_rss) => (real_rss, false),
            None => (heap_capacity_bytes + external_usage_bytes, true),
        };

        Some(Self {
            timestamp_hms: current_local_hms(),
            heap_usage_bytes,
            heap_capacity_bytes,
            external_usage_bytes,
            rss_bytes,
            rss_is_estimated,
            is_gc_event,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_memory_usage_and_process_memory_usage_root_size() {
        let proc_mem = json!({
            "type": "ProcessMemoryUsage",
            "root": {
                "name": "vm",
                "description": "Dart VM Process Memory",
                "size": 188_743_680u64,
                "children": []
            }
        });
        let rss = parse_process_memory_rss(&proc_mem);
        assert_eq!(rss, Some(188_743_680));

        let mem_usage = json!({
            "type": "MemoryUsage",
            "heapUsage": 42_000_000u64,
            "heapCapacity": 64_000_000u64,
            "externalUsage": 8_000_000u64
        });

        let snap = MemorySnapshot::parse_memory_usage(&mem_usage, rss, false).expect("parse mem");
        assert_eq!(snap.heap_usage_bytes, 42_000_000);
        assert_eq!(snap.heap_capacity_bytes, 64_000_000);
        assert_eq!(snap.external_usage_bytes, 8_000_000);
        assert_eq!(snap.rss_bytes, 188_743_680);
        assert!(!snap.rss_is_estimated);

        let fallback_snap =
            MemorySnapshot::parse_memory_usage(&mem_usage, None, true).expect("parse fallback");
        assert_eq!(fallback_snap.rss_bytes, 72_000_000);
        assert!(fallback_snap.rss_is_estimated);
        assert!(fallback_snap.is_gc_event);
    }
}
