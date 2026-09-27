// desktop/src-tauri/src/adaptive.rs
//! 自适应分块并发控制器:把"每文件并发分块数"当作拥塞窗口来调。
//!
//! 依据:
//! - BBR(Cardwell 等,ACM Queue 2016;IETF draft-ietf-ccwg-bbr)——
//!   用"窗口内最大投递率"建模可用带宽,ProbeBW 以 1.25× 乘性探测、
//!   0.75× 乘性退避;不把丢包当拥塞信号,而是看速率本身。
//! - Netflix concurrency-limits(Vegas/Gradient2,"Performance Under Load",
//!   Netflix Tech Blog 2018)——把并发度当作 TCP 拥塞窗口来自适应。
//! - ceiling 稳定化(NVIDIA NeMo Data Designer 工程笔记,AIMD 改进)——
//!   退避后记录"已知安全上限",探测不再冲回配置上限,消除锯齿。
//!
//! 与 TCP 的差异:并发度是整数、页面侧线程池是软调整(缩小不打断在途分块),
//! 采样是 2s 聚合值而非逐包事件——天然抑制突发抖动,不存在级联退避问题。

use std::time::{Duration, Instant};

/// BBR ProbeBW 探测增益(乘性增长步长)。
const GROWTH: f64 = 1.25;
/// BBR ProbeBW 排空增益(乘性退避步长)。
const BACKOFF: f64 = 0.75;
/// 速率提升超过该比例才认为"增长有收益"。
const IMPROVE_RATIO: f64 = 1.03;
/// 速率回落到该比例以下即触发乘性退避。
const DROP_RATIO: f64 = 0.80;
/// 平台期两次探测之间的最短间隔。
const PROBE_INTERVAL: Duration = Duration::from_secs(6);
/// 长时间无增益后清空失败计数(链路容量可能已变化,允许重新探测)。
const IDLE_RESET: Duration = Duration::from_secs(20);
/// 连续探测无收益的次数上限。
const MAX_FAILED_PROBES: u32 = 2;

#[derive(Debug, Clone)]
pub struct AdaptiveConcurrency {
    width: usize,
    min: usize,
    max: usize,
    /// 已知安全上限:退避时记录为当时的并发;探测不会越过它,除非探到增益。
    ceiling: usize,
    /// 上一次并发变更前的速率采样("变更前基线")。
    baseline: f64,
    /// 上一次速率采样(用于回落检测)。
    last_rate: f64,
    /// 上一次获得增益的时间。
    improved_at: Instant,
    /// 最近一次采样是不是"探测 +1",用于无收益时撤回。
    probing: bool,
    failed_probes: u32,
}

impl AdaptiveConcurrency {
    pub fn new(min: usize, initial: usize, max: usize) -> Self {
        let max = max.max(1);
        let min = min.clamp(1, max);
        let width = initial.clamp(min, max);
        Self {
            width,
            min,
            max,
            ceiling: max,
            baseline: 0.0,
            last_rate: 0.0,
            improved_at: Instant::now(),
            probing: false,
            failed_probes: 0,
        }
    }

    /// 当前并发宽度(测试读取用;运行时变更经由 `sample` 的返回值传播)。
    #[cfg(test)]
    pub fn width(&self) -> usize {
        self.width
    }

    /// 探测上限 = min(配置上限, ceiling)。
    fn limit(&self) -> usize {
        self.max.min(self.ceiling)
    }

    fn grow(&mut self) {
        let grown = (self.width as f64 * GROWTH).ceil() as usize;
        let next = grown.max(self.width + 1).min(self.limit());
        if next > self.width {
            self.width = next;
        }
    }

    fn shrink(&mut self) {
        let shrunk = (self.width as f64 * BACKOFF).floor() as usize;
        let next = shrunk
            .max(self.min)
            .min(self.width.saturating_sub(1))
            .max(self.min);
        self.width = next;
    }

