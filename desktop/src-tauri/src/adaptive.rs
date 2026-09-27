// desktop/src-tauri/src/adaptive.rs
//! 自适应分块并发控制器:把每个任务的分块并发作为可调窗口。
//!
//! 这是应用层启发式算法,借鉴拥塞控制和服务端并发限制的思想,不等同于 TCP BBR。
//! 速率来自 Rust 已入账的分块字节,会受网络、IPC 与落盘共同影响,因此使用 EWMA、
//! 连续窗口确认和探测观察期,避免单个短窗口就触发并发来回变化。

use std::time::{Duration, Instant};

/// EWMA 每个采样窗口的权重;2 秒采样时可平滑短时分块/写盘突发。
const RATE_EWMA_ALPHA: f64 = 0.35;
/// 速率持续低于参考值该比例时开始累计退避窗口。
const DROP_RATIO: f64 = 0.80;
/// 需要连续多个低速窗口才退避。
const DROP_CONFIRMATION_SAMPLES: u32 = 3;
/// 速率持续高于参考值该比例时才允许增大并发。
const IMPROVE_RATIO: f64 = 1.10;
/// 增益需要连续多个窗口确认。
const IMPROVE_CONFIRMATION_SAMPLES: u32 = 3;
/// 两次平台期探测之间的最短间隔。
const PROBE_INTERVAL: Duration = Duration::from_secs(10);
/// 探测 +1 后留出足够时间收集多个样本再判断。
const PROBE_SETTLE: Duration = Duration::from_secs(8);
/// 长时间没有确认增益后重置失败计数,允许重新探测。
const IDLE_RESET: Duration = Duration::from_secs(30);
/// 连续探测无收益的次数上限。
const MAX_FAILED_PROBES: u32 = 2;

#[derive(Debug, Clone)]
pub struct AdaptiveConcurrency {
    width: usize,
    min: usize,
    max: usize,
    /// 已知安全上限。退避时保留,成功探测后逐步抬高。
    ceiling: usize,
    /// 当前窗口的 EWMA 速率。
    smoothed_rate: f64,
    /// 当前并发度对应的速率参考值。
    baseline: f64,
    improved_at: Instant,
    last_change_at: Instant,
    low_rate_samples: u32,
    high_rate_samples: u32,
    /// 探测开始时的平滑速率,用于探测结果比较。
    probe: Option<(Instant, f64)>,
    failed_probes: u32,
}

impl AdaptiveConcurrency {
    pub fn new(min: usize, initial: usize, max: usize) -> Self {
        let max = max.max(1);
        let min = min.clamp(1, max);
        let width = initial.clamp(min, max);
        let now = Instant::now();
        Self {
            width,
            min,
            max,
            ceiling: max,
            smoothed_rate: 0.0,
            baseline: 0.0,
            improved_at: now,
            last_change_at: now,
            low_rate_samples: 0,
            high_rate_samples: 0,
            probe: None,
            failed_probes: 0,
        }
    }

    /// 当前并发宽度(测试读取用;运行时变更经由 `sample` 的返回值传播)。
    #[cfg(test)]
    pub fn width(&self) -> usize {
        self.width
    }

    /// 探测上限 = min(配置上限, 已知安全上限)。
    fn limit(&self) -> usize {
        self.max.min(self.ceiling)
    }

    fn grow(&mut self) {
        let grown = (self.width as f64 * 1.25).ceil() as usize;
        self.width = grown.max(self.width + 1).min(self.limit());
    }

    fn shrink(&mut self) {
        let shrunk = (self.width as f64 * 0.75).floor() as usize;
        self.width = shrunk
            .max(self.min)
            .min(self.width.saturating_sub(1).max(self.min));
    }

