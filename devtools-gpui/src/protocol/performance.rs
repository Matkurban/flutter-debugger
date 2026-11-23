use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    pub number: u64,
    pub build_micros: u64,
    pub raster_micros: u64,
    pub elapsed_micros: u64,
    pub vsync_overhead_micros: u64,
}

impl FrameTiming {
    /// Parses a `Flutter.Frame` extension event from Flutter's `SchedulerBinding`.
    /// Preserves the genuine `elapsed` field without artificially overriding it with `build + raster`.
    pub fn parse_flutter_frame(ext_data: &Value) -> Option<Self> {
        let number = ext_data.get("number").and_then(Value::as_u64)?;
        let build_micros = ext_data.get("build").and_then(Value::as_u64).unwrap_or(0);
        let raster_micros = ext_data.get("raster").and_then(Value::as_u64).unwrap_or(0);
        let elapsed_micros = ext_data
            .get("elapsed")
            .and_then(Value::as_u64)
            .unwrap_or(build_micros.max(raster_micros));
        let vsync_overhead_micros = ext_data
            .get("vsyncOverhead")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        Some(Self {
            number,
            build_micros,
            raster_micros,
            elapsed_micros,
            vsync_overhead_micros,
        })
    }

    pub fn build_ms(self) -> f64 {
        (self.build_micros as f64) / 1000.0
    }

    pub fn raster_ms(self) -> f64 {
        (self.raster_micros as f64) / 1000.0
    }

    pub fn total_ms(self) -> f64 {
        (self.elapsed_micros as f64) / 1000.0
    }

    /// Official Flutter DevTools pipeline jank criterion: UI and Raster threads run in a pipelined
    /// fashion, so a frame is janky if and only if either UI build OR Raster exceeds the frame budget.
    pub fn is_jank(self, budget_ms: f64) -> bool {
        self.build_ms() > budget_ms || self.raster_ms() > budget_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pipelined_frame_within_budget_is_not_jank() {
        // UI=10ms, Raster=10ms, elapsed=20ms at 60fps (16.67ms budget) -> NOT jank!
        let smooth_pipelined = FrameTiming::parse_flutter_frame(&json!({
            "number": 1,
            "build": 10_000,
            "raster": 10_000,
            "elapsed": 20_000,
            "vsyncOverhead": 120
        }))
        .unwrap();
        assert!(!smooth_pipelined.is_jank(16.67));
        assert!((smooth_pipelined.total_ms() - 20.0).abs() < 0.01);

        // UI=17ms, Raster=6ms -> jank
        let ui_jank = FrameTiming::parse_flutter_frame(&json!({
            "number": 2,
            "build": 17_000,
            "raster": 6_000,
            "elapsed": 23_000
        }))
        .unwrap();
        assert!(ui_jank.is_jank(16.67));

        // UI=5ms, Raster=17ms -> jank
        let raster_jank = FrameTiming::parse_flutter_frame(&json!({
            "number": 3,
            "build": 5_000,
            "raster": 17_000,
            "elapsed": 22_000
        }))
        .unwrap();
        assert!(raster_jank.is_jank(16.67));
    }
}
