//! CPU 总占用。
//!
//! 任务管理器（Win10+）显示的不是「繁忙时间占比」，而是 `% Processor Utility`：
//!   · % Processor Time    = (kernel+user-idle)/(kernel+user)，GetSystemTimes 的口径
//!   · % Processor Utility = 实际完成的工作 / 标称性能下满负荷能完成的工作
//! CPU 睿频到标称频率之上时，Utility 会明显高于 Processor Time（本机实测：
//! 同样的负载下 Processor Time 约 60%，Utility 约 96%）。这就是「任务管理器都
//! 100% 了，程序只显示 80%」的原因。
//!
//! 这里优先读 Utility（和任务管理器一致），读不到（旧系统/计数器被禁用）才回退
//! 到 GetSystemTimes 的差分算法。

use windows::core::w;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
};
use windows::Win32::System::Threading::GetSystemTimes;

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// 返回 (idle, kernel, user)。注意 kernel 已包含 idle。
fn read() -> Option<(u64, u64, u64)> {
    let (mut i, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut i), Some(&mut k), Some(&mut u)).ok()? };
    Some((ft(i), ft(k), ft(u)))
}

/// 打开 `\Processor Information(_Total)\% Processor Utility`；失败返回 (0, 0)
unsafe fn open_utility_query() -> (isize, isize) {
    let mut query = 0isize;
    if PdhOpenQueryW(None, 0, &mut query) != 0 {
        return (0, 0);
    }
    let mut counter = 0isize;
    let r = PdhAddEnglishCounterW(
        query,
        w!("\\Processor Information(_Total)\\% Processor Utility"),
        0,
        &mut counter,
    );
    if r != 0 {
        PdhCloseQuery(query);
        return (0, 0);
    }
    // 先采一次基准值，下次采样才有有效数据
    PdhCollectQueryData(query);
    (query, counter)
}

pub struct CpuMonitor {
    /// Utility 计数器；0 = 不可用，走 GetSystemTimes 回退
    query: isize,
    counter: isize,
    prev: (u64, u64, u64),
}

impl CpuMonitor {
    pub fn new() -> Self {
        let (query, counter) = unsafe { open_utility_query() };
        Self { query, counter, prev: read().unwrap_or_default() }
    }

    /// 距离上次调用期间的 CPU 占用百分比 (0~100)
    pub fn sample(&mut self) -> f64 {
        if self.query != 0 {
            unsafe {
                if PdhCollectQueryData(self.query) == 0 {
                    let mut v = PDH_FMT_COUNTERVALUE::default();
                    if PdhGetFormattedCounterValue(self.counter, PDH_FMT_DOUBLE, None, &mut v) == 0
                        && v.CStatus == 0
                    {
                        // Utility 在睿频下可能超过 100，显示时截断到 100
                        return v.Anonymous.doubleValue.clamp(0.0, 100.0);
                    }
                }
            }
        }
        self.sample_times()
    }

    /// GetSystemTimes 差分：几乎零开销，作为 Utility 不可用时的回退
    fn sample_times(&mut self) -> f64 {
        let Some(cur) = read() else { return 0.0 };
        let di = cur.0.wrapping_sub(self.prev.0);
        let dk = cur.1.wrapping_sub(self.prev.1);
        let du = cur.2.wrapping_sub(self.prev.2);
        self.prev = cur;
        let total = dk + du;
        if total == 0 {
            return 0.0;
        }
        ((total.saturating_sub(di)) as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

impl Drop for CpuMonitor {
    fn drop(&mut self) {
        if self.query != 0 {
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}