    /// 送入一次速率采样(字节/秒),返回并发度变化后的新值。
    ///
    /// 零速率窗口不作为样本;停滞由页面侧超时与 Rust 看门狗处理。
    pub fn sample(&mut self, now: Instant, rate: f64) -> Option<usize> {
        if !(rate > 0.0) || !rate.is_finite() {
            return None;
        }

        if self.smoothed_rate == 0.0 {
            self.smoothed_rate = rate;
            self.baseline = rate;
            self.improved_at = now;
            self.last_change_at = now;
            return None;
        }
        self.smoothed_rate += RATE_EWMA_ALPHA * (rate - self.smoothed_rate);

        if now.duration_since(self.improved_at) >= IDLE_RESET {
            self.failed_probes = 0;
            self.improved_at = now;
        }

        let before = self.width;

        if self.baseline > 0.0 && self.smoothed_rate < self.baseline * DROP_RATIO {
            self.low_rate_samples = self.low_rate_samples.saturating_add(1);
        } else {
            self.low_rate_samples = 0;
        }

        if self.low_rate_samples >= DROP_CONFIRMATION_SAMPLES {
            // 连续低速窗口确认后才退避,避免一两个短样本造成并发锯齿。
            self.ceiling = self.width;
            self.shrink();
            self.baseline = self.smoothed_rate;
            self.probe = None;
            self.failed_probes = 0;
            self.low_rate_samples = 0;
            self.high_rate_samples = 0;
            self.improved_at = now;
            self.last_change_at = now;
            return (self.width != before).then_some(self.width);
        }

        if let Some((probe_started, probe_baseline)) = self.probe {
            if now.duration_since(probe_started) < PROBE_SETTLE {
                return None;
            }

            if self.smoothed_rate >= probe_baseline * IMPROVE_RATIO {
                // 探测成功:保留新宽度并将其记为已验证。
                self.ceiling = self.ceiling.max(self.width).min(self.max);
                self.baseline = self.smoothed_rate;
                self.failed_probes = 0;
                self.improved_at = now;
            } else {
                // 探测失败:回到探测前宽度,并阻止立即冲回同一宽度。
                self.ceiling = self.ceiling.min(self.width.saturating_sub(1).max(self.min));
                self.width = self.width.saturating_sub(1).max(self.min);
                self.failed_probes = self.failed_probes.saturating_add(1);
                self.baseline = self.smoothed_rate;
                self.improved_at = now;
            }
            self.probe = None;
            self.low_rate_samples = 0;
            self.high_rate_samples = 0;
            self.last_change_at = now;
            return (self.width != before).then_some(self.width);
        }

        if self.baseline > 0.0 && self.smoothed_rate >= self.baseline * IMPROVE_RATIO {
            self.high_rate_samples = self.high_rate_samples.saturating_add(1);
        } else {
            self.high_rate_samples = 0;
        }

        if self.high_rate_samples >= IMPROVE_CONFIRMATION_SAMPLES
            && now.duration_since(self.last_change_at) >= PROBE_INTERVAL
        {
            self.baseline = self.smoothed_rate;
            self.improved_at = now;
            self.high_rate_samples = 0;
            if self.width >= self.ceiling && self.ceiling < self.max {
                self.ceiling += 1;
            }
            self.grow();
            self.last_change_at = now;
        } else if now.duration_since(self.improved_at) >= PROBE_INTERVAL
            && self.failed_probes < MAX_FAILED_PROBES
            && self.width < self.max
        {
            // 平台期只探测一条流,并等待多个样本后再决定是否保留。
            let old_width = self.width;
            self.width = (self.width + 1).min(self.max);
            self.probe = Some((now, self.smoothed_rate));
            self.last_change_at = now;
            if self.width == old_width {
                self.probe = None;
            }
        }

        (self.width != before).then_some(self.width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(controller: &mut AdaptiveConcurrency, at: Instant, rate: f64) -> usize {
        controller.sample(at, rate);
        controller.width()
    }

    #[test]
    fn ignores_zero_and_non_finite_samples() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 16);
        let t0 = Instant::now();
        assert_eq!(controller.sample(t0, 0.0), None);
        assert_eq!(controller.sample(t0, f64::NAN), None);
        assert_eq!(controller.width(), 4);
    }

    #[test]
    fn a_single_low_window_does_not_back_off() {
        let mut controller = AdaptiveConcurrency::new(2, 8, 16);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        assert_eq!(
            tick(&mut controller, t0 + Duration::from_secs(2), 100_000.0),
            8
        );
        assert_eq!(
            tick(&mut controller, t0 + Duration::from_secs(4), 4_000_000.0),
            8
        );
    }