    /// 撤回一次失败的平台期探测:+1 的对称操作,退回探测前宽度。
    fn withdraw(&mut self) {
        self.width = self.width.saturating_sub(1).max(self.min);
    }

    /// 送入一次速率采样(字节/秒),返回并发度变化后的新值。
    ///
    /// 零速率窗口(整窗无进展)不作为样本:它多半是网络/媒体侧的问题,
    /// 调并发帮不上忙(页面的停滞自检与 Rust 看门狗负责这类情形)。
    pub fn sample(&mut self, now: Instant, rate: f64) -> Option<usize> {
        if !(rate > 0.0) {
            return None;
        }
        if now.duration_since(self.improved_at) >= IDLE_RESET {
            self.failed_probes = 0;
            self.improved_at = now;
        }
        let before = self.width;

        if self.baseline > 0.0 && rate < self.last_rate * DROP_RATIO {
            // 明显回落 → 乘性退避;ceiling 记录退避前宽度,之后要越过它必须有增益证明。
            self.ceiling = self.width;
            self.shrink();
            self.baseline = rate;
            self.probing = false;
            self.failed_probes = 0;
            self.improved_at = now;
        } else if self.baseline > 0.0 && rate >= self.baseline * IMPROVE_RATIO {
            // 增长有收益 → 当前宽度被证明安全;若已顶到 ceiling,允许 +1 继续验证。
            self.baseline = rate;
            self.probing = false;
            self.failed_probes = 0;
            self.improved_at = now;
            if self.width >= self.ceiling && self.ceiling < self.max {
                self.ceiling += 1;
            }
            self.grow();
        } else if self.probing {
            // 平台期探测没有换来增益 → 撤回探测,记一次失败。
            self.probing = false;
            self.failed_probes = self.failed_probes.saturating_add(1);
            self.withdraw();
        } else if now.duration_since(self.improved_at) >= PROBE_INTERVAL
            && self.failed_probes < MAX_FAILED_PROBES
            && self.width < self.max
        {
            // 平台期低频探测(BBR ProbeBW:不时试着要更多带宽)。
            // 注意上限用 `max` 而不是 ceiling:ceiling 是"退避后不再快速冲回"的
            // 护栏,不是禁止恢复的死墙 —— 否则在低并发上退避一次就永远卡死。
            self.baseline = rate;
            self.probing = true;
            self.width = (self.width + 1).min(self.max);
        }

        if self.baseline == 0.0 {
            // 首个采样:建立基线即可,不做变更。
            self.baseline = rate;
        }
        self.last_rate = rate;
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
    fn ramps_up_while_rate_keeps_improving() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 16);
        let t0 = Instant::now();
        let mut at = t0;
        let mut rate = 1_000_000.0;
        // 基线采样
        tick(&mut controller, at, rate);
        let mut widths = vec![controller.width()];
        for _ in 0..12 {
            at += Duration::from_secs(2);
            rate *= 1.10;
            widths.push(tick(&mut controller, at, rate));
        }
        assert!(
            widths.windows(2).all(|w| w[1] >= w[0]),
            "递增序列:{widths:?}"
        );
        assert_eq!(controller.width(), 16, "链路足够快时应探到配置上限");
    }

    #[test]
    fn backs_off_multiplicatively_on_rate_drop() {
        let mut controller = AdaptiveConcurrency::new(2, 8, 16);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        let width = tick(&mut controller, t0 + Duration::from_secs(2), 2_000_000.0);
        assert_eq!(width, 6, "8 × 0.75 = 6");
    }

    #[test]
    fn never_drops_below_floor() {
        let mut controller = AdaptiveConcurrency::new(2, 2, 16);
        let t0 = Instant::now();
        tick(&mut controller, t0, 4_000_000.0);
        let width = tick(&mut controller, t0 + Duration::from_secs(2), 1_000_000.0);
        assert_eq!(width, 2);
    }

    #[test]
    fn probes_climb_back_after_a_backoff_even_under_the_ceiling() {
        let mut controller = AdaptiveConcurrency::new(2, 8, 16);
        let t0 = Instant::now();
        let mut at = t0;
        tick(&mut controller, at, 4_000_000.0);
        // 速率回落 → 退避,ceiling 被压到退避前的宽度
        at += Duration::from_secs(2);
        let floor = tick(&mut controller, at, 1_000_000.0);
        // 之后速率平稳:探针要能一步步爬回去,而不是被 ceiling 永久卡死
        let mut max_seen = floor;
        for _ in 0..12 {
            at += PROBE_INTERVAL;
            max_seen = max_seen.max(tick(&mut controller, at, 1_000_000.0));
        }
        assert!(
            max_seen > floor,
            "退避后必须能靠探测恢复:floor={floor} max={max_seen}"
        );
    }

    #[test]
    fn failed_probe_is_withdrawn_and_retried_at_most_twice() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 8);
        let t0 = Instant::now();
        let mut at = t0;
        // 平台期速率(先建立基线,再给足探测间隔)
        tick(&mut controller, at, 4_000_000.0);
        at += PROBE_INTERVAL;
        let probed = tick(&mut controller, at, 4_000_000.0);
        assert_eq!(probed, 5, "平台期应探测 +1");
        // 探测无收益 → 撤回
        at += Duration::from_secs(2);
        let withdrawn = tick(&mut controller, at, 4_000_000.0);
        assert_eq!(withdrawn, 4, "无收益的探测应撤回");
        // 第二次探测同样失败
        at += PROBE_INTERVAL;
        let probed = tick(&mut controller, at, 4_000_000.0);
        assert_eq!(probed, 5);
        at += Duration::from_secs(2);
        let withdrawn = tick(&mut controller, at, 4_000_000.0);
        assert_eq!(withdrawn, 4);
        // 两次失败后暂停探测(下一个采样点已越过 IDLE_RESET,重置探测窗口)
        at += PROBE_INTERVAL;
        assert_eq!(
            tick(&mut controller, at, 4_000_000.0),
            4,
            "两次失败后不能马上再探测"
        );
        // 空闲窗口过后允许重新探测(链路容量可能已变化)
        at += PROBE_INTERVAL;
        assert_eq!(
            tick(&mut controller, at, 4_000_000.0),
            5,
            "空闲窗口后重新探测"
        );
    }

    #[test]
    fn ceiling_caps_growth_after_drop_until_gain_proves_otherwise() {
        let mut controller = AdaptiveConcurrency::new(2, 12, 16);
        let t0 = Instant::now();
        let mut at = t0;
        tick(&mut controller, at, 8_000_000.0); // 建立基线
        // 12 路时速率回落 → 退避到 9,ceiling=12
        at += Duration::from_secs(2);
        assert_eq!(tick(&mut controller, at, 5_000_000.0), 9);
        // 增益证明:速率回升 → 回到 ceiling(12)
        at += Duration::from_secs(2);
        assert_eq!(tick(&mut controller, at, 6_000_000.0), 12);
        // 平台期(无增益)时 ceiling 挡住探测,不再增长
        for _ in 0..4 {
            at += PROBE_INTERVAL;
            tick(&mut controller, at, 6_000_000.0);
        }
        assert_eq!(controller.width(), 12, "无增益时 ceiling 阻止继续增长");
        // 在 ceiling 上重新出现增益 → ceiling 抬升,允许继续增长
        at += Duration::from_secs(2);
        tick(&mut controller, at, 7_000_000.0);
        assert!(controller.width() > 12, "增益证明后应越过原 ceiling");
    }

    #[test]
    fn respects_configured_cap() {
        let mut controller = AdaptiveConcurrency::new(2, 4, 5);
        let t0 = Instant::now();
        let mut at = t0;
        let mut rate = 1_000_000.0;
        tick(&mut controller, at, rate);
        for _ in 0..10 {
            at += Duration::from_secs(2);
            rate *= 1.5;
            tick(&mut controller, at, rate);
        }
        assert_eq!(controller.width(), 5);
    }
}