    #[test]
    fn small_rate_jitter_does_not_make_width_follow_each_sample() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 8);
        let t0 = Instant::now();
        tick(&mut controller, t0, 1_000_000.0);
        let mut changes = 0;
        for sample in 1..=60 {
            let at = t0 + Duration::from_secs(sample * 2);
            let rate = if sample % 2 == 0 {
                1_100_000.0
            } else {
                900_000.0
            };
            changes += usize::from(controller.sample(at, rate).is_some());
        }
        assert!(changes <= 8, "小幅抖动不应让并发每个窗口反复变化:{changes}");
    }

    #[test]
    fn sustained_low_rate_eventually_backs_off_multiplicatively() {
        let mut controller = AdaptiveConcurrency::new(2, 8, 16);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        let mut width = 8;
        for second in [2, 4, 6, 8] {
            width = tick(&mut controller, t0 + Duration::from_secs(second), 100_000.0);
        }
        assert_eq!(width, 6, "持续低速确认后,8 × 0.75 应退到 6");
    }

    #[test]
    fn ramps_up_after_sustained_improvement_and_respects_cap() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 5);
        let t0 = Instant::now();
        tick(&mut controller, t0, 1_000_000.0);
        let mut at = t0;
        let mut rate = 1_000_000.0;
        let mut previous_width = controller.width();
        for _ in 0..30 {
            at += Duration::from_secs(2);
            rate *= 1.10;
            let width = tick(&mut controller, at, rate);
            assert!(width >= previous_width, "增长样本不应降低并发");
            previous_width = width;
        }
        assert_eq!(controller.width(), 5);
    }

    #[test]
    fn plateau_probe_waits_before_withdrawing() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 8);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        assert_eq!(tick(&mut controller, t0 + PROBE_INTERVAL, 4_000_000.0), 5);
        // 探测后头几个窗口仍在观察期,不会立即回到原并发。
        assert_eq!(
            tick(
                &mut controller,
                t0 + PROBE_INTERVAL + Duration::from_secs(2),
                4_000_000.0
            ),
            5
        );
        assert_eq!(
            tick(
                &mut controller,
                t0 + PROBE_INTERVAL + Duration::from_secs(4),
                4_000_000.0
            ),
            5
        );
        assert_eq!(
            tick(
                &mut controller,
                t0 + PROBE_INTERVAL + PROBE_SETTLE,
                4_000_000.0
            ),
            4
        );
    }

    #[test]
    fn a_successful_probe_is_kept_and_becomes_the_new_safe_width() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 8);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        assert_eq!(tick(&mut controller, t0 + PROBE_INTERVAL, 4_000_000.0), 5);
        assert_eq!(
            tick(
                &mut controller,
                t0 + PROBE_INTERVAL + Duration::from_secs(2),
                5_000_000.0
            ),
            5
        );
        assert_eq!(
            tick(
                &mut controller,
                t0 + PROBE_INTERVAL + Duration::from_secs(4),
                5_000_000.0
            ),
            5
        );
        assert_eq!(
            tick(
                &mut controller,
                t0 + PROBE_INTERVAL + PROBE_SETTLE,
                5_000_000.0
            ),
            5
        );
        assert_eq!(controller.width(), 5);
        assert!(controller.ceiling >= controller.width());
    }

    #[test]
    fn failed_probes_are_limited_until_idle_reset() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 8);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        let probe_at = t0 + PROBE_INTERVAL;
        assert_eq!(tick(&mut controller, probe_at, 4_000_000.0), 5);
        assert_eq!(
            tick(&mut controller, probe_at + PROBE_SETTLE, 4_000_000.0),
            4
        );

        let second_probe_at = probe_at + PROBE_SETTLE + PROBE_INTERVAL;
        assert_eq!(tick(&mut controller, second_probe_at, 4_000_000.0), 5);
        assert_eq!(
            tick(&mut controller, second_probe_at + PROBE_SETTLE, 4_000_000.0),
            4
        );

        let blocked_probe_at = second_probe_at + PROBE_SETTLE + PROBE_INTERVAL;
        assert_eq!(tick(&mut controller, blocked_probe_at, 4_000_000.0), 4);
        let reset_at = blocked_probe_at + IDLE_RESET;
        assert_eq!(tick(&mut controller, reset_at, 4_000_000.0), 4);
        assert_eq!(
            tick(&mut controller, reset_at + PROBE_INTERVAL, 4_000_000.0),
            5
        );
    }

    #[test]
    fn recovers_above_a_previous_safe_ceiling_after_gain() {
        let mut controller = AdaptiveConcurrency::new(2, 8, 16);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        let mut width = 8;
        for second in [2, 4, 6, 8] {
            width = tick(&mut controller, t0 + Duration::from_secs(second), 100_000.0);
        }
        assert_eq!(width, 6);
        // 持续恢复后可先探测并确认更高并发,不会被旧 ceiling 永久卡住。
        for step in 1..=20 {
            let at = t0 + Duration::from_secs(8 + step * 2);
            width = tick(&mut controller, at, 4_000_000.0 + step as f64 * 300_000.0);
        }
        assert!(width > 6, "速率恢复后应允许重新探测:width={width}");
    }
}
